//! The Linux program itself: clap CLI, dispatched to the daemon, the
//! `ctl` client, the capability probe, or the capture dump. `main.rs`
//! is a two-line entry that reaches this module on Linux and a stub
//! everywhere else.

use crate::control::{self, Verb};
use crate::paths::{self, Paths};
use crate::{capture, clipboard, daemon, settings, wayland};
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
    /// The forever verb set: reload, trigger-down, trigger-up, toggle,
    /// anki-add, screenshot, ocr-clipboard, static-region. One verb per global
    /// action, never a scripting API (ADR-0003). Bind these in your compositor,
    /// e.g. sway:
    ///   bindsym --no-repeat Mod4+j exec chibipop ctl trigger-down
    ///   bindsym --no-repeat Mod4+a exec chibipop ctl anki-add
    ///   bindsym --no-repeat Mod4+s exec chibipop ctl screenshot
    ///   bindsym --no-repeat Mod4+c exec chibipop ctl ocr-clipboard
    ///   bindsym --no-repeat Mod4+r exec chibipop ctl static-region
    Ctl { verb: String },
    /// Open the settings window: its own process (ADR-0005), so a
    /// settings crash can never take live-hover down.
    Settings,
    /// Connect to the Wayland display, print the capability report, exit.
    Probe,
    /// Grab screen regions with the capture backend and write PNGs.
    ///
    /// A diagnostic for whichever ADR-0002 rung this session selects,
    /// like `probe`: lock-free, socket-free, safe beside a live daemon.
    /// `CHIBIPOP_CAPTURE_BACKEND=portal` forces the fallback rung (and
    /// its consent dialog) on a compositor that advertises screencopy.
    /// Without `--region` it samples every output.
    CaptureDump {
        /// One box in global physical pixels: `x,y,w,h`.
        #[arg(long, value_name = "X,Y,W,H")]
        region: Option<String>,
        /// Where the PNGs go.
        #[arg(long, default_value = "/tmp", value_name = "DIR")]
        out: PathBuf,
        /// Re-read the first box this many times, to watch the damage
        /// race pace a dwell (ADR-0010).
        #[arg(long, default_value_t = 0, value_name = "N")]
        dwell: u32,
        /// Grab each output whole instead of a centred sample.
        #[arg(long)]
        full: bool,
    },
    /// Take the clipboard selection with a known string and hold it.
    ///
    /// The clipboard ladder's diagnostic, the role `probe` plays for the
    /// capability report and `capture-dump` plays for the capture rungs:
    /// lock-free, socket-free, safe beside a live daemon, and the only
    /// way to see - rather than assume - whether this compositor lets a
    /// focus-less daemon own the selection at all. Reads nothing: a
    /// session with no data-control protocol prints the same refusal the
    /// daemon does and exits non-zero.
    ///
    /// It replaces whatever is currently on your clipboard.
    ClipboardCheck {
        /// What to put on the clipboard.
        #[arg(long, default_value = "chibipop clipboard check", value_name = "TEXT")]
        text: String,
        /// Hold the selection this long so another client can read it.
        #[arg(long, default_value_t = 3, value_name = "SECS")]
        hold: u64,
    },
}

pub fn run() -> ExitCode {
    let cli = Cli::parse();
    let env = paths::Env::from_process();
    let paths = paths::resolve(&env, cli.config);
    let result = match cli.command.unwrap_or(Command::Run) {
        Command::Run => daemon::run(paths),
        Command::Ctl { verb } => ctl(&paths, &verb),
        Command::Settings => settings::run(paths),
        Command::Probe => probe(),
        Command::CaptureDump { region, out, dwell, full } => {
            capture_dump(&paths, region.as_deref(), out, dwell, full)
        }
        Command::ClipboardCheck { text, hold } => clipboard_check(&text, hold),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("chibipop: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// The clipboard ladder's diagnostic: take the selection, hold it, say
/// what happened.
///
/// Lock-free and socket-free like `probe`, and it deliberately does the
/// real thing rather than reporting the advertised globals: whether a
/// focus-less client may own the selection here is a fact about the
/// compositor's data-control implementation, not about its registry.
fn clipboard_check(text: &str, hold: u64) -> Result<()> {
    let display = wayland::display_name()?;
    let conn =
        wayland_client::Connection::connect_to_env().context("connecting to the Wayland display")?;
    let globals = wayland::collect_globals(&conn)?;
    println!("WAYLAND_DISPLAY={display}");

    // The pump's note channel, without a pump: this is a one-shot
    // process, so the clipboard thread's lines are drained and printed
    // here instead of being logged there.
    let (notes_tx, notes) = calloop::channel::channel::<String>();
    let Some(board) = clipboard::Clipboard::bind(&globals, notes_tx)? else {
        bail!("{}", clipboard::unavailable_line());
    };
    println!("clipboard: rung {} ({})", board.rung().global(), clipboard::TEXT_MIMES[0]);
    // The line below is what a reader waits for, so the take has to have
    // settled before it is printed: `wl-paste` spawned the instant it
    // appears must find a selection the compositor already knows about.
    // A failed handover prints the thread's own account of it first.
    if let Err(e) = board.set_and_settle(text) {
        while let Ok(line) = notes.try_recv() {
            println!("{line}");
        }
        return Err(e);
    }
    println!("clipboard: selection taken - {} character(s)", text.chars().count());

    // Held on purpose: the offer only answers `send` while this process
    // lives, so a reader (`wl-paste`) needs a window in which to ask.
    std::thread::sleep(std::time::Duration::from_secs(hold));
    while let Ok(line) = notes.try_recv() {
        println!("{line}");
    }
    println!("clipboard: held for {hold}s, releasing");
    Ok(())
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

/// The capture backend's diagnostic dump, also lock-free.
fn capture_dump(
    paths: &Paths,
    region: Option<&str>,
    out: PathBuf,
    dwell: u32,
    full: bool,
) -> Result<()> {
    let region = region.map(capture::dump::parse_region).transpose()?;
    capture::dump::run(capture::dump::Args {
        region,
        out,
        dwell,
        full,
        state_dir: paths.state_dir.clone(),
    })
}
