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
mod autostart;
pub mod child;
mod rebuild;
mod snippets;
mod update;

mod channel;

use crate::lock::{self, LockError};
use crate::paths::{self, Paths};
use crate::shortcuts;
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

    let db_path = paths.data_dir.join("chibipop.sqlite");
    let library_dir = paths.data_dir.join("library");
    let dicts = read_dicts(&db_path);
    let form = chibipop::settings::from_config(&cfg, &dicts);
    let form = match Library::load(&library_dir) {
        Ok(lib) => chibipop::settings::with_library(form, &lib),
        // No library yet is the common fresh-install case; the lists
        // just come from the config's order.
        Err(_) => form,
    };

    let env = paths::Env::from_process();
    let init = app::Init {
        form,
        linux: apply::LinuxFields::from_config(&cfg),
        config_path: paths.config_file.clone(),
        socket_path: runtime_dir.join(control::file_name(&display)),
        log_path: paths.log_file(),
        compositor: snippets::Compositor::detect(),
        channel: hotkey_channel(&paths.state_dir),
        library_dir,
        db_path,
        runtime_dir: runtime_dir.to_path_buf(),
        autostart: autostart::Target::resolve(&env),
        home: env.home.clone(),
        // Resolved in this process, which is the same binary as the
        // daemon (`chibipop settings`), so the snippet names the exe
        // the user is actually running (ticket 51).
        exe: paths::exec_name(),
    };
    app::run(init)?;

    drop(lock);
    Ok(())
}

/// Who owns the trigger binding, as the daemon published it (ticket 36).
///
/// The portal control is only rendered when a daemon actually got the
/// GlobalShortcuts session *and* the bind through — never on a bus
/// probe. ADR-0005's rule is that the hotkey section cannot lie about
/// who owns the key, and "a portal exists on this machine" is not the
/// same fact as "the portal owns this binding": the frontend refuses
/// shortcut sessions to a launch with no app id, so a probe would print
/// a portal binding for a daemon that has none. No file, or a file
/// saying native, therefore means the compositor bind is the truth and
/// the snippet is what helps.
fn hotkey_channel(state_dir: &Path) -> channel::HotkeyChannel {
    match shortcuts::state::read(state_dir) {
        Some(published) if published.portal => channel::HotkeyChannel::Portal {
            current_binding: published.trigger_description(),
        },
        _ => channel::HotkeyChannel::Native,
    }
}

/// The built DB's dictionary names, read-only: what the daemon would see
/// right now. Absent or unreadable is simply an empty list — a fresh
/// install has no database until the first rebuild.
fn read_dicts(db: &Path) -> Vec<DictInfo> {
    let Ok(dictionary) = SqliteDictionary::open(db) else {
        return Vec::new();
    };
    dictionary.dicts().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcuts::state::Published;
    use crate::shortcuts::{Binding, ShortcutId};
    use channel::{HotkeyChannel, HotkeyControl};

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("chibipop_settings_channel_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The portal rung reaches the window with the key the portal named,
    /// and the window renders the rebind control instead of a snippet.
    #[test]
    fn a_published_portal_binding_becomes_the_portal_control() {
        let dir = scratch("portal");
        shortcuts::state::publish(
            &dir,
            &Published::portal(vec![Binding {
                id: ShortcutId::Trigger,
                trigger: Some("Alt+F".into()),
            }]),
        )
        .unwrap();
        let channel = hotkey_channel(&dir);
        assert_eq!(HotkeyChannel::Portal { current_binding: Some("Alt+F".into()) }, channel);
        assert_eq!(
            HotkeyControl::Rebind { current: Some("Alt+F".into()) },
            channel.control(snippets::Compositor::Kde, "ALT+F", Path::new("chibipop"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Bound but with no key reported (every Hyprland session): still the
    /// portal's binding, and the control says so rather than claiming a
    /// compositor snippet would help.
    #[test]
    fn a_portal_that_reports_no_key_is_still_the_portal_channel() {
        let dir = scratch("nokey");
        shortcuts::state::publish(
            &dir,
            &Published::portal(vec![Binding { id: ShortcutId::Trigger, trigger: None }]),
        )
        .unwrap();
        assert_eq!(HotkeyChannel::Portal { current_binding: None }, hotkey_channel(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The native rung, and a machine where no daemon has ever run, both
    /// show the snippet: the compositor bind is the only truth there is.
    #[test]
    fn the_native_rung_and_a_silent_daemon_both_show_the_snippet() {
        let dir = scratch("native");
        assert_eq!(HotkeyChannel::Native, hotkey_channel(&dir), "no file at all");
        shortcuts::state::publish(&dir, &Published::native()).unwrap();
        let channel = hotkey_channel(&dir);
        assert_eq!(HotkeyChannel::Native, channel);
        let HotkeyControl::Snippet { text } =
            channel.control(snippets::Compositor::Sway, "ALT+F", Path::new("/opt/cp/chibipop"))
        else {
            panic!("the native rung must render a snippet");
        };
        assert!(text.contains("/opt/cp/chibipop ctl trigger-down"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
