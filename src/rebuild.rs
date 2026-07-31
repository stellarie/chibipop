//! The rebuild child process.

use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Read};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;

/// What a rebuild reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Line(String),
    Done(PathBuf),
    Failed(String),
}

/// Rebuilds with our own exe.
pub fn spawn(library: &Path, out: &Path) -> Result<Receiver<Progress>> {
    let exe = std::env::current_exe().context("locating chibipop.exe")?;
    spawn_with(&exe, library, out)
}

/// Rebuilds with `exe`.
pub fn spawn_with(exe: &Path, library: &Path, out: &Path) -> Result<Receiver<Progress>> {
    let (tx, rx) = mpsc::channel();
    let exe = exe.to_path_buf();
    let library = library.to_path_buf();
    let out = out.to_path_buf();
    std::thread::Builder::new()
        .name("chibipop-rebuild".to_string())
        .spawn(move || {
            let tmp = tmp_path(&out);
            let built = run(&exe, &library, &out, &tmp, &tx);
            let msg = match built {
                Ok(()) => Progress::Done(out),
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp);
                    Progress::Failed(format!("{e:#}"))
                }
            };
            let _ = tx.send(msg);
        })
        .context("starting the rebuild thread")?;
    Ok(rx)
}

/// One build, start to finish.
fn run(exe: &Path, library: &Path, out: &Path, tmp: &Path, tx: &Sender<Progress>) -> Result<()> {
    // An empty build wipes it.
    if archive_count(library)? == 0 {
        bail!("{} holds no dictionary archives", library.display());
    }
    if tmp.exists() {
        std::fs::remove_file(tmp).with_context(|| format!("removing {}", tmp.display()))?;
    }

    let mut child = Command::new(exe)
        .arg("build-dict")
        .arg("--library")
        .arg(library)
        .arg("--out")
        .arg(tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Or a black box flashes up.
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn()
        .with_context(|| format!("starting {}", exe.display()))?;

    let stdout = child.stdout.take().context("the builder gave no stdout")?;
    let lines = tx.clone();
    // A full pipe deadlocks wait.
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(|l| l.ok()) {
            if lines.send(Progress::Line(line)).is_err() {
                break;
            }
        }
    });
    let mut errors = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut errors);
    }
    let _ = reader.join();

    let status = child.wait().context("waiting for the builder")?;
    if !status.success() {
        bail!("the builder failed ({status}){}", last_line(&errors));
    }

    std::fs::rename(tmp, out)
        .with_context(|| format!("replacing {} with {}", out.display(), tmp.display()))?;
    Ok(())
}

/// `<out>` with `.tmp` appended.
fn tmp_path(out: &Path) -> PathBuf {
    with_suffix(out, ".tmp")
}

/// Where a staged build lands.
pub fn staging_path(out: &Path) -> PathBuf {
    with_suffix(out, ".new")
}

/// Puts a staged build in place.
pub fn promote(staged: &Path, out: &Path) -> Result<()> {
    std::fs::rename(staged, out)
        .with_context(|| format!("replacing {} with {}", out.display(), staged.display()))
}

/// `<out>` plus `suffix`.
fn with_suffix(out: &Path, suffix: &str) -> PathBuf {
    let mut name = out.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// One child line, for a person.
///
/// None for lines to swallow.
pub fn friendly(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("progress") {
        let (n, t) = rest.trim().split_once('/')?;
        let n: u64 = n.trim().parse().ok()?;
        let t: u64 = t.trim().parse().ok()?;
        return Some(format!("{} of {} entries…", format_thousands(n), format_thousands(t)));
    }
    if line.starts_with("building") {
        return Some("Creating search index…".to_string());
    }
    let rest = line.strip_prefix("term dict").or_else(|| line.strip_prefix("freq dict"))?;
    let rest = rest.trim();
    // Drop main.rs's [i] prefix.
    let name = match rest.strip_prefix('[').and_then(|a| a.split_once(']')) {
        Some((n, tail)) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => tail.trim(),
        _ => rest,
    };
    if name.is_empty() {
        return None;
    }
    Some(format!("Reading {name}…"))
}

/// Groups digits by comma.
fn format_thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

/// How many archives are there?
fn archive_count(library: &Path) -> Result<usize> {
    let listing = std::fs::read_dir(library)
        .with_context(|| format!("reading {}", library.display()))?;
    Ok(listing
        .filter_map(std::result::Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("zip")))
        .count())
}

/// The innermost cause, if any.
fn last_line(errors: &str) -> String {
    match errors.lines().rev().find(|l| !l.trim().is_empty()) {
        Some(line) => format!(": {}", line.trim()),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_temp_name_sits_beside_the_output() {
        let tmp = tmp_path(Path::new(r"C:\a\data\chibipop.sqlite"));
        assert_eq!(Path::new(r"C:\a\data\chibipop.sqlite.tmp"), tmp);
    }

    #[test]
    fn the_no_window_flag_is_the_documented_value() {
        assert_eq!(0x0800_0000, CREATE_NO_WINDOW.0);
    }

    #[test]
    fn a_missing_directory_counts_as_an_error_not_as_zero() {
        assert!(archive_count(Path::new(r"C:\nope\chibipop\nope")).is_err());
    }

    #[test]
    fn the_staging_file_sits_beside_the_output() {
        let staged = staging_path(Path::new(r"C:\a\data\chibipop.sqlite"));
        assert_eq!(Path::new(r"C:\a\data\chibipop.sqlite.new"), staged);
    }

    #[test]
    fn a_staged_build_does_not_share_the_live_temp_name() {
        let out = Path::new(r"C:\a\data\chibipop.sqlite");
        assert_ne!(tmp_path(out), tmp_path(&staging_path(out)));
    }

    #[test]
    fn an_archive_line_is_rendered_without_its_index() {
        assert_eq!(
            Some("Reading 01 [JA-EN] jitendex.zip…".to_string()),
            friendly("term dict  [0] 01 [JA-EN] jitendex.zip")
        );
        assert_eq!(
            Some("Reading [JA Freq] jiten_freq.zip…".to_string()),
            friendly("freq dict      [JA Freq] jiten_freq.zip")
        );
        assert_eq!(
            Some("Reading b.zip…".to_string()),
            friendly("term dict  [10] b.zip")
        );
    }

    #[test]
    fn a_progress_line_is_formatted_with_commas() {
        assert_eq!(
            Some("12,500 of 768,636 entries…".to_string()),
            friendly("progress  12500 / 768636")
        );
    }

    #[test]
    fn a_small_progress_has_no_commas() {
        assert_eq!(
            Some("500 of 3,000 entries…".to_string()),
            friendly("progress  500 / 3000")
        );
    }

    #[test]
    fn a_building_line_is_passed_through() {
        assert_eq!(
            Some("Creating search index…".to_string()),
            friendly("building  creating index")
        );
    }

    /// Not for the user.
    #[test]
    fn the_builders_final_line_is_swallowed() {
        assert_eq!(
            None,
            friendly(r"wrote C:\Users\x\data\chibipop.sqlite.tmp: 3 entries, 5 term rows")
        );
        assert_eq!(None, friendly("term dict  [0] "));
        assert_eq!(None, friendly(""));
    }

    #[test]
    fn the_innermost_cause_is_what_gets_reported() {
        let stderr = "Error: opening x\n\nCaused by:\n    invalid Zip archive\n";
        assert_eq!(": invalid Zip archive", last_line(stderr));
        assert_eq!("", last_line("   \n\n"));
    }
}
