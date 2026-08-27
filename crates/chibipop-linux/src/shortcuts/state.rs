//! What the settings window is allowed to say about the trigger binding:
//! a tiny file in the XDG state dir, written by the daemon, read by
//! `chibipop settings`.
//!
//! **Why a file and not a probe.** The settings process is a separate
//! process (ADR-0005) and cannot see the daemon's portal session — and
//! the session is where the truth lives. Probing the bus from the
//! settings window would answer a *different* question ("could a portal
//! be used?") and would let the UI claim the portal owns a binding on a
//! session where the bind actually failed, which is the one thing
//! ADR-0005 forbids: the hotkey control never lies about who owns the
//! key. So the daemon publishes what its own handshake resolved, and the
//! window renders that or falls back to the native snippet.
//!
//! **Why not the control socket.** The verb set is trigger transport, not
//! a scripting API (ADR-0003), and it is deliberately closed. A status
//! read is not a trigger, so it does not belong there.
//!
//! Absent is the normal case, not an error: it is what a fresh install
//! and a never-started daemon look like, and both mean "assume the
//! compositor owns the key", which is the answer that shows the user a
//! snippet they can act on.

use super::{Binding, ShortcutId};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The file name inside `Paths::state_dir`.
const FILE: &str = "trigger-channel";

/// What the daemon resolved for the trigger channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    /// Does the GlobalShortcuts portal own the binding right now?
    pub portal: bool,
    /// What the portal says is bound. Empty on the native rung, and also
    /// on a portal that bound nothing.
    pub bindings: Vec<Binding>,
}

impl Published {
    /// The native rung: the compositor bind is the only truth there is.
    pub fn native() -> Published {
        Published { portal: false, bindings: Vec::new() }
    }

    /// The portal rung, with whatever it reported.
    pub fn portal(bindings: Vec<Binding>) -> Published {
        Published { portal: true, bindings }
    }

    /// The key the settings window shows as "the current binding" for
    /// one action: the portal's own description of that shortcut, when
    /// it has one.
    ///
    /// `None` is the honest absence and covers three real cases — the
    /// native rung (no bindings at all), a portal that bound the id but
    /// reported no key, and an id the portal never answered for. All
    /// three mean the same thing to a row: we were not told a key, so
    /// do not name one.
    pub fn description(&self, id: ShortcutId) -> Option<String> {
        self.bindings
            .iter()
            .find(|binding| binding.id == id)
            .and_then(|binding| binding.trigger.clone())
    }

    /// One line per fact, `key value`. A hand-editable format for a
    /// file nobody should hand-edit is the point: this is a diagnostic
    /// as much as an IPC, and a human reading it should need no tool.
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

    /// The reverse. Unknown lines and unknown ids are skipped: a newer
    /// daemon writing a key this reader does not know must not turn the
    /// hotkey control into an error.
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
        // A bind line without the portal rung is nonsense; the channel
        // line is the authority.
        if !portal {
            bindings.clear();
        }
        Published { portal, bindings }
    }
}

pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE)
}

/// Publish, replacing whatever was there.
///
/// Written to a sibling temp file and renamed, so a settings window
/// reading while the daemon writes sees either the old file or the new
/// one, never half of one. Failure is returned rather than logged here:
/// the caller owns the log, and a status file that could not be written
/// is a diagnostic, never a reason to stop serving triggers.
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

/// What the last daemon run published, or `None` when nothing has.
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

    /// The whole contract: what the daemon knows is what the window
    /// reads, keys and all.
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

    /// Both published ids answer for themselves, and an id the portal
    /// never named answers with the honest absence rather than with the
    /// other row's key — the add-card row borrowing the trigger's key
    /// is exactly the lie ADR-0005 forbids this window.
    #[test]
    fn each_id_gets_its_own_description_and_an_unbound_id_gets_none() {
        let published = Published::portal(vec![
            Binding { id: ShortcutId::Trigger, trigger: Some("Alt+F".into()) },
            Binding { id: ShortcutId::AnkiAdd, trigger: Some("Alt+A".into()) },
        ]);
        assert_eq!(Some("Alt+F".to_string()), published.description(ShortcutId::Trigger));
        assert_eq!(Some("Alt+A".to_string()), published.description(ShortcutId::AnkiAdd));

        // Bound but unnamed (every xdph session), and never answered
        // for at all: one row shape, one answer.
        let partial = Published::portal(vec![Binding {
            id: ShortcutId::Trigger,
            trigger: Some("Alt+F".into()),
        }]);
        assert_eq!(None, partial.description(ShortcutId::AnkiAdd));
        let unnamed =
            Published::portal(vec![Binding { id: ShortcutId::AnkiAdd, trigger: None }]);
        assert_eq!(None, unnamed.description(ShortcutId::AnkiAdd));
    }

    /// A key with spaces in it (KDE spells chords that way) survives.
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

    /// The native rung publishes that it is native, and the window shows
    /// a snippet rather than claiming a portal binding.
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

    /// A rewrite replaces: a daemon that loses the portal rung must not
    /// leave yesterday's binding on screen.
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

    /// No daemon has ever run here: the window falls back, not fails.
    #[test]
    fn an_absent_file_is_no_answer_at_all() {
        let dir = scratch("absent");
        assert_eq!(None, read(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Garbage, a future key, and an id this build does not know are all
    /// survivable; the channel line is what decides.
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

    /// A file claiming bindings on the native rung is inconsistent, and
    /// the channel line wins: the UI must not show a portal key while
    /// telling the user to paste a compositor bind.
    #[test]
    fn bindings_without_the_portal_channel_are_dropped() {
        let parsed = Published::parse("channel native\nbind trigger ALT+F\n");
        assert_eq!(Published::native(), parsed);
    }
}
