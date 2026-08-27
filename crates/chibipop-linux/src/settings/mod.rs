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
    // One read, two rows: see `hotkey_channel`.
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
        runtime_dir: runtime_dir.to_path_buf(),
        autostart: autostart::Target::resolve(&env),
        home: env.home.clone(),
        // Resolved in this process, which is the same binary as the
        // daemon (`chibipop settings`), so the snippet names the exe
        // the user is actually running (ticket 51).
        exe: paths::exec_name(),
        // Whether a focus-less client can write the selection here is a
        // fact about the compositor, not about the daemon, so this
        // window asks the registry itself rather than reading a status
        // the daemon published: the OCR-to-clipboard row has to be right
        // on a machine where no daemon is running at all. One roundtrip
        // on a throwaway connection - the same probe `chibipop probe`
        // prints. A display we cannot reach is simply no rung, which is
        // the honest answer for a row about a Wayland protocol.
        clipboard_rung: clipboard_rung(),
    };
    app::run(init)?;

    drop(lock);
    Ok(())
}

/// Which data-control protocol this session advertises, for the
/// OCR-to-clipboard row.
///
/// A connection and one roundtrip of its own, thrown away immediately:
/// this process is already a Wayland client (iced owns a toplevel), and
/// asking the registry is what makes the row true with no daemon
/// running. Unreachable display or a failed roundtrip is `None` - a row
/// about a Wayland protocol has nothing else to say about a session it
/// cannot see.
fn clipboard_rung() -> Option<clipboard::Rung> {
    let conn = wayland_client::Connection::connect_to_env().ok()?;
    clipboard::rung(&wayland::collect_globals(&conn).ok()?)
}

/// Who owns one action's binding, as the daemon published it (ticket 36).
///
/// Resolved per portal id (ticket 09): the daemon requests both ids in
/// one session, so its published answer names each one separately and a
/// row can render its own key without borrowing another's. `published`
/// is read once by the caller so the two rows can never disagree about
/// which file they read.
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

    /// What `run` does: read the file once, resolve one channel per id.
    fn channel_for(dir: &Path, id: ShortcutId) -> HotkeyChannel {
        hotkey_channel(shortcuts::state::read(dir).as_ref(), id)
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
        assert_eq!(
            HotkeyChannel::Portal { current_binding: None },
            channel_for(&dir, ShortcutId::Trigger)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The native rung, and a machine where no daemon has ever run, both
    /// show the snippet: the compositor bind is the only truth there is.
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

    /// The add-card row's own status (ticket 09): when the portal
    /// answered for `anki-add`, that row names *its* key and the
    /// trigger row names the trigger's - two rows, two keys, one file.
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

    /// A portal that bound the trigger and never answered for the add:
    /// the add row is still on the portal rung (the session owns it),
    /// but it names no key rather than the trigger's.
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
