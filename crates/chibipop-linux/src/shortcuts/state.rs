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

    /// Return whether the portal reported this identifier at all.
    ///
    /// This is separate from [`description`]: a portal can confirm an id with
    /// no trigger description, so `None` must not make the settings row fall
    /// back to an unrelated binding.
    pub fn contains(&self, id: &str) -> bool {
        self.bindings.iter().any(|binding| binding.id.as_str() == id)
    }

    /// Return the key that the settings window shows for one action.
    /// Use the portal's description when it reports one.
    ///
    /// `None` means that no key was reported. This covers the native rung, a
    /// portal that bound the id without a key, and an id that the portal did not
    /// return. The row must not name a key in any of these cases.
    pub fn description(&self, id: &str) -> Option<String> {
        self.bindings
            .iter()
            .find(|binding| binding.id.as_str() == id)
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

    #[test]
    fn a_configured_portal_binding_round_trips_with_its_id() {
        let dir = scratch("portal");
        let published = Published::portal(vec![
            Binding { id: ShortcutId::parse("bind-1").unwrap(), trigger: Some("Alt+F".into()) },
            Binding { id: ShortcutId::parse("bind-2").unwrap(), trigger: None },
        ]);
        publish(&dir, &published).unwrap();

        let read_back = read(&dir).expect("published");
        assert_eq!(published, read_back);
        assert!(read_back.contains("bind-1"));
        assert!(read_back.contains("bind-2"));
        assert_eq!(Some("Alt+F".to_string()), read_back.description("bind-1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_bind_id_gets_its_own_description() {
        let published = Published::portal(vec![
            Binding { id: ShortcutId::parse("bind-1").unwrap(), trigger: Some("Alt+F".into()) },
            Binding { id: ShortcutId::parse("bind-2").unwrap(), trigger: Some("Alt+A".into()) },
        ]);
        assert_eq!(Some("Alt+F".to_string()), published.description("bind-1"));
        assert_eq!(Some("Alt+A".to_string()), published.description("bind-2"));
        assert_eq!(None, published.description("bind-3"));
        assert_eq!(
            None,
            Published::portal(vec![Binding {
                id: ShortcutId::parse("bind-1").unwrap(),
                trigger: None,
            }])
            .description("bind-1")
        );
    }

    #[test]
    fn a_multi_word_key_survives_the_round_trip() {
        let dir = scratch("spaces");
        publish(
            &dir,
            &Published::portal(vec![Binding {
                id: ShortcutId::parse("bind-1").unwrap(),
                trigger: Some("Meta + Shift + F".into()),
            }]),
        )
        .unwrap();
        assert_eq!(
            Some("Meta + Shift + F".to_string()),
            read(&dir).unwrap().description("bind-1")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_native_rung_publishes_no_binding() {
        let dir = scratch("native");
        publish(&dir, &Published::native()).unwrap();
        let read_back = read(&dir).expect("published");
        assert!(!read_back.portal);
        assert!(read_back.bindings.is_empty());
        assert_eq!(None, read_back.description("bind-1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn publishing_native_state_replaces_the_previous_portal_answer() {
        let dir = scratch("replace");
        publish(
            &dir,
            &Published::portal(vec![Binding {
                id: ShortcutId::parse("bind-1").unwrap(),
                trigger: Some("Alt+F".into()),
            }]),
        )
        .unwrap();
        publish(&dir, &Published::native()).unwrap();
        assert_eq!(Published::native(), read(&dir).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_absent_file_is_no_answer_at_all() {
        let dir = scratch("absent");
        assert_eq!(None, read(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_ids_and_unknown_lines_are_skipped() {
        let parsed = Published::parse(concat!(
            "channel portal\n",
            "bind bind-1 ALT+F\n",
            "bind bad/id CTRL+Z\n",
            "gibberish\n",
            "\n",
            "flavour vanilla\n",
        ));
        assert!(parsed.portal);
        assert_eq!(
            vec![Binding {
                id: ShortcutId::parse("bind-1").unwrap(),
                trigger: Some("ALT+F".into()),
            }],
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
