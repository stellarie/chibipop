//! chibipop's own file paths.

use std::path::PathBuf;

/// Resolved beside the exe.
pub fn beside_exe(relative: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|dir| dir.join(relative)))
        .unwrap_or_else(|| PathBuf::from(relative))
}

/// Beside the exe, else the cwd.
pub fn data_file(relative: &str) -> PathBuf {
    let beside = beside_exe(relative);
    if beside.exists() {
        beside
    } else {
        PathBuf::from(relative)
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

    #[test]
    fn a_data_file_missing_beside_the_exe_falls_back_to_the_cwd() {
        let name = "definitely-not-a-real-chibipop-file.sqlite";
        assert_eq!(PathBuf::from(name), data_file(name));
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
