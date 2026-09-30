//! Apply saves the config, then sends `reload`
//! (ARCHITECTURE.md#settings-and-config).
//!
//! The config file holds the only truth. Apply writes the whole
//! struct, then sends one `reload` verb. The presence of the socket
//! *is* the ApplyMode. A connectable socket means the daemon reloads
//! at once. An absent socket means Apply writes the config and shows a
//! notice. No structured settings cross the socket.
//!
//! One step sits between the save and the `reload`. A change to the
//! frequency inputs makes `term.freq` stale, and
//! [`chibipop::settings::dictionary_work`] reports that fact. Apply
//! reconciles the ranks with an in-place recompute over the rows that
//! the database already holds. Apply never rebuilds. See [`reindex`].

use crate::control::{self, Verb};
use anyhow::{Context, Result};
use chibipop::config::{Config, ResolvedConfig};
use chibipop::library::Role;
use chibipop::settings::{DictionaryWork, SettingsForm};
use rusqlite::OpenFlags;
use std::path::Path;

/// Linux settings that do not belong to a profile.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LinuxFields {
    pub show_lookup_log: bool,
}

impl LinuxFields {
    pub fn from_config(cfg: &Config) -> LinuxFields {
        LinuxFields { show_lookup_log: cfg.debug.show_lookup_log }
    }

    pub fn apply_over(&self, cfg: &mut Config) {
        cfg.debug.show_lookup_log = self.show_lookup_log;
    }
}

/// The mode that Apply reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// The daemon took the `reload`. The field `reply` holds its one line.
    Live { reply: String },
    /// No daemon answered. Apply only saved the config.
    ConfigOnly,
}

/// The extra work that a change to the frequency inputs needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frequency {
    /// The recompute stamped every Frequency rank in place, over this
    /// many `term` rows.
    Reindexed(u64),
    /// No database exists yet. A fresh install has no `term` row that
    /// carries a rank, and the first Rebuild reads these same settings.
    /// This state owes no work.
    NoDatabase,
    /// The recompute failed. It runs as one transaction, so every rank
    /// keeps the value that the last build or reindex left. The saved
    /// config therefore describes a ranking that the database does not
    /// hold, until the next Apply or Rebuild.
    Failed(String),
}

/// One Apply, start to finish.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub outcome: ApplyOutcome,
    pub settings: chibipop::settings::AppliedSettings,
    /// The clamp notices that `apply_to` produced, for the status area.
    pub notices: Vec<String>,
    /// `None` when the frequency inputs did not change. Every Apply
    /// that edits another field leaves this field `None`.
    pub frequency: Option<Frequency>,
}

/// Save the whole struct, reconcile the ranks, then send one `reload`.
///
/// The reconcile needs the database, and nothing else does. The config already
/// contains the exact frequency names for the selected profile.
pub fn apply(
    form: &SettingsForm,
    linux: &LinuxFields,
    config_path: &Path,
    socket_path: &Path,
    db: &Path,
) -> Result<Applied> {
    let saved = chibipop::config::load_or_create(config_path)
        .with_context(|| format!("re-reading {}", config_path.display()))?;
    let mut settings = chibipop::settings::apply_to(form, &saved);
    let out = &mut settings.config;
    if linux.show_lookup_log != form.catalog.debug.show_lookup_log {
        linux.apply_over(out);
    }
    out.validate_hotkeys(chibipop::config::Platform::Linux)?;
    let after = out.resolved(Some(&settings.profile_id))?;
    let notices = chibipop::settings::clamp_notice(form, &after).into_iter().collect();
    out.save(config_path)?;
    // Apply recomputes frequency ranks from values that the database already
    // holds. It never reads an archive.
    let frequency = match chibipop::settings::dictionary_work(&saved, out) {
        DictionaryWork::None => None,
        DictionaryWork::Reindex => Some(reindex(db, &after)),
    };
    let outcome = match control::send_to(socket_path, Verb::Reload) {
        Ok(reply) => ApplyOutcome::Live { reply },
        Err(_) => ApplyOutcome::ConfigOnly,
    };
    Ok(Applied { outcome, settings, notices, frequency })
}

/// Recompute every Frequency rank from the values that the database
/// already stores.
///
/// This function opens a writer connection, because it runs an `UPDATE`
/// over the live file and not a lookup. `SqliteDictionary::open`
/// returns a read-only connection on purpose. The flags are
/// `SQLITE_OPEN_READ_WRITE` without `CREATE`, so a fresh install
/// answers [`Frequency::NoDatabase`] and no empty database appears
/// under the daemon. This function sets no `journal_mode` pragma. A
/// build already stamped the promoted file WAL, which lets the open
/// snapshot of the daemon read the old ranking until the commit, and
/// the new ranking after it.
///
/// The reindex reports one line for each thousand rows, and this
/// function drops those lines. Apply runs synchronously, so the window
/// paints nothing during the recompute and no live surface can show
/// them. The committed row count is the report, and [`describe`] states
/// it.
fn reindex(db: &Path, after: &ResolvedConfig) -> Frequency {
    if !db.exists() {
        return Frequency::NoDatabase;
    }
    let enabled = after.dictionaries.enabled(Role::Frequency);
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
    use chibipop::present::DictInfo;
    use chibipop::config::Config;
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
        apply(form, linux, config_path, socket_path, &db)
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
    }

    #[test]
    fn linux_apply_preserves_the_latest_configured_bind_chords() {
        let dir = scratch("bind_preserve");
        let config_path = dir.join("chibipop.toml");
        let mut initial = chibipop::config::load_or_create(&config_path).unwrap();
        initial.binds.clear();
        let mut bind = chibipop::config::Bind::new(
            "bind-17".into(),
            chibipop::config::BindAction::Lookup,
        );
        bind.windows = "CTRL+SHIFT+L".into();
        bind.linux = "ALT+F".into();
        initial.binds.push(bind);
        initial.save(&config_path).unwrap();

        let cfg = chibipop::config::load_or_create(&config_path).unwrap();
        let form = form(&cfg);
        let mut latest = cfg.clone();
        latest.binds[0].windows = "ALT+Q".into();
        latest.binds[0].linux = "CTRL+Q".into();
        latest.save(&config_path).unwrap();

        let mut linux = LinuxFields::from_config(&cfg);
        linux.show_lookup_log = true;
        saving(&form, &linux, &config_path, &dir.join("absent.sock")).unwrap();

        let saved = chibipop::config::load_or_create(&config_path).unwrap();
        assert!(saved.debug.show_lookup_log);
        assert_eq!("ALT+Q", saved.binds[0].windows);
        assert_eq!("CTRL+Q", saved.binds[0].linux);
        let _ = std::fs::remove_dir_all(&dir);
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
    fn linux_debug_setting_saves_with_the_catalog() {
        let dir = scratch("linux_debug");
        let config_path = dir.join("chibipop.toml");
        let cfg = chibipop::config::load_or_create(&config_path).unwrap();
        let linux = LinuxFields { show_lookup_log: true };

        saving(&form(&cfg), &linux, &config_path, &dir.join("absent.sock")).unwrap();

        let saved = chibipop::config::load_or_create(&config_path).unwrap();
        assert_eq!(LinuxFields::from_config(&saved), linux);
        assert_eq!(saved.default_profile, cfg.default_profile);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clamp_notices_surface_in_the_description() {
        let dir = scratch("clamp");
        let config_path = dir.join("chibipop.toml");
        let cfg = chibipop::config::load_or_create(&config_path).unwrap();
        let mut f = form(&cfg);
        f.cfg.ocr.capture_width = 5; // below the 100px floor

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
        form.cfg.popup.summary_chars = 120;

        let applied = apply(
            &form,
            &linux,
            &config_path,
            &dir.join("absent.sock"),
            &db,
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
        form.cfg.dictionaries.ranking_strategy =
            chibipop::dict::frequency::RankingStrategy::Priority;

        let applied = apply(
            &form,
            &LinuxFields::from_config(&cfg),
            &config_path,
            &dir.join("absent.sock"),
            &db,
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

