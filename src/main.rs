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
            for (i, h) in hits.iter().enumerate() {
                let head = h.written.clone().or(h.reading.clone())
                    .unwrap_or_default();
                let reading = h.reading.clone().unwrap_or_default();
                let freq = h.freq
                    .map(|f| f.to_string())
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "{}. {head} [{reading}]  freq={freq}  match={}  score={:.2}",
                    i + 1, h.match_len, h.score
                );
                if !h.process.is_empty() {
                    println!("     via: {}", h.process.join(" -> "));
                }
                for sense in &h.entry.senses {
                    println!("     {}", sense.glosses.join("; "));
                }
            }
            Ok(())
        }
    }
}
