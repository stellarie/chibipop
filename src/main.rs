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
        Command::Probe { at, dict, rules } => {
            let (xs, ys) = at
                .split_once(',')
                .context("--at must be X,Y (e.g. --at 1200,400)")?;
            let cursor = chibipop::geom::PhysPoint {
                x: xs.trim().parse().context("X in --at is not an integer")?,
                y: ys.trim().parse().context("Y in --at is not an integer")?,
            };

            let source = chibipop::text::ocr::OcrTextSource::new()?;
            let region = chibipop::text::layout::region_around(cursor);
            println!("cursor:  ({}, {})", cursor.x, cursor.y);
            println!("region:  x={} y={} w={} h={}", region.x, region.y, region.w, region.h);

            match source.resolve_at(cursor)? {
                None => println!("\nno text resolved at that point"),
                Some(r) => {
                    println!("orient:  {:?}", r.orientation);
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
            Ok(())
        }
        Command::Watch { dict, rules } => {
            let source = chibipop::text::ocr::OcrTextSource::new()?;
            let dictionary = SqliteDictionary::open(&dict)?;
            let engine = LookupEngine::new(Deconjugator::new(load_rules(&rules)?));

            println!("watching - hover over Japanese text. Ctrl-C to stop.\n");

            let mut last_pos: Option<chibipop::geom::PhysPoint> = None;
            let mut last_key: Option<(String, usize)> = None;

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
                let resolved = match source.resolve_at(cursor) {
                    Ok(r) => r,
                    Err(e) => { eprintln!("resolve: {e}"); continue; }
                };
                let Some(r) = resolved else { continue };

                let key = (r.span.text.clone(), r.span.cursor_byte_offset);
                if last_key.as_ref() == Some(&key) {
                    continue;
                }
                last_key = Some(key);

                let ch = r.span.text[r.span.cursor_byte_offset..].chars().next();
                println!("── ({}, {})  {:?}  {:?}", cursor.x, cursor.y, r.orientation, ch);
                match engine.run(&dictionary, &r.span.text[r.span.cursor_byte_offset..]) {
                    Ok(hits) => print_hits(&hits),
                    Err(e) => eprintln!("lookup: {e}"),
                }
                println!();
            }
        }
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
