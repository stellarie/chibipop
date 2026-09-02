//! Apply = save-then-`reload` (ARCHITECTURE.md#settings-and-config).
//!
//! The config file is the sole source of truth: Apply writes the whole
//! struct, then sends one `reload` verb. The socket's presence *is* the
//! ApplyMode — connectable means the daemon hot-reloads, absent means
//! config-only with a notice. No structured settings ever cross the
//! socket.
//!
//! One thing sits between the save and the `reload`: a change to the
//! frequency inputs makes `term.freq` stale, and
//! [`chibipop::settings::dictionary_work`] is what says so. Reconciling it
//! is an in-place recompute over rows that are already here, never a
//! rebuild - see [`reindex`].

use crate::control::{self, Verb};
use anyhow::{Context, Result};
use chibipop::config::{Config, OcrClipboardConfig, PopupLayer, ScreenshotConfig};
use chibipop::library::Role;
use chibipop::present::DictInfo;
use chibipop::settings::{DictionaryWork, SettingsForm};
use rusqlite::OpenFlags;
use std::path::Path;

/// The fields the shared form does not model: Linux platform fields
/// (ARCHITECTURE.md#settings-and-config) plus the lookup-log gate,
/// edited straight on `Config`.
#[derive(Debug, Clone, PartialEq)]
pub struct LinuxFields {
    /// Advisory on the native channel; the portal binding later (36).
    pub trigger_key_linux: String,
    pub add_key_linux: String,
    /// The static-region chord. Native-channel only
    /// (ARCHITECTURE.md#input-ladders, 2026-08-26 addendum): nothing
    /// registers it with the portal, so it exists to be rendered as a
    /// copyable `ctl static-region` bind. Empty means no chord, exactly
    /// as `add_key_linux` does.
    pub static_region_key_linux: String,
    /// The mining screenshot's chord, native-channel only for the same
    /// reason as the static-region one above. `Option`, not a `String`
    /// with `""` for unbound: the config field is `Option<String>`
    /// precisely so absence stays typed (the Windows twin carries no such
    /// sentinel either), so the empty-text-box mapping lives at the UI
    /// edge and nowhere else.
    pub screenshot_key_linux: Option<String>,
    /// `actions.screenshot.save_dir` as typed. Where it resolves *to* is
    /// the daemon's (`Paths::screenshots_dir`), not this window's.
    pub screenshot_save_dir: String,
    /// The OCR-to-clipboard chord, native-channel only for the same
    /// reason as the two above. `Option` for the same reason
    /// `screenshot_key_linux` is: `actions.ocr_clipboard.hotkey_linux` is
    /// `Option<String>` precisely so absence stays typed, and the
    /// empty-text-box mapping lives at the UI edge.
    pub ocr_clipboard_key_linux: Option<String>,
    pub layer: PopupLayer,
    pub show_lookup_log: bool,
}

impl LinuxFields {
    pub fn from_config(cfg: &Config) -> LinuxFields {
        LinuxFields {
            trigger_key_linux: cfg.trigger.trigger_key_linux.clone(),
            add_key_linux: cfg.anki.add_key_linux.clone(),
            static_region_key_linux: cfg.anki.static_region_key_linux.clone(),
            screenshot_key_linux: cfg.actions.screenshot.hotkey_linux.clone(),
            screenshot_save_dir: cfg.actions.screenshot.save_dir.clone(),
            ocr_clipboard_key_linux: cfg
                .actions
                .ocr_clipboard
                .as_ref()
                .and_then(|action| action.hotkey_linux.clone()),
            layer: cfg.popup.layer,
            show_lookup_log: cfg.debug.show_lookup_log,
        }
    }

    pub fn apply_over(&self, cfg: &mut Config) {
        cfg.trigger.trigger_key_linux = self.trigger_key_linux.clone();
        cfg.anki.add_key_linux = self.add_key_linux.clone();
        cfg.anki.static_region_key_linux = self.static_region_key_linux.clone();
        cfg.actions.screenshot.hotkey_linux = self.screenshot_key_linux.clone();
        // The nested section carries *both* platforms' chords, so it may
        // only die when both are absent - the rule
        // `chibipop::settings::apply_to` already applies from the
        // Windows side (`an_unset_ocr_clipboard_key_keeps_the_linux_twin`),
        // read here off the config that call just wrote. Clearing this
        // box must not evict the Windows key with it.
        let windows_chord = cfg.actions.ocr_clipboard.as_ref().and_then(|a| a.hotkey.clone());
        cfg.actions.ocr_clipboard = match (windows_chord, self.ocr_clipboard_key_linux.clone()) {
            (None, None) => None,
            (hotkey, hotkey_linux) => Some(OcrClipboardConfig { hotkey, hotkey_linux }),
        };
        // A cleared box means the default folder, never the data dir
        // itself: a relative `save_dir` is joined onto that dir, so an
        // empty string would scatter PNGs among the database and the
        // dictionary archives.
        let dir = self.screenshot_save_dir.trim();
        cfg.actions.screenshot.save_dir = if dir.is_empty() {
            ScreenshotConfig::default().save_dir
        } else {
            dir.to_string()
        };
        cfg.popup.layer = self.layer;
        cfg.debug.show_lookup_log = self.show_lookup_log;
    }
}

/// Which mode Apply turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// The daemon took the `reload`; `reply` is its one line.
    Live { reply: String },
    /// No daemon: saved only.
    ConfigOnly,
}

/// What a change to the frequency inputs cost beyond the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frequency {
    /// Every Frequency rank recomputed in place, over this many `term`
    /// rows.
    Reindexed(u64),
    /// No database to restamp yet. A fresh install has no `term` row to
    /// carry a rank, and the first Rebuild reads these same settings, so
    /// there is nothing owed here.
    NoDatabase,
    /// The recompute failed. It is one transaction, so every rank is
    /// still the one the last build or reindex left - which also means
    /// the saved config now describes a ranking the database does not
    /// have, until the next Apply or Rebuild.
    Failed(String),
}

/// One Apply, start to finish.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub outcome: ApplyOutcome,
    /// Clamp notices `apply_to` produced, for the status area.
    pub notices: Vec<String>,
    /// `None` when the frequency inputs did not change, which is every
    /// Apply that edited something else.
    pub frequency: Option<Frequency>,
}

/// Save the whole struct, reconcile the ranks, then one `reload`.
///
/// `db` and `dicts` are only here for the reconcile: the database the
/// daemon reads, and the identities it holds, which is what turns the
/// config's exact names into the enabled frequency list.
pub fn apply(
    form: &SettingsForm,
    linux: &LinuxFields,
    config_path: &Path,
    socket_path: &Path,
    db: &Path,
    dicts: &[DictInfo],
) -> Result<Applied> {
    let cfg = chibipop::config::load_or_create(config_path)
        .with_context(|| format!("re-reading {}", config_path.display()))?;
    let mut out = chibipop::settings::apply_to(form, &cfg);
    linux.apply_over(&mut out);
    let notices = chibipop::settings::clamp_notice(form, &out).into_iter().collect();
    out.save(config_path)?;
    // The rule, before the `reload` that would otherwise hand the daemon
    // a config its database disagrees with: a strategy, order or
    // checkbox change recomputes `term.freq` in place from the Reported
    // frequencies already stored, and never re-reads an archive. Only a
    // staged library change rebuilds, and that is `super::rebuild`'s
    // path, reached from the Rebuild button rather than from here.
    let frequency = match chibipop::settings::dictionary_work(&cfg, &out) {
        DictionaryWork::None => None,
        DictionaryWork::Reindex => Some(reindex(db, &out, dicts)),
    };
    let outcome = match control::send_to(socket_path, Verb::Reload) {
        Ok(reply) => ApplyOutcome::Live { reply },
        Err(_) => ApplyOutcome::ConfigOnly,
    };
    Ok(Applied { outcome, notices, frequency })
}

/// Recompute every Frequency rank from what the database already stores.
///
/// A writer connection, because this is an `UPDATE` over the live file
/// and not a lookup - `SqliteDictionary::open` hands out a read-only one
/// on purpose. `SQLITE_OPEN_READ_WRITE` without `CREATE`, so a fresh
/// install answers [`Frequency::NoDatabase`] instead of having an empty
/// database conjured under the daemon's nose. No `journal_mode` pragma
/// either: the file a build promoted is already stamped WAL, which is
/// what lets the daemon's open snapshot keep reading the old ranking
/// until the commit and the new one afterwards.
///
/// The reindex reports a line per thousand rows and they are dropped
/// here: Apply is synchronous, so the window paints nothing while this
/// runs and there is no live surface to stream them to. The committed row
/// count is the report, and [`describe`] is where it is said.
fn reindex(db: &Path, after: &Config, dicts: &[DictInfo]) -> Frequency {
    if !db.exists() {
        return Frequency::NoDatabase;
    }
    let enabled = after.dictionaries.enabled(Role::Frequency, dicts);
    let done = rusqlite::Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .with_context(|| format!("opening {} to restamp its frequency ranks", db.display()))
        .and_then(|mut conn| {
            chibipop::dict::reindex::reindex(
                &mut conn,
                &enabled,
                after.dictionaries.ranking_strategy,
                &|_| {},
            )
        });
    match done {
        Ok(rows) => Frequency::Reindexed(rows),
        Err(e) => Frequency::Failed(format!("{e:#}")),
    }
}

/// The status line an outcome earns.
pub fn describe(applied: &Applied) -> String {
    let mut line = match &applied.outcome {
        ApplyOutcome::Live { reply } => format!("Saved; daemon reloaded ({reply})."),
        ApplyOutcome::ConfigOnly => {
            "Saved. The daemon is not running - settings take effect when it starts.".to_string()
        }
    };
    for notice in &applied.notices {
        line.push(' ');
        line.push_str(notice);
    }
    // A failure here is the one thing Apply can leave half-done: the file
    // is saved and the ranks are not, so it has to be said out loud
    // rather than folded into "Saved".
    match &applied.frequency {
        None => {}
        Some(Frequency::Reindexed(rows)) => {
            line.push_str(&format!(" Frequency rankings recomputed over {rows} term rows."));
        }
        Some(Frequency::NoDatabase) => {
            line.push_str(
                " There is no dictionary database yet - Rebuild reads the new frequency \
                 settings when it builds one.",
            );
        }
        Some(Frequency::Failed(why)) => {
            line.push_str(&format!(
                " The frequency rankings could not be recomputed, so they still come from the \
                 previous settings: {why}"
            ));
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use chibipop::lookup::model::Dictionary;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("chibipop_apply_test").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn form(cfg: &Config) -> SettingsForm {
        chibipop::settings::from_config(cfg, &[])
    }

    /// Apply against a database that is not there and no identities.
    ///
    /// Every test using this edits something other than the frequency
    /// inputs, so the reindex seam has nothing to reconcile and never
    /// reaches for a connection.
    fn saving(
        form: &SettingsForm,
        linux: &LinuxFields,
        config_path: &Path,
        socket_path: &Path,
    ) -> Result<Applied> {
        let db = config_path.with_file_name("chibipop.sqlite");
        apply(form, linux, config_path, socket_path, &db, &[])
    }

    #[test]
    fn without_a_socket_apply_is_config_only() {
        let dir = scratch("config_only");
        let config_path = dir.join("chibipop.toml");
        let cfg = chibipop::config::load_or_create(&config_path).unwrap();
        let mut linux = LinuxFields::from_config(&cfg);
        linux.show_lookup_log = true;

        let applied =
            saving(&form(&cfg), &linux, &config_path, &dir.join("absent.sock")).unwrap();

        assert_eq!(applied.outcome, ApplyOutcome::ConfigOnly);
        let saved = chibipop::config::load_or_create(&config_path).unwrap();
        assert!(saved.debug.show_lookup_log, "the flip must reach the file");
        assert!(describe(&applied).contains("daemon is not running"));
    }

    /// The nested `[actions.ocr_clipboard]` section carries both
    /// platforms' chords, so clearing the Linux box must not evict the
    /// Windows key with it - the mirror of core's
    /// `an_unset_ocr_clipboard_key_keeps_the_linux_twin`, from this side.
    #[test]
    fn clearing_the_linux_ocr_clipboard_chord_keeps_the_windows_twin() {
        let dir = scratch("ocrclip_twin");
        let config_path = dir.join("chibipop.toml");
        let mut cfg = chibipop::config::load_or_create(&config_path).unwrap();
        cfg.actions.ocr_clipboard = Some(OcrClipboardConfig {
            hotkey: Some("f9".to_string()),
            hotkey_linux: Some("ALT+C".to_string()),
        });
        cfg.save(&config_path).unwrap();

        let cfg = chibipop::config::load_or_create(&config_path).unwrap();
        let mut linux = LinuxFields::from_config(&cfg);
        assert_eq!(Some("ALT+C".to_string()), linux.ocr_clipboard_key_linux, "read in as typed");
        // The window's cleared text box, as `Message::OcrClipboardKey`
        // maps it: absence, never an empty string.
        linux.ocr_clipboard_key_linux = None;

        saving(&form(&cfg), &linux, &config_path, &dir.join("absent.sock")).unwrap();

        assert_eq!(
            Some(OcrClipboardConfig { hotkey: Some("f9".to_string()), hotkey_linux: None }),
            chibipop::config::load_or_create(&config_path).unwrap().actions.ocr_clipboard,
            "the Windows chord survives a Linux Apply that cleared the Linux one"
        );
    }

    /// And with neither chord the section goes away rather than being
    /// written as a table full of `None`: absence stays absence
    /// (ARCHITECTURE.md#settings-and-config).
    #[test]
    fn an_ocr_clipboard_section_with_neither_chord_is_dropped() {
        let dir = scratch("ocrclip_dropped");
        let config_path = dir.join("chibipop.toml");
        let mut cfg = chibipop::config::load_or_create(&config_path).unwrap();
        cfg.actions.ocr_clipboard =
            Some(OcrClipboardConfig { hotkey: None, hotkey_linux: Some("ALT+C".to_string()) });
        cfg.save(&config_path).unwrap();

        let cfg = chibipop::config::load_or_create(&config_path).unwrap();
        let mut linux = LinuxFields::from_config(&cfg);
        linux.ocr_clipboard_key_linux = None;

        saving(&form(&cfg), &linux, &config_path, &dir.join("absent.sock")).unwrap();

        assert_eq!(
            None,
            chibipop::config::load_or_create(&config_path).unwrap().actions.ocr_clipboard
        );
    }

    /// The other direction: a chord typed into an empty config creates
    /// the section, and a later reload reads it back.
    #[test]
    fn a_typed_ocr_clipboard_chord_creates_the_section_and_round_trips() {
        let dir = scratch("ocrclip_typed");
        let config_path = dir.join("chibipop.toml");
        let cfg = chibipop::config::load_or_create(&config_path).unwrap();
        assert_eq!(None, cfg.actions.ocr_clipboard, "a default config has no section");
        let mut linux = LinuxFields::from_config(&cfg);
        linux.ocr_clipboard_key_linux = Some("ALT+C".to_string());

        saving(&form(&cfg), &linux, &config_path, &dir.join("absent.sock")).unwrap();

        let saved = chibipop::config::load_or_create(&config_path).unwrap();
        assert_eq!(
            Some(OcrClipboardConfig { hotkey: None, hotkey_linux: Some("ALT+C".to_string()) }),
            saved.actions.ocr_clipboard
        );
        assert_eq!(
            Some("ALT+C".to_string()),
            LinuxFields::from_config(&saved).ocr_clipboard_key_linux
        );
    }

    #[test]
    fn with_a_socket_apply_sends_exactly_one_reload() {
        let dir = scratch("live");
        let config_path = dir.join("chibipop.toml");
        let socket_path = dir.join("run.sock");
        let cfg = chibipop::config::load_or_create(&config_path).unwrap();

        let listener = UnixListener::bind(&socket_path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut stream = reader.into_inner();
            stream.write_all(b"OK reload\n").unwrap();
            line
        });

        let applied =
            saving(&form(&cfg), &LinuxFields::from_config(&cfg), &config_path, &socket_path)
                .unwrap();

        assert_eq!(applied.outcome, ApplyOutcome::Live { reply: "OK reload".into() });
        assert_eq!(server.join().unwrap(), "reload\n", "one verb, nothing else");
        assert!(describe(&applied).contains("daemon reloaded"));
    }

    #[test]
    fn linux_fields_ride_along_and_round_trip() {
        let dir = scratch("linux_fields");
        let config_path = dir.join("chibipop.toml");
        let cfg = chibipop::config::load_or_create(&config_path).unwrap();
        let linux = LinuxFields {
            trigger_key_linux: "CTRL+SHIFT+K".into(),
            add_key_linux: "ALT+B".into(),
            static_region_key_linux: "ALT+R".into(),
            screenshot_key_linux: Some("SUPER+S".into()),
            screenshot_save_dir: "shots".into(),
            ocr_clipboard_key_linux: Some("SUPER+C".into()),
            layer: PopupLayer::Top,
            show_lookup_log: true,
        };

        saving(&form(&cfg), &linux, &config_path, &dir.join("absent.sock")).unwrap();

        let saved = chibipop::config::load_or_create(&config_path).unwrap();
        assert_eq!(LinuxFields::from_config(&saved), linux);
        // The Windows twins survived the whole-struct save untouched.
        assert_eq!(saved.trigger.trigger_key, cfg.trigger.trigger_key);
        assert_eq!(saved.anki.add_key, cfg.anki.add_key);
        assert_eq!(saved.anki.static_region_key, cfg.anki.static_region_key);
        assert_eq!(saved.actions.screenshot.hotkey, cfg.actions.screenshot.hotkey);
        assert_eq!(saved.ocr.language, cfg.ocr.language);
    }

    #[test]
    fn clamp_notices_surface_in_the_description() {
        let dir = scratch("clamp");
        let config_path = dir.join("chibipop.toml");
        let cfg = chibipop::config::load_or_create(&config_path).unwrap();
        let mut f = form(&cfg);
        f.capture_width = 5; // below the 100px floor

        let applied = saving(&f, &LinuxFields::from_config(&cfg), &config_path, &dir.join("no"))
            .unwrap();

        assert_eq!(applied.notices.len(), 1);
        assert!(describe(&applied).contains("raised to the 100px minimum"));
    }

    /// The inode the daemon's open handle is reading, so "in place" is
    /// observed rather than argued: a rebuild renames, and this must not.
    fn inode(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).unwrap().ino()
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/yomitan")
            .join(name)
    }

    /// A real database with one term dictionary and one frequency
    /// dictionary, and a config that names the frequency one as enabled.
    fn built(dir: &Path) -> (PathBuf, PathBuf, Vec<DictInfo>) {
        let db = dir.join("chibipop.sqlite");
        chibipop::dict::build::build(
            &[fixture("terms.zip"), fixture("freq.zip")],
            &[fixture("freq.zip")],
            &db,
            &|_| {},
        )
        .unwrap();
        let dicts = chibipop::lookup::sqlite::SqliteDictionary::open(&db).unwrap().dicts().unwrap();
        let config_path = dir.join("chibipop.toml");
        let mut cfg = chibipop::config::load_or_create(&config_path).unwrap();
        cfg.dictionaries.frequency = vec!["FixtureFreq".to_string()];
        cfg.save(&config_path).unwrap();
        (config_path, db, dicts)
    }

    fn ranked(db: &Path) -> i64 {
        rusqlite::Connection::open(db)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM term WHERE freq IS NOT NULL", [], |r| r.get(0))
            .unwrap()
    }

    /// Unchecking a frequency dictionary is a settings change, and a
    /// settings change recomputes `term.freq` in place. Nothing here
    /// re-reads an archive or builds a file beside the live one - that is
    /// the Rebuild button's path, and this one must never take it.
    #[test]
    fn unchecking_a_frequency_dictionary_restamps_the_ranks_in_place() {
        let dir = scratch("reindex");
        let (config_path, db, dicts) = built(&dir);
        let cfg = chibipop::config::load_or_create(&config_path).unwrap();
        let mut form = chibipop::settings::from_config(&cfg, &dicts);
        assert!(ranked(&db) > 0, "the build stamped the ranks it is about to lose");
        let was = inode(&db);

        form.frequency.iter_mut().for_each(|row| row.enabled = false);
        let applied = apply(
            &form,
            &LinuxFields::from_config(&cfg),
            &config_path,
            &dir.join("absent.sock"),
            &db,
            &dicts,
        )
        .unwrap();

        let Some(Frequency::Reindexed(rows)) = applied.frequency else {
            panic!("a frequency change owes a reindex: {:?}", applied.frequency);
        };
        assert!(rows > 0, "every term row is restamped");
        assert_eq!(0, ranked(&db), "no dictionary is enabled, so no term carries a rank");
        assert_eq!(was, inode(&db), "in place, never a rename");
        assert!(describe(&applied).contains("Frequency rankings recomputed"), "{}", describe(&applied));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// And an Apply that touched something else owes nothing: opening a
    /// writer on the live database on every Save would be a cost with no
    /// reason, and rewriting every rank to the value it already has would
    /// be worse.
    #[test]
    fn an_apply_that_left_the_frequency_inputs_alone_does_no_reindex() {
        let dir = scratch("no_reindex");
        let (config_path, db, dicts) = built(&dir);
        let cfg = chibipop::config::load_or_create(&config_path).unwrap();
        let mut form = chibipop::settings::from_config(&cfg, &dicts);
        let before = ranked(&db);
        let mut linux = LinuxFields::from_config(&cfg);
        linux.show_lookup_log = true;
        form.summary_chars = 120;

        let applied = apply(
            &form,
            &linux,
            &config_path,
            &dir.join("absent.sock"),
            &db,
            &dicts,
        )
        .unwrap();

        assert_eq!(None, applied.frequency);
        assert_eq!(before, ranked(&db));
        assert!(!describe(&applied).contains("Frequency"), "{}", describe(&applied));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fresh install has no database to restamp. Saying so beats
    /// conjuring an empty one under the daemon's nose, which is what a
    /// plain `Connection::open` would have done.
    #[test]
    fn a_frequency_change_with_no_database_yet_says_so_and_creates_nothing() {
        let dir = scratch("reindex_no_db");
        let config_path = dir.join("chibipop.toml");
        let db = dir.join("chibipop.sqlite");
        let cfg = chibipop::config::load_or_create(&config_path).unwrap();
        let mut form = chibipop::settings::from_config(&cfg, &[]);
        form.ranking_strategy = chibipop::dict::frequency::RankingStrategy::Priority;

        let applied = apply(
            &form,
            &LinuxFields::from_config(&cfg),
            &config_path,
            &dir.join("absent.sock"),
            &db,
            &[],
        )
        .unwrap();

        assert_eq!(Some(Frequency::NoDatabase), applied.frequency);
        assert!(!db.exists(), "nothing may create the database the daemon reads");
        let saved = chibipop::config::load_or_create(&config_path).unwrap();
        assert_eq!(
            chibipop::dict::frequency::RankingStrategy::Priority,
            saved.dictionaries.ranking_strategy,
            "the setting still reaches the file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
