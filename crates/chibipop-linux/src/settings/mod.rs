//! `chibipop settings` is a separate `iced` process
//! (ARCHITECTURE.md#settings-and-config). A settings crash cannot stop live hover.
//!
//! Core `Config` and `SettingsForm` drive the window. This module owns only
//! widgets and process rules: the settings-scoped `flock`, the read-only Dictionary list,
//! and the save-then-`reload` Apply action. The window needs no extra Wayland globals
//! beyond those that a toplevel client uses. It opens when hover is unsupported or when
//! no daemon exists.

mod app;
mod apply;
mod autostart;
pub mod child;
mod filechooser;
mod rebuild;
pub(crate) mod snippets;
mod update;

mod channel;

use crate::lock::{self, LockError};
use crate::paths::{self, Paths};
use crate::shortcuts;
use crate::{clipboard, control, wayland};
use anyhow::{Context, Result};
use chibipop::library::Library;
use chibipop::lookup::model::Dictionary;
use chibipop::lookup::sqlite::SqliteDictionary;
use chibipop::present::DictInfo;
use std::path::Path;

pub fn run(paths: Paths) -> Result<()> {
    let display = wayland::display_name()?;
    let runtime_dir = paths.runtime_dir()?;

    // This settings-scoped `flock` differs from the daemon lock.
    // It permits one window per compositor instance.
    // The kernel releases it when the lock owner dies.
    let lock = match lock::acquire_at(runtime_dir, &lock::settings_file_name(&display)) {
        Ok(lock) => lock,
        Err(LockError::AlreadyRunning { path, pid }) => {
            // This is a notice, not an error. The requested window already exists.
            // The code does not raise the window across processes.
            // Compositors often ignore self-activation.
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
        // A fresh install often has no library.
        // In that case, the lists use the order from the configuration.
        Err(_) => form,
    };

    let env = paths::Env::from_process();
    // One published result supplies the owner and key for every shortcut row.
    let published = shortcuts::state::read(&paths.state_dir);
    let init = app::Init {
        form,
        linux: apply::LinuxFields::from_config(&cfg),
        config_path: paths.config_file.clone(),
        socket_path: runtime_dir.join(control::file_name(&display)),
        log_path: paths.log_file(),
        compositor: snippets::Compositor::detect(),
        shortcuts: published,
        state_dir: paths.state_dir.clone(),
        library_dir,
        db_path,
        dicts,
        runtime_dir: runtime_dir.to_path_buf(),
        autostart: autostart::Target::resolve(&env),
        home: env.home.clone(),
        // Resolve this name in the same binary as the daemon (`chibipop settings`).
        // The snippet names the executable that the user runs.
        exe: paths::exec_name(),
        // The compositor decides whether a client without focus can write the selection.
        // The daemon does not decide this. Ask the registry for the status.
        // The OCR-to-clipboard row must stay correct when no daemon exists.
        // Use one throwaway connection for one roundtrip.
        // The `chibipop probe` command uses the same check.
        // If the display is unreachable, report no rung.
        // This is the honest result for a Wayland protocol row.
        clipboard_rung: clipboard_rung(),
    };
    app::run(init)?;

    drop(lock);
    Ok(())
}

/// Return the data-control protocol that this session advertises for the
/// OCR-to-clipboard row.
///
/// The function opens a separate connection and makes one roundtrip.
/// It discards the connection after the roundtrip.
/// This process is already a Wayland client because `iced` owns a toplevel.
/// The registry gives the correct row when no daemon exists.
/// The function returns `None` when the display is unreachable or the roundtrip fails.
/// A Wayland protocol row cannot report more about a session it cannot inspect.
fn clipboard_rung() -> Option<clipboard::Rung> {
    let conn = wayland_client::Connection::connect_to_env().ok()?;
    clipboard::rung(&wayland::collect_globals(&conn).ok()?)
}

/// A session can accept only some requested shortcuts. Each row must use its
/// own result, because another action's registration does not bind this key.
fn hotkey_channel(
    published: Option<&shortcuts::state::Published>,
    id: &str,
) -> channel::HotkeyChannel {
    match published {
        Some(published) if published.portal && published.contains(id) => {
            channel::HotkeyChannel::Portal { current_binding: published.description(id) }
        }
        _ => channel::HotkeyChannel::Native,
    }
}

/// Return the Dictionary names from the built database.
/// Read-only access shows what the daemon would see now.
/// Return an empty list when the database is absent or unreadable.
/// A fresh install has no database before the first rebuild.
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
    use channel::HotkeyChannel;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("chibipop_settings_channel_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn channel_for(dir: &Path, id: &str) -> HotkeyChannel {
        hotkey_channel(shortcuts::state::read(dir).as_ref(), id)
    }

    #[test]
    fn a_published_dynamic_bind_uses_its_own_portal_key() {
        let dir = scratch("dynamic-portal");
        shortcuts::state::publish(
            &dir,
            &Published::portal(vec![Binding {
                id: ShortcutId::parse("bind-91").unwrap(),
                trigger: Some("Alt+F".into()),
            }]),
        )
        .unwrap();

        assert_eq!(
            HotkeyChannel::Portal { current_binding: Some("Alt+F".into()) },
            channel_for(&dir, "bind-91"),
        );
        assert_eq!(HotkeyChannel::Native, channel_for(&dir, "bind-92"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_portal_binding_without_a_key_is_still_confirmed() {
        let dir = scratch("dynamic-no-key");
        shortcuts::state::publish(
            &dir,
            &Published::portal(vec![Binding {
                id: ShortcutId::parse("bind-91").unwrap(),
                trigger: None,
            }]),
        )
        .unwrap();

        assert_eq!(
            HotkeyChannel::Portal { current_binding: None },
            channel_for(&dir, "bind-91"),
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

}
