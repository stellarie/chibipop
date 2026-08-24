//! Rebuild, against a child.

use chibipop::lookup::model::Dictionary;
use chibipop::lookup::sqlite::SqliteDictionary;
use chibipop_windows::rebuild::{friendly, spawn_with, Progress};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

/// The real chibipop.exe.
const BUILDER: &str = env!("CARGO_BIN_EXE_chibipop");

struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/yomitan").join(name)
}

fn scratch(test_name: &str) -> (PathBuf, TempDirGuard) {
    let dir = std::env::temp_dir()
        .join("chibipop_rebuild_test")
        .join(format!("t_{}_{test_name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("library")).unwrap();
    (dir.clone(), TempDirGuard(dir))
}

/// Named independently.
fn tmp_of(out: &Path) -> PathBuf {
    PathBuf::from(format!("{}.tmp", out.display()))
}

fn drain(rx: Receiver<Progress>) -> Vec<Progress> {
    rx.into_iter().collect()
}

fn stock_library(dir: &Path) -> PathBuf {
    let library = dir.join("library");
    std::fs::copy(fixture("terms.zip"), library.join("terms.zip")).unwrap();
    std::fs::copy(fixture("freq.zip"), library.join("freq.zip")).unwrap();
    library
}

#[test]
fn a_fixture_library_builds_and_reports_done() {
    let (dir, _guard) = scratch("builds");
    let library = stock_library(&dir);
    let out = dir.join("chibipop.sqlite");

    let msgs = drain(spawn_with(Path::new(BUILDER), &library, &out).unwrap());

    let Some(Progress::Done(path)) = msgs.last() else {
        panic!("expected Done last, got {msgs:?}");
    };
    assert_eq!(&out, path);

    let db = SqliteDictionary::open(&out).unwrap();
    assert_eq!(3, db.entries(&[1, 2, 3, 4]).unwrap().len(), "3 entries built");
    let dicts = db.dicts().unwrap();
    assert_eq!(1, dicts.len());
    assert_eq!("FixtureTerms", dicts[0].name);
    assert!(!tmp_of(&out).exists(), "the .tmp is renamed away, not left");
}

#[test]
fn progress_lines_arrive_before_done() {
    let (dir, _guard) = scratch("lines");
    let library = stock_library(&dir);
    let out = dir.join("chibipop.sqlite");

    let msgs = drain(spawn_with(Path::new(BUILDER), &library, &out).unwrap());

    let done_at = msgs
        .iter()
        .position(|m| matches!(m, Progress::Done(_)))
        .unwrap_or_else(|| panic!("no Done in {msgs:?}"));
    assert_eq!(msgs.len() - 1, done_at, "Done is the last message");
    assert!(done_at > 0, "the builder's own output is forwarded: {msgs:?}");

    let lines: Vec<&str> = msgs[..done_at]
        .iter()
        .map(|m| match m {
            Progress::Line(l) => l.as_str(),
            other => panic!("expected only lines before Done, got {other:?}"),
        })
        .collect();
    assert!(lines.iter().any(|l| l.contains("terms.zip")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("freq.zip")), "{lines:?}");
    assert!(lines.iter().any(|l| l.starts_with("wrote ")), "{lines:?}");
}

/// From real child output.
#[test]
fn every_archive_line_renders_as_a_name_and_the_last_line_does_not() {
    let (dir, _guard) = scratch("friendly");
    let library = stock_library(&dir);
    let out = dir.join("chibipop.sqlite");

    let msgs = drain(spawn_with(Path::new(BUILDER), &library, &out).unwrap());

    let lines: Vec<&String> = msgs
        .iter()
        .filter_map(|m| match m {
            Progress::Line(l) => Some(l),
            _ => None,
        })
        .collect();
    let shown: Vec<String> = lines.iter().filter_map(|l| friendly(l)).collect();
    assert_eq!(
        vec![
            "Reading terms.zip…".to_string(),
            "Reading freq.zip…".to_string(),
            "Creating search index…".to_string(),
        ],
        shown
    );
    assert_eq!(lines.len(), shown.len() + 1, "the wrote line is swallowed: {lines:?}");
    assert!(!shown.iter().any(|s| s.contains(".tmp")), "{shown:?}");
}

#[test]
fn a_failed_build_leaves_an_existing_output_byte_identical() {
    let (dir, _guard) = scratch("failed");
    let library = dir.join("library");
    std::fs::write(library.join("broken.zip"), b"not a zip at all").unwrap();
    let out = dir.join("chibipop.sqlite");
    let known: &[u8] = b"the dictionary the user already had";
    std::fs::write(&out, known).unwrap();

    let msgs = drain(spawn_with(Path::new(BUILDER), &library, &out).unwrap());

    let Some(Progress::Failed(why)) = msgs.last() else {
        panic!("expected Failed last, got {msgs:?}");
    };
    assert!(why.contains("the builder failed"), "a real child ran: {why}");
    assert!(why.contains("Zip"), "the child's own error is reported: {why}");
    assert!(
        msgs.iter().any(|m| matches!(m, Progress::Line(l) if l.contains("broken.zip"))),
        "stdout was drained even on the failing path: {msgs:?}"
    );
    assert_eq!(known, std::fs::read(&out).unwrap().as_slice(), "untouched");
    assert!(!tmp_of(&out).exists(), "no .tmp left behind");
}

#[test]
fn an_empty_library_directory_is_a_failure_not_an_empty_database() {
    let (dir, _guard) = scratch("empty");
    let library = dir.join("library");
    let out = dir.join("chibipop.sqlite");
    let known: &[u8] = b"the dictionary the user already had";
    std::fs::write(&out, known).unwrap();

    let msgs = drain(spawn_with(Path::new(BUILDER), &library, &out).unwrap());

    let Some(Progress::Failed(why)) = msgs.last() else {
        panic!("expected Failed last, got {msgs:?}");
    };
    // The child would exit 0 here.
    assert_eq!(1, msgs.len(), "refused before spawning: {msgs:?}");
    assert!(why.contains("no dictionary archives"), "{why}");
    assert_eq!(known, std::fs::read(&out).unwrap().as_slice(), "untouched");
    assert!(!tmp_of(&out).exists(), "no .tmp left behind");
}

#[test]
fn a_missing_library_directory_is_a_failure() {
    let (dir, _guard) = scratch("missing");
    let out = dir.join("chibipop.sqlite");

    let msgs = drain(spawn_with(Path::new(BUILDER), &dir.join("gone"), &out).unwrap());

    assert!(matches!(msgs.last(), Some(Progress::Failed(_))), "{msgs:?}");
    assert!(!out.exists(), "nothing is created from nothing");
}

/// Held files are not built.
#[test]
fn a_quarantined_archive_is_invisible_to_the_real_builder() {
    let (dir, _guard) = scratch("quarantined");
    let library = stock_library(&dir);
    let held = library.join(".removed");
    std::fs::create_dir_all(&held).unwrap();
    std::fs::rename(library.join("terms.zip"), held.join("terms.zip")).unwrap();
    std::fs::copy(fixture("terms.zip"), library.join("other.zip")).unwrap();
    let out = dir.join("chibipop.sqlite");

    let msgs = drain(spawn_with(Path::new(BUILDER), &library, &out).unwrap());

    assert!(matches!(msgs.last(), Some(Progress::Done(_))), "{msgs:?}");
    assert!(
        !msgs.iter().any(|m| matches!(m, Progress::Line(l) if l.contains("terms.zip"))),
        "the held archive was read anyway: {msgs:?}"
    );
    assert!(held.join("terms.zip").is_file(), "and it is still there");
    assert_eq!(1, SqliteDictionary::open(&out).unwrap().dicts().unwrap().len());
}

/// All held is still empty.
#[test]
fn a_library_whose_archives_are_all_quarantined_refuses() {
    let (dir, _guard) = scratch("all_held");
    let library = stock_library(&dir);
    let held = library.join(".removed");
    std::fs::create_dir_all(&held).unwrap();
    for name in ["terms.zip", "freq.zip"] {
        std::fs::rename(library.join(name), held.join(name)).unwrap();
    }
    let out = dir.join("chibipop.sqlite");

    let msgs = drain(spawn_with(Path::new(BUILDER), &library, &out).unwrap());

    let Some(Progress::Failed(why)) = msgs.last() else {
        panic!("expected Failed last, got {msgs:?}");
    };
    assert!(why.contains("no dictionary archives"), "{why}");
    assert!(!out.exists());
}
