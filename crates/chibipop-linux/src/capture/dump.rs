//! `chibipop capture-dump`: the own eyes of the capture backends.
//!
//! This command is a diagnostic, like `probe`, and not a part of the
//! product. It opens the real backend that the capture ladder picks
//! for this session. It grabs real boxes, writes PNGs, and prints the
//! decision of each grab. Therefore you can *watch* a dwell on a
//! static screen answer without a copy. You can also watch damage
//! arrive. Without this command the only proof of a capture backend
//! is a hover, which is a slow and indirect way to see one pixel.
//!
//! This command can drive both rungs. The choice of rung obeys the
//! same `CHIBIPOP_CAPTURE_BACKEND` hook that the daemon reads.
//! Therefore `CHIBIPOP_CAPTURE_BACKEND=portal chibipop capture-dump`
//! exercises the portal rung even on a compositor that always takes
//! the promptless path. The portal rung shows its consent dialog
//! exactly as the daemon shows it. The portal rung writes the same
//! restore token that the daemon reads. That token makes "second
//! launch is silent" observable from a shell.
//!
//! This command takes the lock-free path on purpose: no instance lock
//! and no control socket. It is safe to run beside a live daemon.
//!
//! **One ladder, one bracket.** The backend and the read bracket come
//! from [`super::open`] and [`super::read`]. Therefore this file
//! proves the same code that the daemon runs, and not a copy of it. A
//! single named box with no `--dwell` is exactly the one-shot path of
//! the product (own backend, one bracket, no reuse). That case goes
//! through [`super::oneshot`]. Every other case keeps one backend
//! across several boxes, which the dwell damage race needs.

use super::backend;
use super::geometry;
use crate::wayland;
use anyhow::{Context, Result};
use chibipop::geom::PhysRect;
use chibipop::text::Frame;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// The target that the caller gave to the dump.
pub struct Args {
    /// One explicit box in global physical pixels.
    pub region: Option<PhysRect>,
    /// Where the PNGs go.
    pub out: PathBuf,
    /// Repeats the first grab this many times, one read for each.
    pub dwell: u32,
    /// Grabs each whole output, and not a centered sample.
    pub full: bool,
    /// The directory that holds the restore token of the portal rung.
    /// The dump and the daemon share one grant.
    pub state_dir: PathBuf,
}

/// Parses `x,y,w,h` in global physical pixels.
pub fn parse_region(text: &str) -> Result<PhysRect> {
    let mut it = text.split(',');
    let mut next = |what: &str| -> Result<i32> {
        it.next()
            .with_context(|| format!("--region is x,y,w,h; {what} is missing"))?
            .trim()
            .parse::<i32>()
            .with_context(|| format!("--region {what} is not a number"))
    };
    let rect = PhysRect {
        x: next("x")?,
        y: next("y")?,
        w: next("w")?,
        h: next("h")?,
    };
    anyhow::ensure!(it.next().is_none(), "--region takes exactly x,y,w,h");
    anyhow::ensure!(rect.w > 0 && rect.h > 0, "--region needs a positive size");
    Ok(rect)
}

pub fn run(args: Args) -> Result<()> {
    let display = wayland::display_name()?;
    let conn = wayland_client::Connection::connect_to_env()
        .context("connecting to the Wayland display")?;
    let globals = wayland::collect_globals(&conn)?;
    println!("WAYLAND_DISPLAY={display}");

    let (ov, warning) = backend::BackendOverride::from_env();
    if let Some(w) = warning {
        println!("{w}");
    }
    let caps = backend::Capabilities::scan(&globals, super::portal::available());
    let selection = backend::select(&caps, ov);
    println!("{}", selection.startup_line());

    let setup = super::Setup {
        globals,
        backend: selection.backend(),
        state_dir: args.state_dir.clone(),
    };

    // One named box and no dwell selects the one-shot path of the
    // product. This code runs that path as the product runs it.
    // `oneshot` opens the ladder itself. Therefore this code needs
    // nothing above `oneshot`, and it reuses nothing below it.
    if let (Some(region), 0) = (args.region, args.dwell) {
        println!("capture: one-shot path - this grab's own backend");
        let started = Instant::now();
        let frame = super::oneshot(&setup, region)?;
        let path = args.out.join("chibipop-capture-0.png");
        report(&frame, region, started, Some(&path))?;
        return Ok(());
    }

    let super::Opened { mut backend, outputs } =
        super::open(&setup, &mut |line| println!("{line}"))?;

    for (i, g) in outputs.iter().enumerate() {
        let b = geometry::physical_box(g);
        println!(
            "capture: output {i} logical {}x{}+{}+{} physical {}x{}+{}+{} scale {:.4}",
            g.logical_w, g.logical_h, g.logical_x, g.logical_y, b.w, b.h, b.x, b.y, g.scale()
        );
    }
    anyhow::ensure!(!outputs.is_empty(), "no output geometry arrived");

    let regions: Vec<PhysRect> = match args.region {
        Some(r) => vec![r],
        None => outputs
            .iter()
            .filter(|g| geometry::known(g))
            .map(|g| sample_of(geometry::physical_box(g), args.full))
            .collect(),
    };
    anyhow::ensure!(!regions.is_empty(), "nothing to grab");

    for (i, region) in regions.iter().enumerate() {
        let b = backend.bounds_containing(region.center());
        println!(
            "capture: bounds for ({},{}) is {}x{}+{}+{}",
            region.center().x,
            region.center().y,
            b.w,
            b.h,
            b.x,
            b.y
        );
        let path = args.out.join(format!("chibipop-capture-{i}.png"));
        let started = Instant::now();
        let frame = super::read(backend.as_mut(), *region)?;
        report(&frame, *region, started, Some(&path))?;
    }

    for i in 0..args.dwell {
        println!("capture: dwell {} of {}", i + 1, args.dwell);
        let started = Instant::now();
        let frame = super::read(backend.as_mut(), regions[0])?;
        report(&frame, regions[0], started, None)?;
    }
    Ok(())
}

/// Prints the decision of one read as a line, and writes a PNG if the
/// caller gives a path.
///
/// [`super::read`] is the bracket itself. This function is the half
/// that belongs only to the diagnostic. Therefore the library never
/// learns to print.
fn report(
    frame: &Frame,
    region: PhysRect,
    started: Instant,
    write_to: Option<&Path>,
) -> Result<()> {
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    let centre = pixel(&frame.buf, frame.w, frame.w / 2, frame.h / 2);
    println!(
        "capture: {}x{} at ({},{}) source={} unchanged={} {:.1}ms centre=#{:02x}{:02x}{:02x} mean=#{}",
        frame.w,
        frame.h,
        region.x,
        region.y,
        frame.source,
        frame.unchanged,
        ms,
        centre[2],
        centre[1],
        centre[0],
        mean(&frame.buf),
    );
    if let Some(path) = write_to {
        let bytes = chibipop::image::encode_bgra_to_png(&frame.buf, frame.w, frame.h)?;
        std::fs::write(path, &bytes)
            .with_context(|| format!("writing {}", path.display()))?;
        println!("capture: wrote {} ({} bytes)", path.display(), bytes.len());
    }
    Ok(())
}

/// Returns a box to look at on this output: the whole output, or a
/// centered sample of hover size.
fn sample_of(output: PhysRect, full: bool) -> PhysRect {
    if full {
        return output;
    }
    let w = output.w.min(640);
    let h = output.h.min(360);
    PhysRect { x: output.x + (output.w - w) / 2, y: output.y + (output.h - h) / 2, w, h }
}

fn pixel(buf: &[u8], w: i32, x: i32, y: i32) -> [u8; 4] {
    let o = ((y as usize) * (w as usize) + x as usize) * 4;
    buf.get(o..o + 4).map(|p| [p[0], p[1], p[2], p[3]]).unwrap_or_default()
}

/// Returns the mean color as a hex triple. The triple is enough to
/// tell pixels from blackness.
fn mean(buf: &[u8]) -> String {
    let px = buf.len() / 4;
    if px == 0 {
        return "none".to_string();
    }
    let mut sums = [0u64; 3];
    let (pixels, _) = buf.as_chunks::<4>();
    for p in pixels {
        sums[0] += u64::from(p[0]);
        sums[1] += u64::from(p[1]);
        sums[2] += u64::from(p[2]);
    }
    let px = px as u64;
    format!("{:02x}{:02x}{:02x}", sums[2] / px, sums[1] / px, sums[0] / px)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_region_parses_as_x_y_w_h() {
        assert_eq!(parse_region("10,20,30,40").unwrap(), PhysRect { x: 10, y: 20, w: 30, h: 40 });
        assert_eq!(parse_region(" -5 , 0 , 8 , 9 ").unwrap(), PhysRect { x: -5, y: 0, w: 8, h: 9 });
    }

    #[test]
    fn a_malformed_region_is_refused() {
        for bad in ["", "1,2,3", "1,2,3,4,5", "a,2,3,4", "1,2,0,4", "1,2,3,-1"] {
            assert!(parse_region(bad).is_err(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn a_sample_sits_inside_its_output() {
        let output = PhysRect { x: 1920, y: 0, w: 3840, h: 2160 };
        let s = sample_of(output, false);
        assert_eq!((s.w, s.h), (640, 360));
        assert!(s.x >= output.x && s.x + s.w <= output.x + output.w);
        assert!(s.y >= output.y && s.y + s.h <= output.y + output.h);
        assert_eq!(sample_of(output, true), output);
    }

    #[test]
    fn a_small_output_samples_itself() {
        let output = PhysRect { x: 0, y: 0, w: 320, h: 200 };
        assert_eq!(sample_of(output, false), output);
    }

    #[test]
    fn the_mean_of_one_known_colour_is_that_colour() {
        let buf = [0x33, 0x22, 0x11, 0xff].repeat(16);
        assert_eq!(mean(&buf), "112233");
        assert_eq!(mean(&[]), "none");
    }
}
