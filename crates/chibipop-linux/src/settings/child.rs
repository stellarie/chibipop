//! The daemon's settings-child guard
//! (ARCHITECTURE.md#settings-and-config): at most one spawned settings
//! process, tracked by pid.
//!
//! The guard is the daemon's *own* discipline; the settings-scoped
//! flock in `super::run` is the cross-process one (a directly launched
//! second instance never consults this guard). Reaping happens lazily
//! in `spawn_if_absent` via `try_wait` — no SIGCHLD plumbing for one
//! transient child.

use std::io;
use std::process::{Child, Command};

/// What one spawn attempt did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnOutcome {
    Spawned(u32),
    /// The previous child is still alive; nothing was launched.
    AlreadyRunning(u32),
}

/// Holds the one live child, if any.
#[derive(Default)]
pub struct SettingsChild {
    child: Option<Child>,
}

impl SettingsChild {
    pub fn new() -> SettingsChild {
        SettingsChild::default()
    }

    /// Spawn `command` unless the previous child still runs. An exited
    /// child is reaped here, so a fresh spawn follows a close.
    pub fn spawn_if_absent(&mut self, command: &mut Command) -> io::Result<SpawnOutcome> {
        if let Some(child) = &mut self.child {
            match child.try_wait()? {
                None => return Ok(SpawnOutcome::AlreadyRunning(child.id())),
                Some(_status) => self.child = None,
            }
        }
        let child = command.spawn()?;
        let pid = child.id();
        self.child = Some(child);
        Ok(SpawnOutcome::Spawned(pid))
    }
}

/// The production command: this very binary, `settings` subcommand.
///
/// `unmasked` because the daemon blocks SIGINT/SIGTERM for every thread
/// it has and a mask outlives `exec`: without it the settings window - a
/// long-lived process of its own - would start unable to hear a
/// supervisor or a session asking it to stop.
pub fn settings_command() -> io::Result<Command> {
    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command.arg("settings");
    crate::signals::unmasked(&mut command);
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A child we fully control; `/bin/sleep` exists on any test box.
    fn sleeper(seconds: &str) -> Command {
        let mut c = Command::new("/bin/sleep");
        c.arg(seconds);
        c
    }

    #[test]
    fn second_spawn_finds_the_first_alive() {
        let mut guard = SettingsChild::new();
        let SpawnOutcome::Spawned(pid) = guard.spawn_if_absent(&mut sleeper("30")).unwrap()
        else {
            panic!("first spawn must spawn");
        };
        assert_eq!(
            guard.spawn_if_absent(&mut sleeper("30")).unwrap(),
            SpawnOutcome::AlreadyRunning(pid)
        );
        // Cleanup: don't leak a 30s sleeper into the test run.
        if let Some(child) = &mut guard.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    #[test]
    fn an_exited_child_is_reaped_and_replaced() {
        let mut guard = SettingsChild::new();
        let SpawnOutcome::Spawned(first) = guard.spawn_if_absent(&mut sleeper("0")).unwrap()
        else {
            panic!("first spawn must spawn");
        };
        // Wait for the child to exit for real, then respawn.
        guard.child.as_mut().unwrap().wait().unwrap();
        match guard.spawn_if_absent(&mut sleeper("0")).unwrap() {
            SpawnOutcome::Spawned(second) => assert_ne!(first, second),
            SpawnOutcome::AlreadyRunning(_) => panic!("an exited child must be replaced"),
        }
        guard.child.as_mut().unwrap().wait().unwrap();
    }
}
