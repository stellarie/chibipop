// Allow-by-default lints.
#![warn(missing_unsafe_on_extern)]
#![warn(unsafe_attr_outside_unsafe)]
#![warn(unsafe_op_in_unsafe_fn)]

use anyhow::{Context, Result};
use chibipop::lookup::deconj::Deconjugator;
use chibipop::lookup::engine::LookupEngine;
use chibipop::lookup::model::Dictionary;
use chibipop::lookup::rules::load_rules;
use chibipop::lookup::sqlite::SqliteDictionary;
use chibipop::text::capture::UPSCALE;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "chibipop", version, about = "Japanese lookup engine")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Look up text, print hits.
    Lookup {
        text: String,
        #[arg(long)]
        dict: Option<PathBuf>,
        #[arg(long)]
        rules: Option<PathBuf>,
    },
    /// Look up one screen point.
    Probe {
        /// X,Y in physical pixels.
        #[arg(long, value_name = "X,Y")]
        at: String,
        /// Capture box W,H, centred.
        #[arg(long, value_name = "W,H")]
        region: Option<String>,
        /// Total OCR passes.
        #[arg(long, default_value_t = 1)]
        tiles: u8,
        #[arg(long)]
        dict: Option<PathBuf>,
        #[arg(long)]
        rules: Option<PathBuf>,
        /// Draw boxes for N seconds.
        #[arg(long, value_name = "SECONDS", num_args = 0..=1, default_missing_value = "3")]
        show_region: Option<u64>,
        /// Override the OCR upscale factor.
        #[arg(long)]
        upscale: Option<i32>,
        /// Write the OCR'd pixels to a BMP.
        #[arg(long)]
        dump: Option<PathBuf>,
        /// Time N reads in this one process.
        #[arg(long)]
        repeat: Option<u32>,
    },
    /// Open the settings window.
    Settings {
        #[arg(long)]
        dict: Option<PathBuf>,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Print lookups on hover.
    Watch {
        #[arg(long)]
        dict: Option<PathBuf>,
        #[arg(long)]
        rules: Option<PathBuf>,
        /// Total OCR passes.
        #[arg(long, default_value_t = 1)]
        tiles: u8,
    },
    /// Run the popup application.
    Run {
        #[arg(long)]
        dict: Option<PathBuf>,
        #[arg(long)]
        rules: Option<PathBuf>,
        /// Defaults beside the exe.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Rebuilds chibipop.sqlite.
    BuildDict {
        /// Folder of .zip archives.
        #[arg(long)]
        library: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

fn main() -> Result<()> {
    chibipop::update::cleanup_old();
    chibipop::ui::console::hide();
    let cli = Cli::parse();
    // A double-click passes none.
    let command = cli.command.unwrap_or(Command::Run {
        dict: None,
        rules: None,
        config: None,
    });
    match command {
        Command::Lookup { text, dict, rules } => {
            let dict = dict_path(dict);
            let rules = rules_path(rules);
            let dictionary = SqliteDictionary::open(&dict).with_context(|| {
                format!("opening {} - add dictionaries in the settings window",
                        dict.display())
            })?;
            let engine =
                LookupEngine::new(Deconjugator::new(load_rules(&rules)?));

            let hits = engine.run(&dictionary, &text)?;
            if hits.is_empty() {
                println!("no results for {text}");
                return Ok(());
            }
            print_hits(&hits);
            Ok(())
        }
        Command::Probe { at, region, tiles, dict, rules, show_region, upscale, dump, repeat } => {
            let dict = dict_path(dict);
            let rules = rules_path(rules);
            let (xs, ys) = at
                .split_once(',')
                .context("--at must be X,Y (e.g. --at 1200,400)")?;
            let cursor = chibipop::geom::PhysPoint {
                x: xs.trim().parse().context("X in --at is not an integer")?,
                y: ys.trim().parse().context("Y in --at is not an integer")?,
            };

            let capture = probe_capture_size();
            let source =
                chibipop::text::ocr::OcrTextSource::new(tiles, false, capture, true, "ja")?;
            let region_was_default = region.is_none();
            let region = match region {
                None => chibipop::text::layout::region_around(cursor, false, capture),
                Some(spec) => {
                    let (ws, hs) = spec
                        .split_once(',')
                        .context("--region must be W,H (e.g. --region 900,300)")?;
                    let w: i32 = ws.trim().parse().context("W in --region is not an integer")?;
                    let h: i32 = hs.trim().parse().context("H in --region is not an integer")?;
                    chibipop::geom::PhysRect { x: cursor.x - w / 2, y: cursor.y - h / 2, w, h }
                }
            };
            println!("cursor:  ({}, {})", cursor.x, cursor.y);
            println!("region:  x={} y={} w={} h={}", region.x, region.y, region.w, region.h);
            if let Some(f) = upscale {
                println!("upscale: {f} (overridden, no adaptive retry)");
            }

            if let Some(n) = repeat {
                for i in 1..=n {
                    let t = std::time::Instant::now();
                    let read = source.resolve_in_region(cursor, region)?;
                    println!(
                        "read {i}: {:>4} ms  via {}  lines={}",
                        t.elapsed().as_millis(),
                        read.source.as_str(),
                        read.lines.len()
                    );
                }
                return Ok(());
            }

            // One pass, so the dump is what OCR read.
            let single = upscale.or(dump.as_ref().map(|_| UPSCALE));
            let (lines, resolved, source_used, dxgi_error) = match single {
                Some(factor) => {
                    let (lines, cap) = source.recognise_at_capture(region, factor)?;
                    if let Some(path) = &dump {
                        dump_bmp(path, &cap.buf, cap.w, cap.h)?;
                        println!("dump:    wrote {}x{} to {}", cap.w, cap.h, path.display());
                    }
                    let resolved = chibipop::text::layout::resolve(&lines, cursor, true);
                    (lines, resolved, cap.source, cap.dxgi_error)
                }
                None => {
                    let read = source.resolve_in_region(cursor, region)?;
                    (read.lines, read.resolved, read.source, read.dxgi_error)
                }
            };
            match &dxgi_error {
                Some(why) => println!("capture: {}  (dxgi: {why})", source_used.as_str()),
                None => println!("capture: {}", source_used.as_str()),
            }

            println!();
            if lines.is_empty() {
                println!("ocr:     no lines recognised in the region");
            } else {
                for (i, line) in lines.iter().enumerate() {
                    let assembled: String = line.words.iter().map(|w| w.text.as_str()).collect();
                    println!("ocr line {i}: {assembled:?}");
                    for word in &line.words {
                        println!("  word {:?}  x={} y={} w={} h={}",
                                 word.text, word.rect.x, word.rect.y, word.rect.w, word.rect.h);
                    }
                }
            }

            let mut highlight: Option<chibipop::geom::PhysRect> = None;
            match &resolved {
                // Two failure stages apart.
                None if lines.is_empty() => {
                    println!("\nno text resolved at that point: OCR recognised no lines in the region");
                }
                None => {
                    println!("\nno text resolved at that point: OCR recognised text, but hit-scan found none of it close enough to the cursor");
                }
                Some(r) => {
                    println!("\norient:  {:?}", r.orientation);
                    println!("line:    {}", r.span.text);
                    println!("at:      byte {} -> {:?}",
                             r.span.cursor_byte_offset,
                             r.span.text[r.span.cursor_byte_offset..].chars().next());
                    println!("anchor:  x={} y={} w={} h={}",
                             r.span.anchor.x, r.span.anchor.y,
                             r.span.anchor.w, r.span.anchor.h);

                    let dictionary = SqliteDictionary::open(&dict)?;
                    let engine = LookupEngine::new(Deconjugator::new(load_rules(&rules)?));
                    let hits = engine.run(&dictionary, &r.span.text[r.span.cursor_byte_offset..])?;
                    println!();
                    print_hits(&hits);

                    // The app's own highlight fn.
                    let presentation = chibipop::present::build(
                        &hits,
                        &dictionary.dicts()?,
                        &chibipop::config::Config::default().present_config(),
                    );
                    match chibipop::present::match_highlight(&r.span, presentation.top.as_ref()) {
                        Some(m) => {
                            println!("\nmatch:   x={} y={} w={} h={}  ({} chars)",
                                     m.x, m.y, m.w, m.h,
                                     presentation.top.as_ref().map_or(0, |c| c.match_len));
                            highlight = Some(m);
                        }
                        None => println!("\nmatch:   none (no hit, or no geometry on this path)"),
                    }
                }
            }

            let mut tiled_scan: Option<Vec<chibipop::geom::ScanRect>> = None;
            if region_was_default && tiles > 1 {
                let (tiled, scan) = source.resolve_at_tiled_scanned(cursor, show_region.is_some())?;
                match tiled {
                    None => println!("\ntiled:   nothing resolved"),
                    Some(r) => println!("\ntiled:   {:?}  ({} chars)", r.span.text, r.span.text.chars().count()),
                }
                tiled_scan = Some(scan);
            }

            if let Some(seconds) = show_region {
                let mut scan = tiled_scan.unwrap_or_else(|| {
                    let mut scan = vec![chibipop::geom::ScanRect {
                        rect: region,
                        kind: chibipop::geom::ScanKind::Pass1,
                    }];
                    if let Some(r) = &resolved {
                        scan.push(chibipop::geom::ScanRect {
                            rect: r.span.anchor,
                            kind: chibipop::geom::ScanKind::Anchor,
                        });
                    }
                    scan
                });

                // Last: drawn over captures.
                if let Some(rect) = highlight {
                    scan.push(chibipop::geom::ScanRect {
                        rect,
                        kind: chibipop::geom::ScanKind::Match,
                    });
                }

                if scan.is_empty() {
                    println!("\nshow-region: nothing was captured");
                } else {
                    match chibipop::ui::overlay::Overlay::create(false) {
                        Err(e) => eprintln!("chibipop: creating the scan overlay failed: {e:#}"),
                        Ok(overlay) => {
                            if let Err(e) = overlay.show_rects(&scan, &chibipop::ui::theme::Theme::dark()) {
                                eprintln!("chibipop: showing the scan overlay failed: {e:#}");
                            } else {
                                println!("\nshow-region: {} rect(s) for {seconds}s", scan.len());
                                pump_messages_for(std::time::Duration::from_secs(seconds));
                            }
                        }
                    }
                }
            }
            Ok(())
        }
        Command::Watch { dict, rules, tiles } => {
            let dict = dict_path(dict);
            let rules = rules_path(rules);
            let source = chibipop::text::ocr::OcrTextSource::new(
                tiles,
                false,
                chibipop::text::layout::CaptureSize::default(),
                true,
                "ja",
            )?;
            let dictionary = SqliteDictionary::open(&dict)?;
            let engine = LookupEngine::new(Deconjugator::new(load_rules(&rules)?));

            println!("watching - hover over Japanese text. Ctrl-C to stop.\n");

            let mut last_pos: Option<chibipop::geom::PhysPoint> = None;
            let mut last_key: Option<(i32, i32, Option<char>)> = None;

            loop {
                std::thread::sleep(std::time::Duration::from_millis(125));

                let cursor = match source.cursor() {
                    Ok(c) => c,
                    Err(e) => { eprintln!("cursor: {e}"); continue; }
                };

                // Only when it moved.
                if let Some(prev) = last_pos {
                    if (cursor.x - prev.x).abs() <= 4 && (cursor.y - prev.y).abs() <= 4 {
                        continue;
                    }
                }
                last_pos = Some(cursor);

                // One bad frame is not fatal.
                let resolved = match source.resolve_at_tiled(cursor) {
                    Ok(r) => r,
                    Err(e) => { eprintln!("resolve: {e}"); continue; }
                };
                let Some(r) = resolved else { continue };

                // Anchor+char, not line text.
                let ch = r.span.text[r.span.cursor_byte_offset..].chars().next();
                let key = (r.span.anchor.x, r.span.anchor.y, ch);
                if last_key.as_ref() == Some(&key) {
                    continue;
                }
                last_key = Some(key);

                println!("── ({}, {})  {:?}  {:?}", cursor.x, cursor.y, r.orientation, ch);
                match engine.run(&dictionary, &r.span.text[r.span.cursor_byte_offset..]) {
                    Ok(hits) => print_hits(&hits),
                    Err(e) => eprintln!("lookup: {e}"),
                }
                println!();
            }
        }
        Command::Settings { dict, config } => {
            let dict = dict_path(dict);
            let config_path = config.unwrap_or_else(default_config_path);
            let cfg = chibipop::config::load_or_create(&config_path)
                .with_context(|| format!("loading config from {}", config_path.display()))?;
            // Only for the dict names.
            //
            // A rebuild renames onto it.
            let dicts = {
                let dictionary = SqliteDictionary::open(&dict).with_context(|| {
                    format!("opening {} - rebuild it in the settings window",
                            dict.display())
                })?;
                dictionary.dicts().context("reading dictionary identities")?
            };
            chibipop::app::settings_only(cfg, &dicts, &config_path, &dict)
        }
        Command::Run { dict, rules, config } => {
            let dict = dict_path(dict);
            let rules = rules_path(rules);
            let config_path = config.unwrap_or_else(default_config_path);
            let cfg = chibipop::config::load_or_create(&config_path)
                .with_context(|| format!("loading config from {}", config_path.display()))?;
            chibipop::app::run(cfg, &dict, &rules, &config_path)
        }
        Command::BuildDict { library, out } => {
            let mut archives = Vec::new();
            for entry in std::fs::read_dir(&library)
                .with_context(|| format!("reading {}", library.display()))?
            {
                let path = entry?.path();
                if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
                    archives.push(path);
                }
            }
            // Python sorts case-folded.
            archives.sort_by_key(|p| (p.to_string_lossy().to_lowercase(), p.clone()));

            let mut term_archives = Vec::new();
            let mut freq_archives = Vec::new();
            for a in archives {
                if chibipop::dict::archive::is_frequency_archive(&a) {
                    freq_archives.push(a);
                } else {
                    term_archives.push(a);
                }
            }

            for (i, t) in term_archives.iter().enumerate() {
                println!("term dict  [{i}] {}", file_name(t));
            }
            for f in &freq_archives {
                println!("freq dict      {}", file_name(f));
            }

            // A typo is not a rebuild.
            if term_archives.is_empty() {
                anyhow::bail!("no term archives in {}", library.display());
            }

            let counts = chibipop::dict::build::build(
                &term_archives,
                &freq_archives,
                &out,
                &|line| println!("{line}"),
            )?;
            println!(
                "wrote {}: {} entries, {} term rows",
                out.display(),
                counts.entries,
                counts.terms
            );
            Ok(())
        }
    }
}

/// Debug: raw BGRA to an uncompressed BMP.
fn dump_bmp(path: &Path, buf: &[u8], w: i32, h: i32) -> Result<()> {
    use std::io::Write;
    let row_bytes = w as u32 * 4;
    let pixel_bytes = row_bytes * h as u32;
    let mut f = std::fs::File::create(path)?;
    f.write_all(b"BM")?;
    f.write_all(&(14 + 40 + pixel_bytes).to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&(14u32 + 40u32).to_le_bytes())?;
    f.write_all(&40u32.to_le_bytes())?;
    f.write_all(&w.to_le_bytes())?;
    f.write_all(&h.to_le_bytes())?; // positive: bottom-up
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&32u16.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&pixel_bytes.to_le_bytes())?;
    f.write_all(&2835i32.to_le_bytes())?;
    f.write_all(&2835i32.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    // buf is top-down; BMP with positive height is bottom-up.
    for row in (0..h as usize).rev() {
        let start = row * w as usize * 4;
        f.write_all(&buf[start..start + w as usize * 4])?;
    }
    Ok(())
}

/// --dict, or the default.
fn dict_path(given: Option<PathBuf>) -> PathBuf {
    given.unwrap_or_else(|| chibipop::paths::data_file("data/chibipop.sqlite"))
}

/// --rules, or the default.
fn rules_path(given: Option<PathBuf>) -> PathBuf {
    given.unwrap_or_else(|| chibipop::paths::data_file("data/deconjugator.json"))
}

/// A path's file name, or empty.
fn file_name(path: &Path) -> String {
    path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string()
}

/// Beside the running exe.
fn default_config_path() -> PathBuf {
    chibipop::paths::beside_exe("chibipop.toml")
}

/// Probe's box, from config.
fn probe_capture_size() -> chibipop::text::layout::CaptureSize {
    match chibipop::config::load_or_create(&default_config_path()) {
        Ok(cfg) => chibipop::text::layout::CaptureSize {
            w: cfg.ocr.capture_width,
            h: cfg.ocr.capture_height,
        },
        Err(e) => {
            eprintln!("chibipop: config unreadable, using the default capture size: {e:#}");
            chibipop::text::layout::CaptureSize::default()
        }
    }
}

/// Keeps a window painted.
fn pump_messages_for(dur: std::time::Duration) {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    let deadline = std::time::Instant::now() + dur;
    let mut msg = MSG::default();
    while std::time::Instant::now() < deadline {
        // SAFETY: `msg` is a valid, thread-local MSG; PeekMessageW with a null
        // HWND retrieves messages for any window this thread owns, and both
        // TranslateMessage and DispatchMessageW take it by const pointer.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn print_hits(hits: &[chibipop::lookup::model::Hit]) {
    if hits.is_empty() {
        println!("no results");
        return;
    }
    for (i, h) in hits.iter().enumerate() {
        let head = h.written.clone().or(h.reading.clone()).unwrap_or_default();
        let reading = h.reading.clone().unwrap_or_default();
        let freq = h.freq.map(|f| f.to_string()).unwrap_or_else(|| "-".to_string());
        println!("{}. {head} [{reading}]  freq={freq}  match={}  score={:.2}",
                 i + 1, h.match_len, h.score);
        if !h.process.is_empty() {
            println!("     via: {}", h.process.join(" -> "));
        }
        for sense in &h.entry.senses {
            println!("     {}", sense.glosses.join("; "));
        }
    }
}
