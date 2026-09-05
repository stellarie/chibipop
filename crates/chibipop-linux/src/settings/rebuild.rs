//! The rebuild runs in this process (ARCHITECTURE.md#settings-and-config).
//!
//! Windows spawns `chibipop.exe build-dict` as a child with
//! `CREATE_NO_WINDOW` and reads its stdout. Linux has no window to
//! suppress and no reason for a second process. The same core builder
//! runs here on a background thread. Its progress lines go straight
//! down a channel. `chibipop::dict::build::build` owns the whole
//! crash-safety design. It builds into `<out>.building`. It reads the
//! finished file back before anything depends on it. It clears the
//! sidecars that the replaced database left under the name of `<out>`.
//! It renames only after these steps. A dead rebuild leaves one stray
//! `.building` file and nothing else. The rename is the single instant
//! of the switch. The open snapshot of the daemon reads the old inode
//! for the whole build.
//!
//! After a successful promote, exactly one `reload` verb goes to the
//! daemon. The verb goes *after* the promote, not before, on purpose.
//! The promote is atomic, and the old inode stays readable. A daemon
//! that drops its handles first has no dictionary for the whole build.
//! The opposite order is safe only with a running daemon, and the
//! daemon can be absent. A reopen after the promote has no race, and
//! it is optional.
//!
//! That reload reopens **two** handles in the daemon, not one: the
//! dictionary of the worker (`worker::ReopenDict`) and the media-store
//! connection of the popup (`Popup::reconfigure`). A failure sends
//! nothing. The old database is still the live one. A reload command
//! for that database is only noise.

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
    /// The build finished, the rename happened, the library keeps the
    /// staged changes, and the daemon knows.
    Done { entries: i64, terms: i64, reload: Reload },
    /// Nothing changed: the old database is still the live one.
    Failed(String),
}

/// The result of the one `reload` verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reload {
    /// The daemon answered. This value holds its one reply line.
    Sent(String),
    /// No process listens on the control socket.
    NoDaemon,
}

/// Every path that one rebuild reads or writes.
#[derive(Debug, Clone)]
pub struct Plan {
    /// The archive library that receives the staged edits.
    pub library_dir: PathBuf,
    /// The database that the daemon reads. The build writes a new file
    /// beside it, then renames the new file over it.
    pub out: PathBuf,
    /// The directory that holds the library flock file.
    pub runtime_dir: PathBuf,
    /// The control socket of the daemon. An absent socket means no daemon.
    pub socket: PathBuf,
}

/// Take the library and start the build.
///
/// This function claims the flock synchronously, so a refusal reaches
/// the status area before any file moves. The flock then moves into
/// the thread, and the kernel releases it however the rebuild ends.
/// Every later step runs on the thread and reports through `sink`.
/// These steps are the staging, the build, the commit, and the
/// `reload`.
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
            let result = {
                // Held until terminal result.
                let _lock: InstanceLock = lock;
                run(&form, &plan, &sink)
            };
            sink(result);
        })
        .map_err(LockError::Io)?;
    Ok(())
}

/// One rebuild, start to finish.
fn run(form: &SettingsForm, plan: &Plan, sink: &impl Fn(Progress)) -> Progress {
    // This call refuses an empty library before it moves any file.
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
/// This function prints the archive names. It uses the same `term dict
/// [i] <file>` and `freq dict      <file>` spelling that the Windows
/// `build-dict` command prints. The core builder only counts rows, so
/// the caller must name the file that the build reads.
///
/// The first list is `dict_paths`. That list holds every readable
/// archive that the build installs as a dictionary row. A
/// frequency-only or pitch-only archive is now a dictionary of its own
/// (ARCHITECTURE.md#dictionary-and-lookup), and the term list does not
/// skip it. The `term dict` prefix stays because
/// [`chibipop::dict::progress::friendly`] turns these lines into
/// "Reading <file>…" and parses exactly that word. The second list
/// names the archives that the build reads for their frequencies. An
/// archive with both roles therefore appears twice on purpose.
fn build(plan: &Plan, sink: &impl Fn(Progress)) -> anyhow::Result<(i64, i64)> {
    // The build reads exactly what stage_into_library wrote.
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

/// The file name of the archive, because the user recognizes that name.
fn file_name(path: &Path) -> String {
    path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned()
}

/// Send the one `reload` verb, only after a successful rename.
fn notify(socket: &Path) -> Reload {
    match control::send_to(socket, Verb::Reload) {
        Ok(reply) => Reload::Sent(reply),
        Err(_) => Reload::NoDaemon,
    }
}

/// Complete the staged removals.
fn keep(pending: &Pending) {
    if let Err(e) = pending.commit() {
        eprintln!("chibipop: clearing the library's .removed folder failed: {e:#}");
    }
}

/// Restore every archive.
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

/// Build the status line for a finished rebuild.
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
    use std::time::Duration;

    /// A directory that the test removes at drop.
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
            // No process listens unless a test binds this path.
            socket: dir.join("run/absent.sock"),
        }
    }

    /// A form that stages `sources`.
    fn staging(sources: &[PathBuf]) -> SettingsForm {
        let cfg = chibipop::config::Config::default();
        let mut form = chibipop::settings::from_config(&cfg, &[]);
        for source in sources {
            form.stage_add(source).unwrap_or_else(|| panic!("staging {}", source.display()));
        }
        form
    }

    /// Drive one rebuild to its last message.
    fn drive(form: &SettingsForm, plan: Plan) -> (Vec<Progress>, Progress) {
        let (tx, rx) = mpsc::channel();
        spawn(form, plan, move |p| {
            let _ = tx.send(p);
        })
        .expect("the library lock");
        collect(rx)
    }

    /// The inode that a path names now. The build writes beside the
    /// file and renames over it, so this inode changes while an open
    /// connection keeps the old inode.
    fn inode(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).unwrap().ino()
    }

    fn collect(rx: Receiver<Progress>) -> (Vec<Progress>, Progress) {
        let all: Vec<Progress> = rx.into_iter().collect();
        let last = all.last().expect("a rebuild always reports").clone();
        (all, last)
    }

    /// Every name in the library, sorted. A rollback must restore this
    /// list.
    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A listener that counts the requests and answers like the daemon.
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

    /// The core contract. The build writes beside the file, renames
    /// over it, and leaves nothing.
    #[test]
    fn a_rebuild_lands_on_the_database_and_leaves_no_building_file() {
        let (dir, _guard) = scratch("lands");
        let p = plan(&dir);
        // A stale .building file from a killed rebuild must not stop the next rebuild.
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

    /// This test defines how the daemon answers lookups during a
    /// rebuild. The open snapshot reads the old inode until the daemon
    /// reopens the file. Only a reopen shows the new dictionary. This
    /// test holds the whole `reload` contract.
    #[test]
    fn an_open_snapshot_keeps_reading_the_old_database_until_it_reopens() {
        let (dir, _guard) = scratch("snapshot");
        let p = plan(&dir);

        let first = staging(&[fixture("terms.zip")]);
        let (_, built) = drive(&first, p.clone());
        done(&built);

        // The connection of the daemon, opened before the second rebuild.
        let daemon = SqliteDictionary::open(&p.out).expect("the daemon's snapshot");
        let before = daemon.dicts().unwrap().len();
        let old_inode = inode(&p.out);

        let mut second = chibipop::settings::with_library(
            staging(&[]),
            &Library::load(&p.library_dir).unwrap(),
        );
        // A different second dictionary, so the count moves. A copy of
        // the first under another name does not move the count.
        // `Library::load` collapses byte-identical archives to one
        // entry, because two names for one archive are one dictionary.
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
    /// A user imported one dictionary again, rebuilt, and saw no popup.
    /// The library held two byte-identical copies of the archive. The
    /// file `library.json` listed one copy and not the other, so the
    /// build made **two** dictionaries from one file. The log said
    /// `2 dictionary/ies: Jitendex.org [2026-08-11], Jitendex.org
    /// [2026-08-11]`. Every later lookup reported
    /// `database disk image is malformed`.
    ///
    /// This test covers the scenario, not the mechanism. One archive
    /// under two names must rebuild into one dictionary. The promoted
    /// file must be sound. A lookup must answer. Exactly one `reload`
    /// must go to the daemon. A stale write-ahead log under the name of
    /// the promoted file broke the file. That mechanism has its own
    /// test beside `dict::build::promote`, which holds the order that
    /// fixes it.
    #[test]
    fn the_reported_failure_rebuilds_into_one_sound_dictionary_that_answers() {
        let (dir, _guard) = scratch("reported");
        let p = plan(&dir);
        let seen = fake_daemon(&p.socket);
        let media_zip = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/media/media.zip");

        // The state before the failure: one dictionary, imported and built.
        done(&drive(&staging(std::slice::from_ref(&media_zip)), p.clone()).1);
        assert_eq!(1, SqliteDictionary::open(&p.out).unwrap().dicts().unwrap().len());

        // The leftover of the second import: the same bytes under a second, unlisted name.
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
        // The lookup that gave the user no popup.
        assert_eq!(1, reopened.terms_for("ねこ").expect("the lookup must not fail").len());
    }

    /// A build dies after an edit to the library. The rollback restores
    /// the archives, the live database does not move, and the rebuild
    /// tells the daemon nothing. This test forces the failure with a
    /// *directory* at the path of the build-beside file. A killed
    /// rebuild plus a rough cleanup leaves exactly that on disk.
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

    /// The refusal of an empty library reaches the status area, and nothing moves.
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

    /// An absent daemon is not a failure. The config file and now the
    /// database hold the truth, and the next start reads them.
    #[test]
    fn an_absent_daemon_is_reported_not_treated_as_a_failure() {
        let (dir, _guard) = scratch("no_daemon");
        let p = plan(&dir);
        let (_, last) = drive(&staging(&[fixture("terms.zip")]), p.clone());
        assert!(matches!(last, Progress::Done { reload: Reload::NoDaemon, .. }), "{last:?}");
    }

    /// Two settings processes share one library. The flock refuses the
    /// second process before it moves a file, and the message names the
    /// holder.
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
        // The lock is free, but a new claim can still fail at once.
        // Sibling tests in this binary fork children (settings::child,
        // cursor::hyprctl), and a fork clones the fd table. Between the
        // clone and the exec, the copy of the fd of `held` in the child
        // keeps the flock alive, because CLOEXEC closes the fd only at
        // exec. A bounded retry covers that window. Production never
        // meets it, because the holding process forks nothing during a
        // rebuild.
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
    fn terminal_progress_arrives_after_the_library_flock_is_free() {
        let (dir, _guard) = scratch("terminal_unlocked");
        let p = plan(&dir);
        let runtime_dir = p.runtime_dir.clone();
        let lock_name = lock::library_file_name(&p.library_dir);
        let form = staging(&[fixture("terms.zip")]);
        let (tx, rx) = mpsc::channel();

        spawn(&form, p, move |progress| {
            if matches!(progress, Progress::Done { .. } | Progress::Failed(_)) {
                let reacquired = lock::acquire_at(&runtime_dir, &lock_name).is_ok();
                let _ = tx.send(reacquired);
            }
        })
        .expect("the library lock");

        assert!(
            rx.recv_timeout(Duration::from_secs(30)).unwrap(),
            "terminal progress must wait until the library flock is free"
        );
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
        // The shared renderer formats raw builder lines.
        assert_eq!(
            "12,500 of 768,636 entries…",
            describe(&Progress::Line("progress  12500 / 768636".to_string()))
        );
    }
}
