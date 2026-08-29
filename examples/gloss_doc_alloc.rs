//! What one cached `GlossDoc` costs, measured with a counting allocator.
//!
//! Ticket 02 chose a flat arena over a box tree on two axes that latency
//! cannot decide: allocation count and retained heap, because the parsed tree
//! is *cached*. `examples/gloss_arena_bench.rs` prototyped all three
//! candidates and set the bar this harness has to clear, over the same real
//! payloads:
//!
//! | per hover-worth of cached entries | live blocks | retained |
//! |---|---:|---:|
//! | box tree (`Vec<Node>`, `String`, `HashMap`) | 1 817 | 384 KB |
//! | arena prototype | 186 | 84 KB |
//!
//! This measures the shipped `chibipop::dict::gloss::GlossDoc` the same way,
//! so the claim in the ticket is a measurement of the real type rather than of
//! a prototype that resembles it.
//!
//! ```sh
//! cargo run -p chibipop --release --example gloss_doc_alloc          # every headword
//! cargo run -p chibipop --release --example gloss_doc_alloc -- 40    # first 40
//! ```
//!
//! Release mode is mandatory: a debug-profile `serde` number is off by an
//! order of magnitude.
//!
//! A hover-worth is the first `MAX_RESULTS` payloads of a headword, which is
//! what `LookupEngine::run` leaves after `ranked.truncate(MAX_RESULTS)`.
//!
//! The allocator is a real counting global allocator wrapping
//! `std::alloc::System`, gated on a relaxed `AtomicBool` - a plain `mov`, no
//! locked instruction - so the timed free pass runs with the counters off and
//! is not distorted by the instrumentation.

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
// counting global allocator - the one in gloss_arena_bench.rs, unchanged, so
// the two harnesses' numbers are comparable by construction
// ---------------------------------------------------------------------------

static COUNT_ON: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static FREES: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);
static FREED_BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicI64 = AtomicI64::new(0);
static PEAK: AtomicI64 = AtomicI64::new(0);

/// `System`, plus counters behind one relaxed load.
///
/// `realloc` delegates to `System::realloc` rather than falling back to the
/// trait's alloc-copy-dealloc default: without that, every `Vec` growth would
/// become a full copy even when the allocator could have extended in place,
/// which would penalise the arena - which grows a handful of big vectors - for
/// no reason of its own.
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

/// One line of `hover-payloads.jsonl`. Unknown fields are ignored, so this
/// declares only what the measurement reads.
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

/// Nearest-rank percentile, matching `tools/hover-parse-bench`.
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
    /// Allocations made while building one hover-worth.
    allocs: Vec<f64>,
    /// Bytes requested while building one hover-worth.
    alloc_bytes: Vec<f64>,
    /// Bytes the built documents still hold: what a cache entry retains.
    retained: Vec<f64>,
    /// Peak live bytes during the build.
    peak: Vec<f64>,
    /// Allocations the built documents still hold: `allocs - frees`. What an
    /// `Arc`-shared cache entry keeps alive, and frees later.
    blocks: Vec<f64>,
    /// Microseconds to drop one hover-worth.
    free: Vec<f64>,
    /// The documents' own footprint accounting, as a cross-check on the
    /// allocator's.
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

    // A hover-worth per headword, worst-case headwords kept out of the
    // percentiles for the same reason the other harnesses keep them out:
    // they are deliberately over-represented in the retained file.
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
        // Counted pass. The drop stays inside the counted window, so the
        // residual check below actually proves the documents freed what they
        // took; timing it here would measure the instrumentation instead, so
        // the free is timed separately below.
        COUNT_ON.store(true, Relaxed);
        alloc_reset();
        let built: Vec<GlossDoc> = payloads.iter().map(|p| GlossDoc::parse(p)).collect();
        let stat = alloc_snapshot();
        // Read every stored field inside the counted window, so nothing here
        // is a field the optimizer may quietly decline to write, and the
        // documents are provably still alive at the snapshot above.
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

        // Timed pass, counters off: one uninstrumented build, then the drop
        // this harness is reporting.
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
