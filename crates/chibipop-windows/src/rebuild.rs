//! Run a dictionary rebuild in a child process.
//!
//! The child writes a temporary database beside the output. The parent renames
//! that file into place only after the child succeeds.

use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Read};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;

/// Report progress or the result of a dictionary rebuild.
///
/// `Line` carries child output. `Done` carries the output path. `Failed` carries
/// the error text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Line(String),
    Done(PathBuf),
    Failed(String),
}

/// Start a dictionary rebuild with the current executable.
///
/// The returned channel carries child output and the final result. The rebuild
/// runs on a separate thread so the caller does not block on the child.
pub fn spawn(library: &Path, out: &Path) -> Result<Receiver<Progress>> {
    let exe = std::env::current_exe().context("locating chibipop.exe")?;
    spawn_with(&exe, library, out)
}

/// Start a dictionary rebuild with `exe`.
///
/// The returned channel carries child output and the final result. Use this
/// entry point when the caller supplies the executable path.
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

/// Run one rebuild and replace the output only after success.
fn run(exe: &Path, library: &Path, out: &Path, tmp: &Path, tx: &Sender<Progress>) -> Result<()> {
    // Reject an empty library before the rebuild can replace the output.
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
        // Do not show a console window. The child has no visible console window.
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn()
        .with_context(|| format!("starting {}", exe.display()))?;

    let stdout = child.stdout.take().context("the builder gave no stdout")?;
    let lines = tx.clone();
    // Read stdout on another thread. A full pipe can block the child before `wait` returns.
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

/// Build the temporary path beside `<out>`.
///
/// The rebuild writes here before it replaces `<out>`.
fn tmp_path(out: &Path) -> PathBuf {
    with_suffix(out, ".tmp")
}

/// Return `<out>` with `suffix` appended.
fn with_suffix(out: &Path, suffix: &str) -> PathBuf {
    let mut name = out.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Count the ZIP archives in `library`.
///
/// Return an error when the directory cannot be read.
fn archive_count(library: &Path) -> Result<usize> {
    let listing = std::fs::read_dir(library)
        .with_context(|| format!("reading {}", library.display()))?;
    Ok(listing
        .filter_map(std::result::Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("zip")))
        .count())
}

/// Extract the last non-empty line from child error text.
///
/// The rebuild adds this line to its failure message.
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
    fn the_innermost_cause_is_what_gets_reported() {
        let stderr = "Error: opening x\n\nCaused by:\n    invalid Zip archive\n";
        assert_eq!(": invalid Zip archive", last_line(stderr));
        assert_eq!("", last_line("   \n\n"));
    }
}
