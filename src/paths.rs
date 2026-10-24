//! File paths that chibipop owns.

use std::path::{Path, PathBuf};

/// Return a path beside the executable.
pub fn beside_exe(relative: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|dir| dir.join(relative)))
        .unwrap_or_else(|| PathBuf::from(relative))
}

/// Return the repository root above `target/`.
///
/// Match only `<root>/target/debug` or `<root>/target/release`.
/// A shipped chibipop has no such path structure. It must not traverse parent
/// directories to use an unrelated `data` folder beside its directory.
fn cargo_root(exe_dir: &Path) -> Option<PathBuf> {
    let profile = exe_dir.file_name()?.to_str()?;
    if profile != "debug" && profile != "release" {
        return None;
    }
    let target = exe_dir.parent()?;
    if target.file_name()?.to_str()? != "target" {
        return None;
    }
    target.parent().map(Path::to_path_buf)
}

/// Return the path beside the executable or at the Cargo root.
///
/// Do not use the working directory.
pub fn data_file(relative: &str) -> PathBuf {
    let beside = beside_exe(relative);
    if beside.exists() {
        return beside;
    }

    // Development uses `data/` at the repository root.
    match std::env::current_exe().ok().and_then(|e| e.parent().and_then(cargo_root)) {
        Some(root) => root.join(relative),
        None => beside,
    }
}

