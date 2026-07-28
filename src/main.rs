// Allow-by-default, so silent until asked for.
#![warn(missing_unsafe_on_extern)]
#![warn(unsafe_attr_outside_unsafe)]
#![warn(unsafe_op_in_unsafe_fn)]

use anyhow::{Context, Result};
use chibipop::lookup::deconj::Deconjugator;
use chibipop::lookup::engine::LookupEngine;
use chibipop::lookup::model::Dictionary;
use chibipop::lookup::rules::load_rules;
use chibipop::lookup::sqlite::SqliteDictionary;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "chibipop", about = "Japanese lookup engine")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Look up Japanese text and print ranked results.
    Lookup {
        text: String,
        #[arg(long)]
        dict: Option<PathBuf>,
        #[arg(long)]
        rules: Option<PathBuf>,
    },
    /// Resolve and look up the text at one screen point, showing every stage.
    Probe {
        /// Screen coordinates as X,Y in physical pixels.
        #[arg(long, value_name = "X,Y")]
        at: String,
        /// Capture box size as W,H, centred on --at. Defaults to the
        /// shipped REGION_W,REGION_H. Windows' OCR reads a whole image at
        /// once, so this changes what it recognises - vary it to measure.
        #[arg(long, value_name = "W,H")]
        region: Option<String>,
        /// Total OCR passes, as [ocr] max_ocr_passes would set. Defaults to
        /// 1 because that is what the app ships: a diagnostic that defaults
        /// to a configuration production does not use measures the wrong
        /// thing. Ignored when --region is given, which is single-capture by
        /// definition.
        #[arg(long, default_value_t = 1)]
        tiles: u8,
        #[arg(long)]
        dict: Option<PathBuf>,
        #[arg(long)]
        rules: Option<PathBuf>,
        /// Draw the captured regions, and the match box, for this many
        /// seconds after probing. Bare `--show-region` shows them for 3s.
        /// Omitted, nothing is drawn. Note probe always draws both, being
        /// the diagnostic view; the app draws the capture regions only
        /// under `[debug] show_scan_region`, so a real hover on the shipped
        /// defaults shows one box where this shows several.
        #[arg(long, value_name = "SECONDS", num_args = 0..=1, default_missing_value = "3")]
        show_region: Option<u64>,
    },
    /// Open the settings window on its own, without starting the popup.
    ///
    /// The way in when the tray icon will not open its menu. Applying saves
    /// `chibipop.toml`; restart chibipop for it to take effect.
    Settings {
        #[arg(long)]
        dict: Option<PathBuf>,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Follow the cursor and print a lookup whenever the hovered word changes.
    Watch {
        #[arg(long)]
        dict: Option<PathBuf>,
        #[arg(long)]
        rules: Option<PathBuf>,
        /// Total OCR passes, as [ocr] max_ocr_passes would set. The memory
        /// figures in docs/REGRESSION.md were taken at 3.
        #[arg(long, default_value_t = 1)]
        tiles: u8,
    },
    /// Run the popup application: hover Japanese text anywhere on screen to
    /// see its definition beside it. Right-click the tray icon to change
    /// mode or quit.
    Run {
        #[arg(long)]
        dict: Option<PathBuf>,
        #[arg(long)]
        rules: Option<PathBuf>,
        /// Defaults to chibipop.toml beside the running executable (spec
        /// section 4.3), not this crate's data/-relative CWD convention -
        /// so a shortcut-launched chibipop.exe still finds its settings.
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Lookup { text, dict, rules } => {
            let dict = dict_path(dict);
            let rules = rules_path(rules);
            let dictionary = SqliteDictionary::open(&dict).with_context(|| {
                format!("opening {} - build it with tools/build-dict/build.py",
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
        Command::Probe { at, region, tiles, dict, rules, show_region } => {
            let dict = dict_path(dict);
            let rules = rules_path(rules);
            let (xs, ys) = at
                .split_once(',')
                .context("--at must be X,Y (e.g. --at 1200,400)")?;
            let cursor = chibipop::geom::PhysPoint {
                x: xs.trim().parse().context("X in --at is not an integer")?,
                y: ys.trim().parse().context("Y in --at is not an integer")?,
            };

            let source = chibipop::text::ocr::OcrTextSource::new(tiles)?;
            let region_was_default = region.is_none();
            let region = match region {
                None => chibipop::text::layout::region_around(cursor),
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

            let (lines, resolved) = source.resolve_in_region(cursor, region)?;

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
                // Distinguish "OCR itself found nothing" from "OCR found
                // text, but none of it was close enough to the cursor" -
                // probe exists to tell these two failure stages apart.
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

                // Last: drawn over the captures.
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
            let source = chibipop::text::ocr::OcrTextSource::new(tiles)?;
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

                // Skip the work entirely unless the pointer actually moved.
                if let Some(prev) = last_pos {
                    if (cursor.x - prev.x).abs() <= 4 && (cursor.y - prev.y).abs() <= 4 {
                        continue;
                    }
                }
                last_pos = Some(cursor);

                // One bad frame must not end the session.
                let resolved = match source.resolve_at_tiled(cursor) {
                    Ok(r) => r,
                    Err(e) => { eprintln!("resolve: {e}"); continue; }
                };
                let Some(r) = resolved else { continue };

                // Key on the hovered character's own anchor - absolute
                // virtual-desktop space, independent of how the sliding
                // capture region clipped the surrounding line - plus the
                // character itself. Keying on the assembled line text
                // instead (as before) reprints on every re-clip, because the
                // line gains or loses characters at either end as the region
                // slides even when the cursor is still over the same glyph.
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
            // Opened only for the dictionary identities the reorder list
            // shows - no engine, no rules, no OCR.
            let dictionary = SqliteDictionary::open(&dict).with_context(|| {
                format!("opening {} - build it with tools/build-dict/build.py",
                        dict.display())
            })?;
            let dicts = dictionary.dicts().context("reading dictionary identities")?;
            chibipop::app::settings_only(cfg, &dicts, &config_path)
        }
        Command::Run { dict, rules, config } => {
            let dict = dict_path(dict);
            let rules = rules_path(rules);
            let config_path = config.unwrap_or_else(default_config_path);
            let cfg = chibipop::config::load_or_create(&config_path)
                .with_context(|| format!("loading config from {}", config_path.display()))?;
            chibipop::app::run(cfg, &dict, &rules, &config_path)
        }
    }
}

/// --dict, or the shipped default.
fn dict_path(given: Option<PathBuf>) -> PathBuf {
    given.unwrap_or_else(|| chibipop::paths::data_file("data/chibipop.sqlite"))
}

/// --rules, or the shipped default.
fn rules_path(given: Option<PathBuf>) -> PathBuf {
    given.unwrap_or_else(|| chibipop::paths::data_file("data/deconjugator.json"))
}

/// `chibipop.toml` beside the running executable (spec section 4.3). Falls
/// back to the current directory if the executable's own path can't be
/// determined, which should not happen in practice.
fn default_config_path() -> PathBuf {
    chibipop::paths::beside_exe("chibipop.toml")
}

/// Pump this thread's messages for `dur`, so a window created by a one-shot
/// command stays painted instead of appearing frozen. `probe` has no message
/// loop of its own; this exists only for `--show-region`.
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
