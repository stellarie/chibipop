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

    /// The current working directory must not determine this path.
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

    /// A shipped layout must not traverse a parent directory.
    #[test]
    fn anything_but_a_cargo_target_layout_declines() {
        // The shipped case puts the executable and data in one folder without
        // `target/` above it.
        assert_eq!(None, cargo_root(Path::new("C:/Users/You/Downloads/chibipop")));
        // The profile name is correct, but the parent directory is not.
        assert_eq!(None, cargo_root(Path::new("C:/x/notarget/release")));
        // The parent directory is correct, but the profile name is not.
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
