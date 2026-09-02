//! This benchmark measures the cost of one cached `GlossDoc` with an allocation counter.
//!
//! `GlossDoc` uses a flat arena instead of a box tree.
//! Parse time alone does not select between them because the parsed tree stays in the cache.
//! Allocation count and retained heap also affect the choice.
//!
//! `examples/gloss_arena_bench.rs` compared three candidates with the same real payloads.
//! It set these limits:
//!
//! | per hover set of cached entries | live blocks | retained |
//! |---|---:|---:|
//! | box tree (`Vec<Node>`, `String`, `HashMap`) | 1 817 | 384 KB |
//! | arena prototype | 186 | 84 KB |
//!
//! This benchmark applies the same measurement to the shipped
//! `chibipop::dict::gloss::GlossDoc`.
//! The result measures the actual type, not a similar prototype.
//!
//! ```sh
//! cargo run -p chibipop --release --example gloss_doc_alloc          # every headword
//! cargo run -p chibipop --release --example gloss_doc_alloc -- 40    # first 40
//! ```
//!
//! Release mode is required. A debug-profile `serde` result differs by one order of magnitude.
//!
//! A hover set contains the first `MAX_RESULTS` payloads of a headword.
//! `LookupEngine::run` leaves this set after `ranked.truncate(MAX_RESULTS)`.
//!
//! The global allocator wraps `std::alloc::System`.
//! A relaxed `AtomicBool` controls the counters.
//! The load compiles to a plain `mov` instruction with no locked instruction.
//! The timed free pass disables the counters.
//! The instrumentation does not affect that pass.

use anyhow::{Context, Result};
use chibipop::dict::gloss::{plain_items, GlossDoc};
use chibipop::lookup::engine::MAX_RESULTS;
use serde::Deserialize;
use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::Relaxed};
use std::time::Instant;

// ---------------------------------------------------------------------------
// Allocation counter from `gloss_arena_bench.rs`.
// The shared implementation makes the two results comparable.
// ---------------------------------------------------------------------------

static COUNT_ON: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static FREES: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);
static FREED_BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicI64 = AtomicI64::new(0);
static PEAK: AtomicI64 = AtomicI64::new(0);

/// `Counting` adds counters to `System`. One relaxed load controls the counters.
///
/// `realloc` calls `System::realloc` instead of the trait default.
/// The default allocates, copies, and deallocates.
/// It copies each `Vec` during growth, even when the allocator can extend the block in place.
/// This behavior penalizes the arena because it grows a few large vectors.
struct Counting;

impl Counting {
    #[inline]
    fn on_alloc(size: usize) {
        ALLOCS.fetch_add(1, Relaxed);
        ALLOC_BYTES.fetch_add(size as u64, Relaxed);
        let live = LIVE.fetch_add(size as i64, Relaxed) + size as i64;
        PEAK.fetch_max(live, Relaxed);
    }

    #[inline]
    fn on_free(size: usize) {
        FREES.fetch_add(1, Relaxed);
        FREED_BYTES.fetch_add(size as u64, Relaxed);
        LIVE.fetch_sub(size as i64, Relaxed);
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if !p.is_null() && COUNT_ON.load(Relaxed) {
            Self::on_alloc(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc_zeroed(layout);
        if !p.is_null() && COUNT_ON.load(Relaxed) {
            Self::on_alloc(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if COUNT_ON.load(Relaxed) {
            Self::on_free(layout.size());
        }
        System.dealloc(ptr, layout);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = System.realloc(ptr, layout, new_size);
        if !p.is_null() && COUNT_ON.load(Relaxed) {
            Self::on_free(layout.size());
            Self::on_alloc(new_size);
        }
        p
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[derive(Clone, Copy, Default)]
struct AllocStat {
    allocs: u64,
    frees: u64,
    alloc_bytes: u64,
    freed_bytes: u64,
    live: i64,
    peak: i64,
}

fn alloc_reset() {
    ALLOCS.store(0, Relaxed);
    FREES.store(0, Relaxed);
    ALLOC_BYTES.store(0, Relaxed);
    FREED_BYTES.store(0, Relaxed);
    LIVE.store(0, Relaxed);
    PEAK.store(0, Relaxed);
}

fn alloc_snapshot() -> AllocStat {
    AllocStat {
        allocs: ALLOCS.load(Relaxed),
        frees: FREES.load(Relaxed),
        alloc_bytes: ALLOC_BYTES.load(Relaxed),
        freed_bytes: FREED_BYTES.load(Relaxed),
        live: LIVE.load(Relaxed),
        peak: PEAK.load(Relaxed),
    }
}

// ---------------------------------------------------------------------------
// fixture
// ---------------------------------------------------------------------------

/// `Record` stores the fields that the measurement reads from one line of
/// `hover-payloads.jsonl`. Serde ignores fields that it does not need.
#[derive(Deserialize)]
struct Record {
    term: String,
    worst: bool,
    payloads: Vec<String>,
}

fn results_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/hover-parse-bench/results")
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

// ---------------------------------------------------------------------------
// statistics
// ---------------------------------------------------------------------------

/// `percentile` uses the nearest-rank percentile from `tools/hover-parse-bench`.
fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((q * (sorted.len() - 1) as f64).round() as usize).min(sorted.len() - 1);
    sorted[i]
}

struct Dist {
    p50: f64,
    p95: f64,
    max: f64,
    total: f64,
}

fn dist(samples: &[f64]) -> Dist {
    let mut v = samples.to_vec();
    v.sort_by(f64::total_cmp);
    Dist {
        p50: percentile(&v, 0.50),
        p95: percentile(&v, 0.95),
        max: v.last().copied().unwrap_or(0.0),
        total: v.iter().sum(),
    }
}

// ---------------------------------------------------------------------------
// driver
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Series {
    /// This field stores the number of allocations for one hover set.
    allocs: Vec<f64>,
    /// This field stores the bytes requested for one hover set.
    alloc_bytes: Vec<f64>,
    /// This field stores the bytes that built documents retain in a cache entry.
    retained: Vec<f64>,
    /// This field stores the peak live bytes during the build.
    peak: Vec<f64>,
    /// This field stores allocations that built documents still own.
    /// Its value is `allocs - frees`.
    /// An `Arc` cache entry retains these allocations and frees them later.
    blocks: Vec<f64>,
    /// This field stores the time in microseconds to drop one hover set.
    free: Vec<f64>,
    /// This field stores the document footprint for comparison with allocator results.
    footprint: Vec<f64>,
    nodes: Vec<f64>,
    entries: Vec<f64>,
}

fn main() -> Result<()> {
    let limit: usize = std::env::args()
        .nth(1)
        .map(|a| a.parse().context("usage: gloss_doc_alloc [headword-limit]"))
        .transpose()?
        .unwrap_or(usize::MAX);

    let records = load_records()?;
    if records.is_empty() {
        anyhow::bail!("hover-payloads.jsonl is empty");
    }

    // Each headword supplies one hover set.
    // Percentiles exclude worst-case headwords.
    // The retained file contains too many such headwords for a frequency sample.
    let slices: Vec<(&str, &[String])> = records
        .iter()
        .filter(|r| !r.worst)
        .take(limit)
        .map(|r| (r.term.as_str(), &r.payloads[..r.payloads.len().min(MAX_RESULTS)]))
        .collect();
    if slices.is_empty() {
        anyhow::bail!("no frequency-sample headwords in hover-payloads.jsonl");
    }

    println!(
        "gloss_doc_alloc: {} headwords, first {MAX_RESULTS} payloads each, {:.1} MB of \
         glossary json",
        slices.len(),
        slices.iter().flat_map(|(_, p)| p.iter()).map(String::len).sum::<usize>() as f64 / 1e6,
    );

    let mut s = Series::default();
    let mut residual = 0i64;
    let mut rendered = 0usize;

    for (_, payloads) in &slices {
        // The counted pass includes the drop.
        // The residual check confirms that the documents free their allocations.
        // A timed drop here includes the instrumentation cost.
        // The separate pass measures the drop with counters disabled.
        COUNT_ON.store(true, Relaxed);
        alloc_reset();
        let built: Vec<GlossDoc> = payloads.iter().map(|p| GlossDoc::parse(p)).collect();
        let stat = alloc_snapshot();
        // Read every stored field while the counters are active.
        // The optimizer must keep the field writes because this pass reads every field.
        // It also keeps the documents alive at the snapshot above.
        let mut nodes = 0usize;
        let mut footprint = 0usize;
        for doc in &built {
            nodes += doc.all_nodes().len();
            footprint += doc.footprint();
            rendered += plain_items(doc).iter().map(String::len).sum::<usize>();
        }
        drop(built);
        let after = alloc_snapshot();
        COUNT_ON.store(false, Relaxed);
        residual = residual.max(after.live);

        // The timed pass disables counters.
        // It builds the documents without instrumentation and then measures the drop.
        let built: Vec<GlossDoc> = payloads.iter().map(|p| GlossDoc::parse(p)).collect();
        black_box(&built);
        let started = Instant::now();
        drop(built);
        let free_us = started.elapsed().as_secs_f64() * 1e6;

        s.allocs.push(stat.allocs as f64);
        s.alloc_bytes.push(stat.alloc_bytes as f64);
        s.retained.push(stat.live as f64);
        s.peak.push(stat.peak as f64);
        s.blocks.push((stat.allocs - stat.frees) as f64);
        s.free.push(free_us);
        s.footprint.push(footprint as f64);
        s.nodes.push(nodes as f64);
        s.entries.push(payloads.len() as f64);

        assert_eq!(
            stat.alloc_bytes - stat.freed_bytes,
            stat.live as u64,
            "allocator accounting disagrees with itself"
        );
    }
    black_box(rendered);

    let allocs = dist(&s.allocs);
    let alloc_bytes = dist(&s.alloc_bytes);
    let retained = dist(&s.retained);
    let peak = dist(&s.peak);
    let blocks = dist(&s.blocks);
    let free = dist(&s.free);
    let footprint = dist(&s.footprint);
    let nodes = dist(&s.nodes);
    let entries = dist(&s.entries);

    println!("\nper hover-worth of cached entries (counting global allocator)");
    println!("{:<34}  {:>12}  {:>12}  {:>12}", "measure", "p50", "p95", "max");
    println!("{}", "-".repeat(34 + 3 * 14));
    let row = |name: &str, d: &Dist| {
        println!("{:<34}  {:>12.0}  {:>12.0}  {:>12.0}", name, d.p50, d.p95, d.max);
    };
    row("allocations made", &allocs);
    row("bytes requested", &alloc_bytes);
    row("live blocks retained", &blocks);
    row("bytes retained", &retained);
    row("peak live bytes during parse", &peak);
    row("self-reported footprint, bytes", &footprint);
    row("nodes", &nodes);
    row("entries", &entries);
    println!(
        "{:<34}  {:>12.2}  {:>12.2}  {:>12.2}",
        "free, us", free.p50, free.p95, free.max
    );

    let per_entry_blocks = blocks.total / entries.total;
    let per_entry_bytes = retained.total / entries.total;
    println!(
        "\nper cached entry, over {:.0} entries: {:.1} live blocks, {:.0} B retained \
         ({:.1} B/node over {:.0} nodes)",
        entries.total,
        per_entry_blocks,
        per_entry_bytes,
        retained.total / nodes.total,
        nodes.total,
    );
    println!(
        "against the box-tree baseline of 1817 live blocks and 384 KB per hover-worth: \
         {:.1}x fewer blocks, {:.1}x less retained heap",
        1817.0 / blocks.p50,
        384.0 * 1024.0 / retained.p50,
    );
    println!(
        "residual live bytes after every document was dropped: {residual} (0 = no leak)"
    );
    Ok(())
}
