//! `chibipop capture-dump`: the capture backend's own eyes.
//!
//! A diagnostic, like `probe`, and no part of the product: it opens the
//! real backend on this thread, grabs real boxes, writes PNGs, and
//! prints what the pacing decided per grab - so a dwell on a static
//! screen can be *watched* answering without copying, and damage can be
//! watched arriving. Without it the only proof of a capture backend is
//! a hover, which is a slow and indirect way to see one pixel.
//!
//! It takes the lock-free path on purpose: no instance lock, no control
//! socket, safe to run beside a live daemon.

use super::{geometry, png, WlrScreencopy};
use crate::wayland;
use anyhow::{Context, Result};
use chibipop::geom::PhysRect;
use chibipop::text::RegionCapture;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// What the dump was asked to look at.
pub struct Args {
    /// One explicit box in global physical pixels.
    pub region: Option<PhysRect>,
    /// Where the PNGs go.
    pub out: PathBuf,
    /// Repeat the first grab this many times, as one read each.
    pub dwell: u32,
    /// Grab each output whole instead of a centred sample.
    pub full: bool,
}

/// `x,y,w,h` in global physical pixels.
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
    if !super::available(&globals) {
        // ADR-0002: an absent rung is a rung the ladder skips.
        println!("capture: wlr-screencopy unavailable (the ladder would fall through)");
        anyhow::bail!("{} is not advertised", super::session::MANAGER_GLOBAL);
    }
    let mut backend = WlrScreencopy::open(&globals)?;
    let outputs = backend.outputs();
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
        grab_once(&mut backend, *region, Some(&path))?;
    }

    for i in 0..args.dwell {
        println!("capture: dwell {} of {}", i + 1, args.dwell);
        grab_once(&mut backend, regions[0], None)?;
    }
    Ok(())
}

/// One whole read - bracket included, exactly as the Worker reads - so
/// the pacing seen here is the pacing a hover gets.
fn grab_once(
    backend: &mut WlrScreencopy,
    region: PhysRect,
    write_to: Option<&Path>,
) -> Result<()> {
    let started = Instant::now();
    backend.begin_read();
    let frame = backend.grab(region);
    backend.end_read();
    let frame = frame.with_context(|| {
        format!("grabbing {}x{} at ({},{})", region.w, region.h, region.x, region.y)
    })?;
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
    anyhow::ensure!(
        frame.w == region.w && frame.h == region.h,
        "the backend answered {}x{} for a {}x{} request",
        frame.w,
        frame.h,
        region.w,
        region.h
    );
    if let Some(path) = write_to {
        let bytes = png::encode(&frame.buf, frame.w as u32, frame.h as u32);
        std::fs::write(path, &bytes)
            .with_context(|| format!("writing {}", path.display()))?;
        println!("capture: wrote {} ({} bytes)", path.display(), bytes.len());
    }
    Ok(())
}

/// A box worth looking at on this output: the whole thing, or a
/// centred sample of hover size.
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

/// Mean colour, as a hex triple: enough to tell pixels from blackness.
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
