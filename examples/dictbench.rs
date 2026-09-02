//! This benchmark measures one Dictionary build from start to finish.
//!
//! `dict::build` runs the bank pipeline in parallel. This benchmark measures that pipeline.
//! The fixed progress output reports each phase in the same form.
//!
//! The benchmark writes the Dictionary to a temporary file.
//! It prints each progress line that the settings window shows.
//! Each line includes the time since the previous line.
//! The report identifies the phase that takes more time.
//!
//! ```text
//! cargo run --release --example dictbench -- <term.zip>… [-- <freq.zip>…]
//! BENCH_OUT=/path/to.sqlite   where to build (default: the temp dir)
//! BENCH_VERBOSE=1             keep the per-5000-entry progress lines
//! ```
//!
//! Set `BENCH_OUT` to a disk path to measure disk work.
//! A temporary directory often uses `tmpfs`.
//! A build on `tmpfs` excludes the disk work that a user waits for.

use std::cell::Cell;
use std::path::PathBuf;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let mut terms: Vec<PathBuf> = Vec::new();
    let mut freqs: Vec<PathBuf> = Vec::new();
    let mut after_marker = false;
    for arg in std::env::args().skip(1) {
        if arg == "--" {
            after_marker = true;
        } else if after_marker {
            freqs.push(PathBuf::from(arg));
        } else {
            terms.push(PathBuf::from(arg));
        }
    }

    let out = std::env::var_os("BENCH_OUT")
        .map_or_else(|| std::env::temp_dir().join("dictbench.sqlite"), PathBuf::from);
    let _ = std::fs::remove_file(&out);
    let verbose = std::env::var_os("BENCH_VERBOSE").is_some();

    let start = Instant::now();
    let last = Cell::new(start);
    let counts = chibipop::dict::build::build(&terms, &freqs, &out, &|line| {
        if !verbose && line.starts_with("progress") {
            return;
        }
        let now = Instant::now();
        println!(
            "[{:8.3}s +{:6.3}] {line}",
            start.elapsed().as_secs_f64(),
            now.duration_since(last.get()).as_secs_f64(),
        );
        last.set(now);
    })?;

    let total = start.elapsed().as_secs_f64();
    let size = std::fs::metadata(&out).map_or(0, |m| m.len());
    println!(
        "TOTAL {total:.3}s  {:.0} entries/s  entries={} terms={} db={:.1} MiB",
        counts.entries as f64 / total,
        counts.entries,
        counts.terms,
        size as f64 / (1024.0 * 1024.0),
    );
    Ok(())
}
