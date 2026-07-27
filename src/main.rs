use anyhow::{Context, Result};
use chibipop::lookup::deconj::Deconjugator;
use chibipop::lookup::engine::LookupEngine;
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
        #[arg(long, default_value = "data/chibipop.sqlite")]
        dict: PathBuf,
        #[arg(long, default_value = "data/deconjugator.json")]
        rules: PathBuf,
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
        /// Total OCR passes, as [ocr] max_ocr_passes would set. 1 is single
        /// capture. Ignored when --region is given, which is single-capture
        /// by definition.
        #[arg(long, default_value_t = 3)]
        tiles: u8,
        #[arg(long, default_value = "data/chibipop.sqlite")]
        dict: PathBuf,
        #[arg(long, default_value = "data/deconjugator.json")]
        rules: PathBuf,
    },
    /// Follow the cursor and print a lookup whenever the hovered word changes.
    Watch {
        #[arg(long, default_value = "data/chibipop.sqlite")]
        dict: PathBuf,
        #[arg(long, default_value = "data/deconjugator.json")]
        rules: PathBuf,
    },
    /// Run the popup application: hover Japanese text anywhere on screen to
    /// see its definition beside it. Right-click the tray icon to change
    /// mode or quit.
    Run {
        #[arg(long, default_value = "data/chibipop.sqlite")]
        dict: PathBuf,
        #[arg(long, default_value = "data/deconjugator.json")]
        rules: PathBuf,
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
        Command::Probe { at, region, tiles, dict, rules } => {
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

            match resolved {
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
                }
            }

            if region_was_default && tiles > 1 {
                match source.resolve_at_tiled(cursor)? {
                    None => println!("\ntiled:   nothing resolved"),
                    Some(r) => println!("\ntiled:   {:?}  ({} chars)", r.span.text, r.span.text.chars().count()),
                }
            }
            Ok(())
        }
        Command::Watch { dict, rules } => {
            let source = chibipop::text::ocr::OcrTextSource::new(3)?;
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
        Command::Run { dict, rules, config } => {
            let config_path = config.unwrap_or_else(default_config_path);
            let cfg = chibipop::config::load_or_create(&config_path)
                .with_context(|| format!("loading config from {}", config_path.display()))?;
            chibipop::app::run(cfg, &dict, &rules, &config_path)
        }
    }
}

/// `chibipop.toml` beside the running executable (spec section 4.3). Falls
/// back to the current directory if the executable's own path can't be
/// determined, which should not happen in practice.
fn default_config_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|dir| dir.join("chibipop.toml")))
        .unwrap_or_else(|| PathBuf::from("chibipop.toml"))
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
