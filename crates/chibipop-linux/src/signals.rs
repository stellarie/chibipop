//! This module explains how the daemon receives shutdown signals.
//!
//! `signalfd` is the only path for SIGINT and SIGTERM.
//! Every daemon thread must block both signals.
//! The signal mask remains active after `exec`.
//! Each child process must unblock both signals before it calls `exec`.

use anyhow::{Context, Result};
use calloop::signals::{Signal, Signals};
use nix::sys::signal::{SigSet, Signal as NixSignal};
use std::os::unix::process::CommandExt;
use std::process::Command;

/// The daemon stops for SIGINT from Ctrl-C or SIGTERM from a supervisor.
/// All other signals keep their default disposition.
const SHUTDOWN: [Signal; 2] = [Signal::SIGINT, Signal::SIGTERM];

/// Block SIGINT and SIGTERM before the daemon creates another thread.
/// Return the calloop event source for both signals.
///
/// `Signals::new` blocks signals only on the thread that calls it.
/// Each new thread inherits that signal mask.
/// The daemon must call this function before it creates any other thread.
/// The tray, zbus, shortcuts, and clipboard threads are examples.
/// Otherwise, the kernel can deliver a process signal to one of those threads.
/// The default action then stops the process.
/// SIGTERM makes the process exit with code 143.
/// The control socket and the instance lock remain on disk.
/// No `signal:` or `shutdown:` line appears.
///
/// The daemon registers the event source on the calloop event pump later.
/// This order does not lose signals.
/// A startup signal remains in the signalfd.
/// The signal cannot stop a partly initialized daemon.
pub fn block_shutdown() -> Result<Signals> {
    Signals::new(&SHUTDOWN).context("blocking SIGINT/SIGTERM for the whole process")
}

/// Unblock SIGINT and SIGTERM in `command` before `exec`.
///
/// The signal mask from [`block_shutdown`] remains active after `exec`.
/// Without this function, every child process starts with SIGTERM blocked.
/// The settings process is the primary example.
/// See ARCHITECTURE.md#settings-and-config.
/// A session or supervisor must be able to stop that process.
/// Apply this rule at every child process call site.
/// This rule is simpler than a separate decision at each call site.
/// This function unblocks only the two signals that [`block_shutdown`] blocked.
/// It preserves every signal that the daemon parent blocked.
pub fn unmasked(command: &mut Command) -> &mut Command {
    // SAFETY: `pre_exec` calls this closure in the forked child process.
    // Only async-signal-safe calls are valid there.
    // POSIX defines `pthread_sigmask` as async-signal-safe.
    // This closure allocates no memory.
    // The stack owns the `SigSet` argument.
    unsafe {
        command.pre_exec(|| {
            let mut mask = SigSet::empty();
            mask.add(NixSignal::SIGINT);
            mask.add(NixSignal::SIGTERM);
            mask.thread_unblock().map_err(std::io::Error::from)
        })
    }
}
