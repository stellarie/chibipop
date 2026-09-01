//! Times one dictionary build, end to end.
//!
//! The measuring stick behind `dict::build`'s parallel bank pipeline, kept
//! because a performance claim nobody can re-run is a performance claim that
//! rots. It builds into a throwaway file and prints every progress line the
//! settings window would show, each stamped with the time since the line
//! before it, so a regression names the phase it landed in.
//!
//! ```text
//! cargo run --release --example dictbench -- <term.zip>… [-- <freq.zip>…]
//! BENCH_OUT=/path/to.sqlite   where to build (default: the temp dir)
//! BENCH_VERBOSE=1             keep the per-5000-entry progress lines
//! ```
//!
//! `BENCH_OUT` matters: a temp dir is often a tmpfs, and a build that never
//! touches a disk is not the build a user waits for.

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
