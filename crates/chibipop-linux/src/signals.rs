//! The daemon's shutdown-signal discipline (ticket 13): which thread may
//! see SIGINT/SIGTERM, and who must not inherit that answer.
//!
//! Two rules, and they only work together. The signalfd is the daemon's
//! one way in, so every *thread* has to be out of the running for
//! delivery — and a mask survives `exec`, so every *child* has to be
//! given the default disposition back.

use anyhow::{Context, Result};
use calloop::signals::{Signal, Signals};
use nix::sys::signal::{SigSet, Signal as NixSignal};
use std::os::unix::process::CommandExt;
use std::process::Command;

/// The two the daemon shuts down on: an interactive Ctrl-C and a
/// supervisor stopping the unit. Everything else keeps its default.
const SHUTDOWN: [Signal; 2] = [Signal::SIGINT, Signal::SIGTERM];

/// Take SIGINT/SIGTERM off every thread this process will ever have, and
/// answer with the calloop source that now owns them.
///
/// **Call this before the first thread is spawned.** `Signals::new` masks
/// its *calling* thread only — calloop's own docs say so, and say the
/// simplest fix is to set the source up first — and a thread inherits the
/// mask at spawn, so ordering is the whole mechanism. Called after, the
/// daemon's other threads (tray, zbus, shortcuts, clipboard, ...) are all
/// candidates for a process-directed signal with an empty `SigBlk`, the
/// kernel picks one, and it takes the default action: exit 143 with the
/// control socket and the instance lock still on disk, and neither the
/// `signal:` nor the `shutdown:` line ever written.
///
/// The source is registered on the pump later, which is not a gap: the
/// signals are blocked from here on, so one arriving during startup waits
/// in the signalfd instead of killing a half-built daemon.
pub fn block_shutdown() -> Result<Signals> {
    Signals::new(&SHUTDOWN).context("blocking SIGINT/SIGTERM for the whole process")
}

/// Give `command` the default disposition back before it execs.
///
/// [`block_shutdown`]'s mask outlives `exec`, so without this every child
/// the daemon spawns starts life deaf to a SIGTERM. The settings window
/// (ADR-0005) is the one that matters — a long-lived process a session or
/// a supervisor has to be able to stop — but the rule is cheaper to hold
/// everywhere than to re-derive per call site. Exactly the two signals
/// [`block_shutdown`] took are handed back: whatever the daemon's own
/// parent chose to block is none of this function's business.
pub fn unmasked(command: &mut Command) -> &mut Command {
    // SAFETY: `pre_exec` runs in the forked child, where only
    // async-signal-safe calls are allowed. `pthread_sigmask` is one
    // (POSIX lists it), it allocates nothing, and the `SigSet` it is
    // handed is built on the stack.
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

    /// What `/proc/<tid>/status` says this thread has blocked.
    fn blocked() -> SigSet {
        SigSet::thread_get_mask().expect("reading the thread's signal mask")
    }

    /// The rule the ticket was filed on: the block reaches threads that
    /// do not exist yet, and only those spawned after it.
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

    /// And the rule that keeps the first one from costing more than it
    /// buys: the mask survives `exec`, so a child would inherit a
    /// SIGTERM it has no signalfd to read.
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

    /// `SigBlk` as the child itself reports it, with or without
    /// [`unmasked`] applied.
    ///
    /// `grep` is exec'd directly, with no shell in between: the mask
    /// itself survives `exec`, but Ubuntu's `dash` build clears its
    /// inherited mask at startup, so a `/bin/sh -c` probe measures the
    /// shell's own policy instead of the leak (it turned CI red while
    /// every bash-as-sh box passed).
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
        // Bit n-1 is signal n, the same order /proc prints and sigset
        // stores; rebuilt as a SigSet so the assertions read in signals.
        let mut mask = SigSet::empty();
        for signal in NixSignal::iterator() {
            if bits & (1 << (signal as i32 - 1)) != 0 {
                mask.add(signal);
            }
        }
        mask
    }
}
