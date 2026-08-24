//! The Linux program itself: clap CLI, dispatched to the daemon, the
//! `ctl` client, or the capability probe. `main.rs` is a two-line entry
//! that reaches this module on Linux and a stub everywhere else.

use crate::control::{self, Verb};
use crate::paths::{self, Paths};
use crate::{daemon, wayland};
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "chibipop", version, about = "Japanese lookup engine (Wayland)")]
struct Cli {
    /// Use this config file; skips portable/XDG config discovery.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon (the default when no subcommand is given).
    Run,
    /// Send one verb to the running daemon's control socket.
    ///
    /// The forever verb set: reload, trigger-down, trigger-up, toggle.
    /// Bind these in your compositor, e.g. sway:
    ///   bindsym --no-repeat Mod4+j exec chibipop ctl trigger-down
    Ctl { verb: String },
    /// Connect to the Wayland display, print the capability report, exit.
    Probe,
}

pub fn run() -> ExitCode {
    let cli = Cli::parse();
    let env = paths::Env::from_process();
    let paths = paths::resolve(&env, cli.config);
    let result = match cli.command.unwrap_or(Command::Run) {
        Command::Run => daemon::run(paths),
        Command::Ctl { verb } => ctl(&paths, &verb),
        Command::Probe => probe(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("chibipop: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// One verb over the socket; prints the daemon's reply.
fn ctl(paths: &Paths, verb_text: &str) -> Result<()> {
    let Some(verb) = Verb::parse(verb_text) else {
        bail!("unknown verb {verb_text:?}; expected one of {}", control::verb_list());
    };
    let display = wayland::display_name()?;
    let runtime_dir = paths.runtime_dir()?;
    let reply = control::send(runtime_dir, &display, verb).with_context(|| {
        format!(
            "connecting to {} - is the daemon running?",
            runtime_dir.join(control::file_name(&display)).display()
        )
    })?;
    println!("{reply}");
    if !reply.starts_with("OK") {
        bail!("the daemon refused: {reply}");
    }
    Ok(())
}

/// The startup capability report, on demand and without the lock: safe
/// to run beside a live daemon.
fn probe() -> Result<()> {
    let display = wayland::display_name()?;
    let conn =
        wayland_client::Connection::connect_to_env().context("connecting to the Wayland display")?;
    println!("WAYLAND_DISPLAY={display}");
    for line in wayland::report(&wayland::collect_globals(&conn)?) {
        println!("{line}");
    }
    Ok(())
}
