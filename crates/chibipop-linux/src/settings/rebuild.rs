//! The rebuild, in this process (ARCHITECTURE.md#settings-and-config).
//!
//! Windows spawns `chibipop.exe build-dict` as a child with
//! `CREATE_NO_WINDOW` and reads its stdout; on Linux there is no window
//! to suppress and no reason for a second process, so the same core
//! builder runs on a background thread here and its progress lines go
//! straight down a channel. `chibipop::dict::build::build` owns the whole
//! crash-safety story: it builds into `<out>.building`, reads the finished
//! file back before anything depends on it, clears the sidecars the
//! database being replaced left under `<out>`'s name, and only then
//! renames. A dead rebuild leaves a stray `.building` and nothing else,
//! and the rename is the single instant the switch happens. The daemon's
//! open snapshot keeps reading the old inode throughout.
//!
//! After a successful promote exactly one `reload` verb goes to the
//! daemon, and *after* rather than before on purpose: the promote is
//! atomic and the old inode stays readable, so a daemon told to drop its
//! handles first would only spend the build with no dictionary at all —
//! and would have to be running for the promote to be safe, which it may
//! not be. Reopening afterwards is both race-free and optional.
//!
//! That reload has **two** handles to reopen in the daemon, not one: the
//! worker's dictionary (`worker::ReopenDict`) and, since ticket 03, the
//! popup's own media-store connection (`Popup::reconfigure`). Failure
//! sends nothing — the old database is still the live one, and telling
//! the daemon to reopen it would be noise.

use crate::control::{self, Verb};
use crate::lock::{self, InstanceLock, LockError};
use chibipop::library::{Library, Pending};
use chibipop::settings::SettingsForm;
use std::path::{Path, PathBuf};

/// What a rebuild reports, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// One raw builder line, for [`chibipop::dict::progress::friendly`].
    Line(String),
    /// Built, renamed over, staged library changes kept, daemon told.
    Done { entries: i64, terms: i64, reload: Reload },
    /// Nothing changed: the old database is still the live one.
    Failed(String),
}

/// What the one `reload` verb achieved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reload {
    /// The daemon answered; its one reply line.
    Sent(String),
    /// Nothing listening on the control socket.
    NoDaemon,
}

/// Everywhere one rebuild reads or writes.
#[derive(Debug, Clone)]
pub struct Plan {
    /// The archive library the staged edits land in.
    pub library_dir: PathBuf,
    /// The database the daemon reads; built beside, renamed over.
    pub out: PathBuf,
    /// Where the library flock file lives.
    pub runtime_dir: PathBuf,
    /// The daemon's control socket; absent means no daemon.
    pub socket: PathBuf,
}

/// Take the library and start building.
///
/// The flock is claimed synchronously so a refusal reaches the status
/// area before any file moves, then moves into the thread: the kernel
/// releases it however the rebuild ends. Everything after the claim —
/// staging, building, committing, the `reload` — happens on the thread
/// and is reported through `sink`.
pub fn spawn(
    form: &SettingsForm,
    plan: Plan,
    sink: impl Fn(Progress) + Send + 'static,
) -> Result<(), LockError> {
    let lock = lock::acquire_at(&plan.runtime_dir, &lock::library_file_name(&plan.library_dir))?;
    let form = form.clone();
    std::thread::Builder::new()
        .name("chibipop-rebuild".to_string())
        .spawn(move || {
            // Held for the whole build, dropped with the thread.
            let _lock: InstanceLock = lock;
            sink(run(&form, &plan, &sink));
        })
        .map_err(LockError::Io)?;
    Ok(())
}

/// One rebuild, start to finish.
fn run(form: &SettingsForm, plan: &Plan, sink: &impl Fn(Progress)) -> Progress {
    // Refuses an empty library before it moves anything.
    let pending = match chibipop::settings::stage_into_library(form, &plan.library_dir) {
        Ok(pending) => pending,
        Err(e) => return Progress::Failed(format!("{e:#}")),
    };
    match build(plan, sink) {
        Ok((entries, terms)) => {
            keep(&pending);
            Progress::Done { entries, terms, reload: notify(&plan.socket) }
        }
        Err(e) => {
            let message = format!("{e:#}");
            put_back(&pending, &message);
            Progress::Failed(message)
        }
    }
}

/// Build the staged library into `plan.out`.
///
/// The archive names are announced here, in the same terse `term dict
/// [i] <file>` / `freq dict      <file>` spelling the Windows
/// `build-dict` command prints: core's builder only counts rows, so the
/// caller is what tells the user which file is being read.
///
/// The first list is `dict_paths`, which is every readable archive the
/// build installs as a dictionary row - a frequency- or pitch-only
/// archive is a dictionary in its own right now
/// (ARCHITECTURE.md#dictionary-and-lookup), not something the term list
/// skips. The `term dict` prefix stays because
/// [`chibipop::dict::progress::friendly`] is what turns these into
/// "Reading <file>…", and it parses exactly that word; the second list
/// re-announces the archives read for their frequencies, so an archive
/// carrying both roles is named twice on purpose.
fn build(plan: &Plan, sink: &impl Fn(Progress)) -> anyhow::Result<(i64, i64)> {
    // Reconciled: what stage_into_library just wrote is what builds.
    let lib = Library::load(&plan.library_dir)?;
    let dicts = lib.dict_paths(&plan.library_dir);
    let freqs = lib.freq_paths(&plan.library_dir);
    for (i, d) in dicts.iter().enumerate() {
        sink(Progress::Line(format!("term dict  [{i}] {}", file_name(d))));
    }
    for f in &freqs {
        sink(Progress::Line(format!("freq dict      {}", file_name(f))));
    }
    let counts = chibipop::dict::build::build(&dicts, &freqs, &plan.out, &|line| {
        sink(Progress::Line(line.to_string()));
    })?;
    Ok((counts.entries, counts.terms))
}

/// The archive's own file name, which is what the user recognises.
fn file_name(path: &Path) -> String {
    path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned()
}

/// The one `reload`, sent only after a successful rename.
fn notify(socket: &Path) -> Reload {
    match control::send_to(socket, Verb::Reload) {
        Ok(reply) => Reload::Sent(reply),
        Err(_) => Reload::NoDaemon,
    }
}

/// Let the removals go.
fn keep(pending: &Pending) {
    if let Err(e) = pending.commit() {
        eprintln!("chibipop: clearing the library's .removed folder failed: {e:#}");
    }
}

/// Put every archive back.
fn put_back(pending: &Pending, why: &str) {
    match pending.rollback() {
        Ok(()) => eprintln!("chibipop: {why} - your dictionary archives were put back."),
        Err(e) => {
            eprintln!("chibipop: {why}");
            eprintln!("chibipop: putting your archives back failed: {e:#}");
            eprintln!("chibipop: they are in the library's .removed folder.");
        }
    }
}

/// The status line a finished rebuild earns.
pub fn describe(progress: &Progress) -> String {
    match progress {
        Progress::Line(line) => {
            chibipop::dict::progress::friendly(line).unwrap_or_default()
        }
        Progress::Done { entries, terms, reload } => {
            let tail = match reload {
                Reload::Sent(reply) => format!("the daemon reloaded ({reply})"),
                Reload::NoDaemon => {
                    "the daemon is not running - it opens the new one when it starts".to_string()
                }
            };
            format!("Rebuilt: {entries} entries, {terms} term rows; {tail}.")
        }
        Progress::Failed(why) => {
            format!("The rebuild failed; your dictionary is unchanged. {why}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chibipop::lookup::model::Dictionary;
    use chibipop::lookup::sqlite::SqliteDictionary;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Receiver};
    use std::sync::Arc;

    /// A directory that goes away with the test.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> (PathBuf, Scratch) {
        let dir = std::env::temp_dir()
            .join(format!("chibipop_rebuild_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("library")).unwrap();
        std::fs::create_dir_all(dir.join("run")).unwrap();
        (dir.clone(), Scratch(dir))
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/yomitan")
            .join(name)
    }

    fn plan(dir: &Path) -> Plan {
        Plan {
            library_dir: dir.join("library"),
            out: dir.join("chibipop.sqlite"),
            runtime_dir: dir.join("run"),
            // Nothing listens unless a test binds it.
            socket: dir.join("run/absent.sock"),
        }
    }

    /// A form that stages `sources` in.
    fn staging(sources: &[PathBuf]) -> SettingsForm {
        let cfg = chibipop::config::Config::default();
        let mut form = chibipop::settings::from_config(&cfg, &[]);
        for source in sources {
            form.stage_add(source).unwrap_or_else(|| panic!("staging {}", source.display()));
        }
        form
    }

    /// Drives one rebuild to its last message.
    fn drive(form: &SettingsForm, plan: Plan) -> (Vec<Progress>, Progress) {
        let (tx, rx) = mpsc::channel();
        spawn(form, plan, move |p| {
            let _ = tx.send(p);
        })
        .expect("the library lock");
        collect(rx)
    }

    /// Which inode a path names right now: the whole point of building
    /// beside and renaming over is that this changes while an open
    /// connection keeps the old one.
    fn inode(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).unwrap().ino()
    }

    fn collect(rx: Receiver<Progress>) -> (Vec<Progress>, Progress) {
        let all: Vec<Progress> = rx.into_iter().collect();
        let last = all.last().expect("a rebuild always reports").clone();
        (all, last)
    }

    /// Every name in the library, sorted: what a rollback must restore.
    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A listener that counts requests and replies like the daemon.
    fn fake_daemon(path: &Path) -> Arc<AtomicUsize> {
        let listener = UnixListener::bind(path).expect("binding the fake daemon socket");
        let seen = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                if line.trim() == Verb::Reload.as_str() {
                    count.fetch_add(1, Ordering::SeqCst);
                }
                let mut stream = reader.into_inner();
                let _ = stream.write_all(b"OK reload\n");
            }
        });
        seen
    }

    fn done(progress: &Progress) -> (i64, i64) {
        match progress {
            Progress::Done { entries, terms, .. } => (*entries, *terms),
            other => panic!("expected a finished rebuild, got {other:?}"),
        }
    }

    /// The core contract: built beside, renamed over, nothing left behind.
    #[test]
    fn a_rebuild_lands_on_the_database_and_leaves_no_building_file() {
        let (dir, _guard) = scratch("lands");
        let p = plan(&dir);
        // A stale .building from a killed rebuild must not stop the next.
        std::fs::write(dir.join("chibipop.sqlite.building"), b"junk").unwrap();
        std::fs::write(&p.out, b"the old database").unwrap();

        let form = staging(&[fixture("terms.zip"), fixture("freq.zip")]);
        let (lines, last) = drive(&form, p.clone());

        let (entries, terms) = done(&last);
        assert!(entries > 0 && terms > 0, "{last:?}");
        assert!(!dir.join("chibipop.sqlite.building").exists(), "the temp file must be renamed away");
        let dict = SqliteDictionary::open(&p.out).expect("the renamed database opens");
        assert!(!dict.dicts().unwrap().is_empty(), "the new database names its dictionaries");
        assert!(
            lines.iter().any(|p| matches!(p, Progress::Line(l) if l.starts_with("term dict"))),
            "progress must be reported while it builds: {lines:?}"
        );
    }

    /// What "the daemon keeps serving lookups mid-rebuild" means: the open
    /// snapshot reads the old inode until it reopens, and reopening is
    /// what shows the new dictionary. This is the whole `reload` contract
    /// in one test.
    #[test]
    fn an_open_snapshot_keeps_reading_the_old_database_until_it_reopens() {
        let (dir, _guard) = scratch("snapshot");
        let p = plan(&dir);

        let first = staging(&[fixture("terms.zip")]);
        let (_, built) = drive(&first, p.clone());
        done(&built);

        // The daemon's connection, opened before the second rebuild.
        let daemon = SqliteDictionary::open(&p.out).expect("the daemon's snapshot");
        let before = daemon.dicts().unwrap().len();
        let old_inode = inode(&p.out);

        let mut second = chibipop::settings::with_library(
            staging(&[]),
            &Library::load(&p.library_dir).unwrap(),
        );
        // A genuinely different second dictionary, so the count moves. A
        // copy of the first under another name would not: `Library::load`
        // collapses byte-identical archives to one entry, because two names
        // for one archive are one dictionary.
        let extra = dir.join("second.zip");
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/media/media.zip"),
            &extra,
        )
        .unwrap();
        second.stage_add(&extra).expect("staging the second archive");
        let (_, rebuilt) = drive(&second, p.clone());
        done(&rebuilt);
        assert_ne!(
            old_inode,
            inode(&p.out),
            "the new database must arrive by rename, not by rewriting the inode \
             the daemon is reading"
        );

        assert_eq!(
            before,
            daemon.dicts().unwrap().len(),
            "the already-open snapshot must keep reading the inode it opened"
        );
        let reopened = SqliteDictionary::open(&p.out).expect("reopening after the rename");
        assert!(
            reopened.dicts().unwrap().len() > before,
            "reopening is what serves the new dictionary"
        );
    }

    /// The reported failure, end to end, through the real rebuild.
    ///
    /// A user re-imported their one dictionary, rebuilt, and got no popups
    /// at all. Their library held two byte-identical copies of the archive,
    /// one listed in `library.json` and one not, so the build made **two**
    /// dictionaries out of one file - their log said
    /// `2 dictionary/ies: Jitendex.org [2026-08-11], Jitendex.org
    /// [2026-08-11]` - and every lookup after it reported
    /// `database disk image is malformed`.
    ///
    /// This is the scenario, not the mechanism: one archive under two
    /// names has to rebuild into one dictionary, the promoted file has to be
    /// sound, a lookup has to answer, and exactly one `reload` has to go
    /// out. The mechanism that broke the file - a stale write-ahead log left
    /// under the promoted file's name - has its own test beside
    /// `dict::build::promote`, which is where the ordering that fixes it
    /// lives.
    #[test]
    fn the_reported_failure_rebuilds_into_one_sound_dictionary_that_answers() {
        let (dir, _guard) = scratch("reported");
        let p = plan(&dir);
        let seen = fake_daemon(&p.socket);
        let media_zip = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/media/media.zip");

        // What they had before: one dictionary, imported and built.
        done(&drive(&staging(std::slice::from_ref(&media_zip)), p.clone()).1);
        assert_eq!(1, SqliteDictionary::open(&p.out).unwrap().dicts().unwrap().len());

        // The re-import's leftover: the same bytes, a second name, unlisted.
        std::fs::copy(&media_zip, p.library_dir.join("[JA-EN] media (2026-08-11).zip")).unwrap();
        let again = chibipop::settings::with_library(
            staging(&[]),
            &Library::load(&p.library_dir).unwrap(),
        );
        let reloads_before = seen.load(Ordering::SeqCst);

        let (lines, rebuilt) = drive(&again, p.clone());
        done(&rebuilt);

        assert_eq!(
            reloads_before + 1,
            seen.load(Ordering::SeqCst),
            "one reload, and only after the promote",
        );
        for suffix in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{suffix}", p.out.display()));
            assert!(!sidecar.exists(), "{} must not outlive the promote", sidecar.display());
        }

        let reopened = SqliteDictionary::open(&p.out).expect("reopening after the promote");
        let dicts = reopened.dicts().unwrap();
        assert_eq!(1, dicts.len(), "one archive under two names is one dictionary: {dicts:?}");
        assert_eq!(
            1,
            lines
                .iter()
                .filter(|l| matches!(l, Progress::Line(t) if t.starts_with("term dict")))
                .count(),
            "and the rebuild only ever read one archive: {lines:?}",
        );
        // The lookup they could not get a popup from.
        assert_eq!(1, reopened.terms_for("ねこ").expect("the lookup must not fail").len());
    }

    /// A build that dies after the library was already edited: the
    /// archives go back, the live database never moves, and the daemon is
    /// told nothing. The failure is forced by leaving a *directory* where
    /// the build-beside file goes, which is what a killed rebuild plus a
    /// clumsy cleanup looks like on disk.
    #[test]
    fn a_failed_build_puts_the_archives_back_and_leaves_the_database_alone() {
        let (dir, _guard) = scratch("failed");
        let p = plan(&dir);

        done(&drive(&staging(&[fixture("terms.zip")]), p.clone()).1);
        let before = std::fs::read(&p.out).unwrap();
        let library_before = listing(&p.library_dir);

        std::fs::create_dir_all(dir.join("chibipop.sqlite.building/in-the-way")).unwrap();
        let mut form = chibipop::settings::with_library(
            staging(&[]),
            &Library::load(&p.library_dir).unwrap(),
        );
        let extra = dir.join("second.zip");
        std::fs::copy(fixture("terms.zip"), &extra).unwrap();
        form.stage_add(&extra).expect("staging a second archive");

        let (_, last) = drive(&form, p.clone());
        assert!(matches!(last, Progress::Failed(_)), "{last:?}");
        assert_eq!(before, std::fs::read(&p.out).unwrap(), "the live database must not move");
        assert_eq!(library_before, listing(&p.library_dir), "the staged archive must be undone");
    }

    /// The empty-library refusal reaches the status area, and nothing moves.
    #[test]
    fn an_empty_library_is_refused_without_touching_the_database() {
        let (dir, _guard) = scratch("empty");
        let p = plan(&dir);
        std::fs::write(&p.out, b"the old database").unwrap();

        let (_, last) = drive(&staging(&[]), p.clone());
        match &last {
            Progress::Failed(why) => assert!(why.contains("no dictionary"), "{why}"),
            other => panic!("an empty library must be refused, got {other:?}"),
        }
        assert_eq!(b"the old database".to_vec(), std::fs::read(&p.out).unwrap());
    }

    /// Exactly one `reload`, and only on success.
    #[test]
    fn one_reload_is_sent_after_a_successful_rebuild() {
        let (dir, _guard) = scratch("reload_once");
        let mut p = plan(&dir);
        p.socket = dir.join("run/daemon.sock");
        let seen = fake_daemon(&p.socket);

        let (_, last) = drive(&staging(&[fixture("terms.zip")]), p.clone());
        match &last {
            Progress::Done { reload: Reload::Sent(reply), .. } => {
                assert_eq!("OK reload", reply);
            }
            other => panic!("expected a live reload, got {other:?}"),
        }
        assert_eq!(1, seen.load(Ordering::SeqCst), "the daemon must be told exactly once");
    }

    #[test]
    fn no_reload_is_sent_when_the_build_fails() {
        let (dir, _guard) = scratch("reload_never");
        let mut p = plan(&dir);
        p.socket = dir.join("run/daemon.sock");
        let seen = fake_daemon(&p.socket);

        let (_, last) = drive(&staging(&[]), p.clone());
        assert!(matches!(last, Progress::Failed(_)), "{last:?}");
        assert_eq!(0, seen.load(Ordering::SeqCst), "a failed rebuild tells the daemon nothing");
    }

    /// No daemon is not a failure: the config file (and now the database)
    /// is the source of truth, read at next start.
    #[test]
    fn an_absent_daemon_is_reported_not_treated_as_a_failure() {
        let (dir, _guard) = scratch("no_daemon");
        let p = plan(&dir);
        let (_, last) = drive(&staging(&[fixture("terms.zip")]), p.clone());
        assert!(matches!(last, Progress::Done { reload: Reload::NoDaemon, .. }), "{last:?}");
    }

    /// Two settings processes, one library: the second is refused before
    /// it moves a file, and the message names the holder.
    #[test]
    fn a_concurrent_rebuild_is_refused_by_the_library_flock() {
        let (dir, _guard) = scratch("contend");
        let p = plan(&dir);
        let held = lock::acquire_at(&p.runtime_dir, &lock::library_file_name(&p.library_dir))
            .expect("standing in for the first settings process");

        let form = staging(&[fixture("terms.zip")]);
        match spawn(&form, p.clone(), |_| {}) {
            Err(LockError::AlreadyRunning { path, pid }) => {
                assert_eq!(held.path(), path);
                let msg = lock::rebuild_refusal(&p.library_dir, &path, pid);
                assert!(msg.contains("Another rebuild"), "{msg}");
            }
            Err(LockError::Io(e)) => panic!("io error instead of a refusal: {e}"),
            Ok(()) => panic!("two rebuilds of one library must not overlap"),
        }
        assert!(!p.out.exists(), "a refused rebuild writes nothing");

        drop(held);
        // Freed, but not always instantly reacquirable: sibling tests in
        // this binary fork children (settings::child, cursor::hyprctl),
        // and a fork clones the fd table - between clone and exec the
        // child's copy of `held`'s fd keeps the flock alive (CLOEXEC
        // closes only at exec). A bounded retry rides out that window;
        // production never hits it because the holding process forks
        // nothing while it rebuilds.
        let mut tries = 0;
        let last = loop {
            let (tx, rx) = mpsc::channel();
            match spawn(&form, p.clone(), move |progress| {
                let _ = tx.send(progress);
            }) {
                Ok(()) => break collect(rx).1,
                Err(LockError::AlreadyRunning { .. }) if tries < 100 => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => panic!("the freed library lock: {e:?}"),
            }
        };
        done(&last);
    }

    #[test]
    fn the_status_line_names_the_counts_and_what_the_daemon_did() {
        let live = Progress::Done {
            entries: 3,
            terms: 5,
            reload: Reload::Sent("OK reload".to_string()),
        };
        assert_eq!("Rebuilt: 3 entries, 5 term rows; the daemon reloaded (OK reload).", describe(&live));
        let dead = Progress::Done { entries: 3, terms: 5, reload: Reload::NoDaemon };
        assert!(describe(&dead).contains("not running"), "{}", describe(&dead));
        let failed = Progress::Failed("invalid Zip archive".to_string());
        assert!(describe(&failed).contains("unchanged"), "{}", describe(&failed));
        assert!(describe(&failed).contains("invalid Zip archive"));
        // Raw builder lines go through the shared renderer.
        assert_eq!(
            "12,500 of 768,636 entries…",
            describe(&Progress::Line("progress  12500 / 768636".to_string()))
        );
    }
}
