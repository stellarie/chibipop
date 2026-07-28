//! Where chibipop's own files live.

use std::path::PathBuf;

/// Resolved beside the exe.
pub fn beside_exe(relative: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|dir| dir.join(relative)))
        .unwrap_or_else(|| PathBuf::from(relative))
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
}
