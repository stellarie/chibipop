//! The daemon publishes trigger binding state in an XDG state file.
//! `chibipop settings` reads this file.
//!
//! **Why a file instead of a probe.** The settings process is separate
//! (ARCHITECTURE.md#settings-and-config). It cannot access the daemon's
//! portal session, which contains the binding state. A bus probe answers
//! whether a portal can serve, not whether the portal owns a binding.
//! The settings window must show the owner of the key. The daemon publishes
//! the result of its own session setup. The window renders that result or
//! shows the native binding snippet.
//!
//! **Why not the control socket.** The control socket carries trigger
//! transport, not a scripting API (ARCHITECTURE.md#input-ladders), and its
//! verb set is closed. A status read is not a trigger, so it does not belong
//! on that socket.
//!
//! An absent file is normal. It means that no daemon has published state.
//! The settings window then assumes that the compositor owns the key and
//! shows a snippet that the user can apply.

use super::{Binding, ShortcutId};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The file name inside `Paths::state_dir`.
const FILE: &str = "trigger-channel";

/// State that the daemon resolved for the trigger channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    /// True when the GlobalShortcuts portal owns the binding.
    pub portal: bool,
    /// Bindings that the portal reports. This list is empty on the native rung
    /// and when the portal binds no shortcut.
    pub bindings: Vec<Binding>,
}

impl Published {
    /// Return state for the native rung. The compositor bind is the only
    /// binding source.
    pub fn native() -> Published {
        Published { portal: false, bindings: Vec::new() }
    }

    /// Return state for the portal rung with its reported bindings.
    pub fn portal(bindings: Vec<Binding>) -> Published {
        Published { portal: true, bindings }
    }

    /// Return the key that the settings window shows for one action.
    /// Use the portal's description when it reports one.
    ///
    /// `None` means that no key was reported. This covers the native rung, a
    /// portal that bound the id without a key, and an id that the portal did not
    /// return. The row must not name a key in any of these cases.
    pub fn description(&self, id: ShortcutId) -> Option<String> {
        self.bindings
            .iter()
            .find(|binding| binding.id == id)
            .and_then(|binding| binding.trigger.clone())
    }

    /// Render one line for each fact as `key value`. The format stays readable
    /// for a person and carries diagnostic state between processes.
    fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(if self.portal { "channel portal\n" } else { "channel native\n" });
        for binding in &self.bindings {
            match &binding.trigger {
                Some(trigger) => {
                    out.push_str(&format!("bind {} {trigger}\n", binding.id.as_str()));
                }
                None => out.push_str(&format!("bind {}\n", binding.id.as_str())),
            }
        }
        out
    }

    /// Parse the file. Skip unknown lines and ids so a newer daemon does not
    /// turn the settings window into an error.
    fn parse(text: &str) -> Published {
        let mut portal = false;
        let mut bindings = Vec::new();
        for line in text.lines() {
            let mut words = line.split_whitespace();
            match (words.next(), words.next()) {
                (Some("channel"), Some("portal")) => portal = true,
                (Some("bind"), Some(id)) => {
                    let Some(id) = ShortcutId::parse(id) else { continue };
                    let rest: Vec<&str> = words.collect();
                    let trigger = (!rest.is_empty()).then(|| rest.join(" "));
                    bindings.push(Binding { id, trigger });
                }
                _ => {}
            }
        }
        // A bind line without the portal rung is invalid. The channel line is
        // authoritative.
        if !portal {
            bindings.clear();
        }
        Published { portal, bindings }
    }
}

pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE)
}

/// Publish a new state and replace the old file.
///
/// Write a sibling temporary file and rename it. A settings window that
/// reads during the write sees the old file or the new file, never a partial
/// file. Return failures to the caller. The caller owns the log. A failed
/// state write is diagnostic and must not stop trigger service.
pub fn publish(state_dir: &Path, published: &Published) -> std::io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let final_path = path(state_dir);
    let temp = final_path.with_extension("tmp");
    {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(published.render().as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&temp, &final_path)
}

/// Read the state from the last daemon run. Return `None` when no file exists.
pub fn read(state_dir: &Path) -> Option<Published> {
    let text = std::fs::read_to_string(path(state_dir)).ok()?;
    Some(Published::parse(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("chibipop_trigger_state_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// This test checks the full contract. The settings window reads every
    /// binding that the daemon publishes.
    #[test]
    fn a_portal_binding_round_trips_to_the_settings_window() {
        let dir = scratch("portal");
        let published = Published::portal(vec![
            Binding { id: ShortcutId::Trigger, trigger: Some("Alt+F".into()) },
            Binding { id: ShortcutId::AnkiAdd, trigger: None },
        ]);
        publish(&dir, &published).unwrap();

        let read_back = read(&dir).expect("published");
        assert_eq!(published, read_back);
        assert_eq!(Some("Alt+F".to_string()), read_back.description(ShortcutId::Trigger));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Each id returns its own description. An id that the portal did not name
    /// returns `None` instead of another id's key.
    #[test]
    fn each_id_gets_its_own_description_and_an_unbound_id_gets_none() {
        let published = Published::portal(vec![
            Binding { id: ShortcutId::Trigger, trigger: Some("Alt+F".into()) },
            Binding { id: ShortcutId::AnkiAdd, trigger: Some("Alt+A".into()) },
        ]);
        assert_eq!(Some("Alt+F".to_string()), published.description(ShortcutId::Trigger));
        assert_eq!(Some("Alt+A".to_string()), published.description(ShortcutId::AnkiAdd));

        // A bound id can have no key, and the portal can omit an id.
        // Both cases return `None`.
        let partial = Published::portal(vec![Binding {
            id: ShortcutId::Trigger,
            trigger: Some("Alt+F".into()),
        }]);
        assert_eq!(None, partial.description(ShortcutId::AnkiAdd));
        let unnamed =
            Published::portal(vec![Binding { id: ShortcutId::AnkiAdd, trigger: None }]);
        assert_eq!(None, unnamed.description(ShortcutId::AnkiAdd));
    }

    /// Preserve a key that contains spaces. KDE uses this spelling for chords.
    #[test]
    fn a_multi_word_key_survives_the_round_trip() {
        let dir = scratch("spaces");
        publish(
            &dir,
            &Published::portal(vec![Binding {
                id: ShortcutId::Trigger,
                trigger: Some("Meta + Shift + F".into()),
            }]),
        )
        .unwrap();
        assert_eq!(
            Some("Meta + Shift + F".to_string()),
            read(&dir).unwrap().description(ShortcutId::Trigger)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The native rung publishes native state. The settings window shows a
    /// snippet instead of a portal binding.
    #[test]
    fn the_native_rung_publishes_no_binding() {
        let dir = scratch("native");
        publish(&dir, &Published::native()).unwrap();
        let read_back = read(&dir).expect("published");
        assert!(!read_back.portal);
        assert!(read_back.bindings.is_empty());
        assert_eq!(None, read_back.description(ShortcutId::Trigger));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A later publish replaces the previous state. A daemon that loses the
    /// portal rung must not leave the old binding on screen.
    #[test]
    fn publishing_again_replaces_the_previous_answer() {
        let dir = scratch("replace");
        publish(
            &dir,
            &Published::portal(vec![Binding {
                id: ShortcutId::Trigger,
                trigger: Some("Alt+F".into()),
            }]),
        )
        .unwrap();
        publish(&dir, &Published::native()).unwrap();
        assert_eq!(Published::native(), read(&dir).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An absent file means that no daemon has published state. The settings
    /// window uses the native fallback.
    #[test]
    fn an_absent_file_is_no_answer_at_all() {
        let dir = scratch("absent");
        assert_eq!(None, read(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Unknown lines and ids do not stop parsing. The channel line controls
    /// whether bindings remain.
    #[test]
    fn unknown_lines_and_ids_are_skipped() {
        let parsed = Published::parse(concat!(
            "channel portal\n",
            "bind trigger ALT+F\n",
            "bind future-thing CTRL+Z\n",
            "gibberish\n",
            "\n",
            "flavour vanilla\n",
        ));
        assert!(parsed.portal);
        assert_eq!(
            vec![Binding { id: ShortcutId::Trigger, trigger: Some("ALT+F".into()) }],
            parsed.bindings
        );
    }

    /// Bindings without the portal channel are invalid. The channel line wins.
    /// The window must not show a portal key with a native bind snippet.
    #[test]
    fn bindings_without_the_portal_channel_are_dropped() {
        let parsed = Published::parse("channel native\nbind trigger ALT+F\n");
        assert_eq!(Published::native(), parsed);
    }
}
