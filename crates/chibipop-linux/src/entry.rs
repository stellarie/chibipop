//! This module runs the Linux program. It dispatches the clap CLI to the
//! daemon, the `ctl` client, the capability probe, or the capture dump.
//! `main.rs` has a two-line entry that reaches this module on Linux and a
//! stub on other platforms.

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
    /// Use this config file. Skip portable/XDG config discovery.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon. This is the default when no subcommand is given.
    Run,
    /// Send one verb to the active daemon's control socket.
    ///
    /// The forever verb set contains reload, trigger-down, trigger-up, toggle,
    /// anki-add, screenshot, ocr-clipboard, and static-region. Each verb names
    /// one global action. This is not an API for scripts
    /// (ARCHITECTURE.md#input-ladders). Bind these verbs in your compositor.
    /// For example, use these sway binds:
    ///   bindsym --no-repeat Mod4+j exec chibipop ctl trigger-down
    ///   bindsym --no-repeat Mod4+a exec chibipop ctl anki-add
    ///   bindsym --no-repeat Mod4+s exec chibipop ctl screenshot
    ///   bindsym --no-repeat Mod4+c exec chibipop ctl ocr-clipboard
    ///   bindsym --no-repeat Mod4+r exec chibipop ctl static-region
    Ctl { verb: String },
    /// Open the settings window in its own process
    /// (ARCHITECTURE.md#settings-and-config). A settings crash must not stop
    /// live hover.
    Settings,
    /// Connect to the Wayland display, print the capability report, and exit.
    Probe,
    /// Grab screen regions with the capture backend and write PNG files.
    ///
    /// This diagnostic reports the capture rung that the session selects, like
    /// `probe`. It uses no lock or socket, so it is safe beside an active daemon.
    /// `CHIBIPOP_CAPTURE_BACKEND=portal` selects the fallback rung and its consent
    /// dialog on a compositor that advertises screencopy. Without `--region`, it
    /// samples every output.
    CaptureDump {
        /// One box in global physical pixels: `x,y,w,h`.
        #[arg(long, value_name = "X,Y,W,H")]
        region: Option<String>,
        /// Directory for PNG files.
        #[arg(long, default_value = "/tmp", value_name = "DIR")]
        out: PathBuf,
        /// Repeat the first box read this many times. Use it to watch the damage
        /// race at a dwell pace (ARCHITECTURE.md#hover-cadence).
        #[arg(long, default_value_t = 0, value_name = "N")]
        dwell: u32,
        /// Grab each output in full instead of a centered sample.
        #[arg(long)]
        full: bool,
    },
    /// Take the clipboard selection with a known string and hold it.
    ///
    /// This command checks the clipboard ladder. It has the role that `probe` has
    /// for the capability report and `capture-dump` has for capture rungs. It uses
    /// no lock or socket, so it is safe beside an active daemon. It is the only
    /// way to learn whether this compositor lets a focus-less daemon own the
    /// selection. It reads nothing. A session with no data-control protocol
    /// prints the same refusal as the daemon and exits with a nonzero status.
    ///
    /// It replaces the current clipboard selection.
    ///
    ClipboardCheck {
        /// Text to put on the clipboard.
        #[arg(long, default_value = "chibipop clipboard check", value_name = "TEXT")]
        text: String,
        /// Seconds to hold the selection so another client can read it.
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

/// Run the clipboard ladder diagnostic. Take the selection, hold it, and
/// report the result.
///
/// This function uses no lock or socket, like `probe`. It makes the real
/// selection handoff instead of the advertised globals. A focus-less
/// client can own the selection only when the compositor implements data
/// control. The registry cannot prove this fact.
fn clipboard_check(text: &str, hold: u64) -> Result<()> {
    let display = wayland::display_name()?;
    let conn =
        wayland_client::Connection::connect_to_env().context("connecting to the Wayland display")?;
    let globals = wayland::collect_globals(&conn)?;
    println!("WAYLAND_DISPLAY={display}");

    // This one-shot process has no pump. Drain the thread's notes and print
    // them here, not in the log.
    let (notes_tx, notes) = calloop::channel::channel::<String>();
    let Some(board) = clipboard::Clipboard::bind(&globals, notes_tx)? else {
        bail!("{}", clipboard::unavailable_line());
    };
    println!("clipboard: rung {} ({})", board.rung().global(), clipboard::TEXT_MIMES[0]);
    // Print this line only after the compositor accepts the selection. `wl-paste`
    // starts as soon as the line appears, so it must find a selection that the
    // compositor already knows.
    // A failed handoff prints the thread's own note first.
    if let Err(e) = board.set_and_settle(text) {
        while let Ok(line) = notes.try_recv() {
            println!("{line}");
        }
        return Err(e);
    }
    println!("clipboard: selection taken - {} character(s)", text.chars().count());

    // Keep this process alive on purpose. The offer answers `send` only while
    // this process lives, so a reader such as `wl-paste` needs time to ask.
    std::thread::sleep(std::time::Duration::from_secs(hold));
    while let Ok(line) = notes.try_recv() {
        println!("{line}");
    }
    println!("clipboard: held for {hold}s, releasing");
    Ok(())
}

/// Send one verb through the socket and print the daemon's reply.
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

/// Print the startup capability report without the lock. This command can
/// run beside an active daemon.
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

/// Write a diagnostic dump from the capture backend without the lock.
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
