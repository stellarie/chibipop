//! `chibipop settings`: a separate process on iced (ADR-0005), so a
//! settings crash can never take live-hover down.
//!
//! Core's `Config` + `SettingsForm` drive the window; this module owns
//! only widgetry and the process discipline around it: the
//! settings-scoped flock, the read-only dictionary listing, and the
//! save-then-`reload` Apply. The window needs no Wayland globals beyond
//! what any toplevel client uses — it opens fine where hover is
//! unsupported and where no daemon runs at all.

mod app;
mod apply;
pub mod child;
mod snippets;

mod channel;

use crate::lock::{self, LockError};
use crate::paths::Paths;
use crate::{control, wayland};
use anyhow::{Context, Result};
use chibipop::library::Library;
use chibipop::lookup::model::Dictionary;
use chibipop::lookup::sqlite::SqliteDictionary;
use chibipop::present::DictInfo;
use std::path::Path;

pub fn run(paths: Paths) -> Result<()> {
    let display = wayland::display_name()?;
    let runtime_dir = paths.runtime_dir()?;

    // The settings-scoped flock, distinct from the daemon's: one window
    // per compositor instance, released by the kernel on any death.
    let lock = match lock::acquire_at(runtime_dir, &lock::settings_file_name(&display)) {
        Ok(lock) => lock,
        Err(LockError::AlreadyRunning { path, pid }) => {
            // A notice, not an error: the window the user wants is up.
            // No cross-process raise (ADR-0005) - compositors routinely
            // ignore self-activation.
            let holder = match pid {
                Some(pid) => format!("pid {pid}"),
                None => "an unknown pid".to_string(),
            };
            println!(
                "chibipop settings is already open for WAYLAND_DISPLAY={display} \
                 ({holder} holds {})",
                path.display()
            );
            return Ok(());
        }
        Err(LockError::Io(e)) => {
            return Err(e).with_context(|| {
                format!("acquiring the settings lock in {}", runtime_dir.display())
            });
        }
    };

    if let Some(parent) = paths.config_file.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating the config dir {}", parent.display()))?;
    }
    let cfg = chibipop::config::load_or_create(&paths.config_file)?;

    let dicts = read_dicts(&paths.data_dir.join("chibipop.sqlite"));
    let form = chibipop::settings::from_config(&cfg, &dicts);
    let form = match Library::load(&paths.data_dir.join("library")) {
        Ok(lib) => chibipop::settings::with_library(form, &lib),
        // No library yet is the common fresh-install case; the lists
        // just come from the config's order.
        Err(_) => form,
    };

    let init = app::Init {
        form,
        linux: apply::LinuxFields::from_config(&cfg),
        config_path: paths.config_file.clone(),
        socket_path: runtime_dir.join(control::file_name(&display)),
        log_path: paths.log_file(),
        compositor: snippets::Compositor::detect(),
        channel: channel::HotkeyChannel::Native,
    };
    app::run(init)?;

    drop(lock);
    Ok(())
}

/// The built DB's dictionary names, read-only; absent or unreadable is
/// simply an empty list (rebuilds are ticket 41's).
fn read_dicts(db: &Path) -> Vec<DictInfo> {
    let Ok(dictionary) = SqliteDictionary::open(db) else {
        return Vec::new();
    };
    dictionary.dicts().unwrap_or_default()
}
