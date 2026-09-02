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

#[cfg(test)]
mod tests {
    use super::*;

    /// Get this thread's signal mask. `/proc/<tid>/status` reports it as `SigBlk`.
    fn blocked() -> SigSet {
        SigSet::thread_get_mask().expect("reading the thread's signal mask")
    }

    /// New threads inherit the signal mask.
    /// Only threads that start after [`block_shutdown`] inherit it.
    #[test]
    fn a_thread_spawned_after_the_block_inherits_it() {
        let before = std::thread::spawn(blocked).join().unwrap();
        assert!(
            !before.contains(NixSignal::SIGTERM),
            "an unblocked process's threads start with SIGTERM deliverable"
        );

        let guard = block_shutdown().unwrap();
        let after = std::thread::spawn(blocked).join().unwrap();
        for signal in [NixSignal::SIGINT, NixSignal::SIGTERM] {
            assert!(blocked().contains(signal), "{signal:?} on the blocking thread itself");
            assert!(after.contains(signal), "{signal:?} on a thread spawned after the block");
        }
        drop(guard);
    }

    /// The signal mask remains active after `exec`.
    /// A child needs [`unmasked`] because it does not own a signalfd.
    #[test]
    fn a_child_gets_the_default_disposition_back() {
        let guard = block_shutdown().unwrap();

        assert!(child_mask(false).contains(NixSignal::SIGTERM), "the leak this guards against");
        let handed_back = child_mask(true);
        for signal in [NixSignal::SIGINT, NixSignal::SIGTERM] {
            assert!(!handed_back.contains(signal), "{signal:?} must be the child's own again");
        }

        drop(guard);
    }

    /// Read `SigBlk` from the child process, with or without [`unmasked`].
    ///
    /// Call `grep` directly without a shell.
    /// The signal mask remains active after `exec`.
    /// The Ubuntu build of `dash` clears the inherited mask at startup.
    /// A `/bin/sh -c` check would test shell policy, not the inherited mask.
    fn child_mask(unmask: bool) -> SigSet {
        let mut command = Command::new("grep");
        command.args(["^SigBlk:", "/proc/self/status"]);
        if unmask {
            unmasked(&mut command);
        }
        let out = command.output().expect("grep exists on any test box");
        assert!(out.status.success(), "child failed: {out:?}");
        let text = String::from_utf8(out.stdout).unwrap();
        let hex = text.split_whitespace().nth(1).expect("SigBlk: <hex>");
        let bits = u64::from_str_radix(hex, 16).expect("a hex mask");
        // Bit n-1 represents signal n in `/proc` and `sigset`.
        // Rebuild a `SigSet` so the assertions can inspect each signal.
        let mut mask = SigSet::empty();
        for signal in NixSignal::iterator() {
            if bits & (1 << (signal as i32 - 1)) != 0 {
                mask.add(signal);
            }
        }
        mask
    }
}
