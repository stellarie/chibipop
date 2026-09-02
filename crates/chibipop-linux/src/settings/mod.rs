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
mod snippets;
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
    // Read the state once for both rows. See `hotkey_channel`.
    let published = shortcuts::state::read(&paths.state_dir);
    let init = app::Init {
        form,
        linux: apply::LinuxFields::from_config(&cfg),
        config_path: paths.config_file.clone(),
        socket_path: runtime_dir.join(control::file_name(&display)),
        log_path: paths.log_file(),
        compositor: snippets::Compositor::detect(),
        channel: hotkey_channel(published.as_ref(), shortcuts::ShortcutId::Trigger),
        add_channel: hotkey_channel(published.as_ref(), shortcuts::ShortcutId::AnkiAdd),
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

/// Return the owner of one action bind from the daemon state.
///
/// Resolve each portal ID separately. The daemon requests both IDs in one session.
/// Its published state names each ID, so each row can render its own key.
/// The caller reads `published` once, so both rows use the same file state.
///
/// Render the portal control only after the daemon acquires the
/// `GlobalShortcuts` session and completes the bind.
/// Do not use a bus probe as proof.
/// The hotkey section must show the actual bind owner.
/// A portal on the machine does not prove that the portal owns this bind.
/// The frontend refuses a shortcut session when a launch has no app ID.
/// A probe would therefore show a portal bind for a daemon that has none.
/// If no file exists, or the file says native, treat the compositor bind as truth.
/// The snippet then helps the user create the compositor bind.
fn hotkey_channel(
    published: Option<&shortcuts::state::Published>,
    id: shortcuts::ShortcutId,
) -> channel::HotkeyChannel {
    match published {
        Some(published) if published.portal => {
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
    use channel::{HotkeyChannel, HotkeyControl};

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("chibipop_settings_channel_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// This mirrors `run`: it reads the file once and resolves one channel for each ID.
    fn channel_for(dir: &Path, id: ShortcutId) -> HotkeyChannel {
        hotkey_channel(shortcuts::state::read(dir).as_ref(), id)
    }

    /// The portal rung gives the window the key that the portal names.
    /// The window renders a rebind control instead of a snippet.
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
        let channel = channel_for(&dir, ShortcutId::Trigger);
        assert_eq!(HotkeyChannel::Portal { current_binding: Some("Alt+F".into()) }, channel);
        assert_eq!(
            HotkeyControl::Rebind { current: Some("Alt+F".into()) },
            channel.control(
                snippets::Compositor::Kde,
                "ALT+F",
                Path::new("chibipop"),
                snippets::Bind::Hold,
            )
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every Hyprland session can report a portal bind without a key.
    /// Keep the portal bind and show that state in the control.
    /// Do not claim that a compositor snippet can help.
    #[test]
    fn a_portal_that_reports_no_key_is_still_the_portal_channel() {
        let dir = scratch("nokey");
        shortcuts::state::publish(
            &dir,
            &Published::portal(vec![Binding { id: ShortcutId::Trigger, trigger: None }]),
        )
        .unwrap();
        assert_eq!(
            HotkeyChannel::Portal { current_binding: None },
            channel_for(&dir, ShortcutId::Trigger)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The native rung and a machine where no daemon has run both show the snippet.
    /// The compositor bind provides the only available status.
    #[test]
    fn the_native_rung_and_a_silent_daemon_both_show_the_snippet() {
        let dir = scratch("native");
        assert_eq!(
            HotkeyChannel::Native,
            channel_for(&dir, ShortcutId::Trigger),
            "no file at all"
        );
        shortcuts::state::publish(&dir, &Published::native()).unwrap();
        let channel = channel_for(&dir, ShortcutId::Trigger);
        assert_eq!(HotkeyChannel::Native, channel);
        let HotkeyControl::Snippet { text } = channel.control(
            snippets::Compositor::Sway,
            "ALT+F",
            Path::new("/opt/cp/chibipop"),
            snippets::Bind::Hold,
        )
        else {
            panic!("the native rung must render a snippet");
        };
        assert!(text.contains("/opt/cp/chibipop ctl trigger-down"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The add-card row keeps its own status.
    /// When the portal answers for `anki-add`, that row names *its* key.
    /// The trigger row names the trigger key. Two rows, two keys, one file.
    #[test]
    fn the_add_card_row_gets_the_key_the_portal_published_for_it() {
        let dir = scratch("addportal");
        shortcuts::state::publish(
            &dir,
            &Published::portal(vec![
                Binding { id: ShortcutId::Trigger, trigger: Some("Alt+F".into()) },
                Binding { id: ShortcutId::AnkiAdd, trigger: Some("Alt+A".into()) },
            ]),
        )
        .unwrap();
        assert_eq!(
            HotkeyChannel::Portal { current_binding: Some("Alt+F".into()) },
            channel_for(&dir, ShortcutId::Trigger)
        );
        assert_eq!(
            HotkeyChannel::Portal { current_binding: Some("Alt+A".into()) },
            channel_for(&dir, ShortcutId::AnkiAdd)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A portal can bind the trigger without an answer for `anki-add`.
    /// Keep the add row on the portal rung because the session owns it.
    /// Show no key instead of the trigger key.
    #[test]
    fn an_unanswered_add_id_is_still_the_portal_rung_with_no_key() {
        let dir = scratch("addsilent");
        shortcuts::state::publish(
            &dir,
            &Published::portal(vec![Binding {
                id: ShortcutId::Trigger,
                trigger: Some("Alt+F".into()),
            }]),
        )
        .unwrap();
        assert_eq!(
            HotkeyChannel::Portal { current_binding: None },
            channel_for(&dir, ShortcutId::AnkiAdd)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
