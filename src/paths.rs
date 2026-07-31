//! chibipop's own file paths.

use std::path::{Path, PathBuf};

/// Resolved beside the exe.
pub fn beside_exe(relative: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|dir| dir.join(relative)))
        .unwrap_or_else(|| PathBuf::from(relative))
}

/// The repo root above target/.
///
/// Only matches `<root>/target/debug` or `<root>/target/release`, so a
/// shipped chibipop - which has no such ancestry - can never walk up and
/// adopt an unrelated `data` folder sitting beside its own directory.
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

/// Beside the exe or cargo root.
///
/// Never the working directory.
pub fn data_file(relative: &str) -> PathBuf {
    let beside = beside_exe(relative);
    if beside.exists() {
        return beside;
    }

    // Dev has data/ at repo root.
    match std::env::current_exe().ok().and_then(|e| e.parent().and_then(cargo_root)) {
        Some(root) => root.join(relative),
        None => beside,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_name_lands_beside_the_executable() {
        let got = beside_exe("data/chibipop.sqlite");
        let exe = std::env::current_exe().unwrap();
        assert_eq!(exe.parent().unwrap(), got.parent().unwrap().parent().unwrap());
        assert!(got.ends_with("data/chibipop.sqlite"));
    }

    #[test]
    fn a_bare_name_lands_directly_beside_it() {
        let got = beside_exe("chibipop.toml");
        let exe = std::env::current_exe().unwrap();
        assert_eq!(exe.parent().unwrap(), got.parent().unwrap());
    }

    /// The cwd must never decide.
    #[test]
    fn a_data_file_that_exists_nowhere_is_still_an_absolute_path() {
        let name = "definitely-not-a-real-chibipop-file.sqlite";
        let got = data_file(name);
        assert!(got.is_absolute(), "{got:?}");
        assert_ne!(PathBuf::from(name), got);
        assert!(got.ends_with(name), "{got:?}");
    }

    #[test]
    fn a_cargo_target_layout_resolves_to_the_repo_root() {
        assert_eq!(
            Some(PathBuf::from("C:/x/chibipop")),
            cargo_root(Path::new("C:/x/chibipop/target/release"))
        );
        assert_eq!(
            Some(PathBuf::from("C:/x/chibipop")),
            cargo_root(Path::new("C:/x/chibipop/target/debug"))
        );
    }

    /// A shipped layout must never walk up.
    #[test]
    fn anything_but_a_cargo_target_layout_declines() {
        // The shipped case: exe and data in one folder, no target/ above.
        assert_eq!(None, cargo_root(Path::new("C:/Users/You/Downloads/chibipop")));
        // Right profile name, wrong parent.
        assert_eq!(None, cargo_root(Path::new("C:/x/notarget/release")));
        // Right parent, wrong profile name.
        assert_eq!(None, cargo_root(Path::new("C:/x/target/staging")));
    }

    #[test]
    fn a_data_file_beside_the_exe_wins_over_the_cwd() {
        let name = "chibipop-paths-marker.tmp";
        let beside = beside_exe(name);
        std::fs::write(&beside, b"x").expect("writable next to the test binary");
        assert_eq!(beside, data_file(name));
        let _ = std::fs::remove_file(&beside);
    }
}
