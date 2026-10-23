//! The daemon settings-child guard
//! (ARCHITECTURE.md#settings-and-config).
//! The guard tracks at most one spawned settings process by process ID.
//!
//! The guard is the daemon's internal control. The settings-scoped
//! flock in `super::run` controls separate processes. A second instance
//! that starts directly does not read this guard. The `spawn_if_absent`
//! function collects dead children with `try_wait`. The system does not
//! use SIGCHLD signals for one temporary child.

use std::io;
use std::process::{Child, Command};

/// The result of one spawn attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnOutcome {
    Spawned(u32),
    /// The previous child is still alive. The function started no process.
    AlreadyRunning(u32),
}

/// Holds the one active child process, if one exists.
#[derive(Default)]
pub struct SettingsChild {
    child: Option<Child>,
}

impl SettingsChild {
    pub fn new() -> SettingsChild {
        SettingsChild::default()
    }

    /// Spawn `command` unless the previous child still runs.
    /// The function collects an exited child before it starts a new child.
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

/// The production command for this binary with the `settings` subcommand.
///
/// The command uses `unmasked` because the daemon blocks SIGINT and SIGTERM
/// on all threads. A signal mask survives across `exec`. Without this call,
/// the settings window cannot receive stop signals from a supervisor or session.
pub fn settings_command() -> io::Result<Command> {
    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command.arg("settings");
    crate::signals::unmasked(&mut command);
    Ok(command)
}
