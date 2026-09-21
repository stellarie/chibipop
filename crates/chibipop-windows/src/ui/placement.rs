//! Remembers where the settings window sits.
//! Windows restores no position by itself.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const STATE_FILE: &str = "chibipop.window.toml";

const STATE_HEADER: &str = "# The position of the settings window. chibipop writes this file.\n";

/// The top-left corner in screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
}

/// The part of a monitor that windows may cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Area {
    /// Keeps a window of `size` inside the area.
    pub fn fit(&self, wanted: (i32, i32), size: (i32, i32)) -> (i32, i32) {
        let right = (self.right - size.0).max(self.left);
        let bottom = (self.bottom - size.1).max(self.top);
        (wanted.0.clamp(self.left, right), wanted.1.clamp(self.top, bottom))
    }
}

/// Returns the path of the position file.
pub fn state_path() -> PathBuf {
    crate::paths::beside_exe(STATE_FILE)
}

/// Reads the remembered corner.
pub fn load(path: &Path) -> Option<Placement> {
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

/// Writes the remembered corner.
pub fn store(path: &Path, placement: Placement) {
    let Ok(body) = toml::to_string(&placement) else { return };
    // Write beside, then rename.
    let tmp = path.with_extension("toml.tmp");
    if std::fs::write(&tmp, format!("{STATE_HEADER}{body}")).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full screen, no taskbar.
    const SCREEN: Area = Area { left: 0, top: 0, right: 1920, bottom: 1080 };
    /// The same screen with a 40 pixel taskbar.
    const DESKTOP: Area = Area { left: 0, top: 0, right: 1920, bottom: 1040 };

    fn state_file(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("chibipop_window_{}_{name}.toml", std::process::id()))
    }

    #[test]
    fn a_corner_that_clears_the_taskbar_is_kept() {
        assert_eq!((100, 60), DESKTOP.fit((100, 60), (760, 900)));
    }

    #[test]
    fn a_window_as_tall_as_the_work_area_takes_its_top() {
        assert_eq!((300, 0), DESKTOP.fit((300, 40), (760, 1040)));
    }

    #[test]
    fn a_corner_below_the_work_area_moves_up_to_the_last_row() {
        assert_eq!((500, 640), DESKTOP.fit((500, 900), (760, 400)));
    }

    #[test]
    fn a_corner_left_of_the_work_area_moves_right() {
        assert_eq!((0, 200), DESKTOP.fit((-120, 200), (760, 400)));
    }

    #[test]
    fn a_window_larger_than_the_work_area_aligns_to_the_top_left() {
        assert_eq!((0, 0), SCREEN.fit((400, 400), (2400, 1400)));
    }

    #[test]
    fn a_corner_from_a_removed_monitor_moves_onto_the_nearest_one() {
        assert_eq!((1160, 640), DESKTOP.fit((3400, 700), (760, 400)));
    }

    #[test]
    fn a_stored_corner_round_trips() {
        let path = state_file("round_trip");
        let _ = std::fs::remove_file(&path);
        store(&path, Placement { x: -1200, y: 40 });
        assert_eq!(Some(Placement { x: -1200, y: 40 }), load(&path));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_stored_file_names_its_writer() {
        let path = state_file("header");
        let _ = std::fs::remove_file(&path);
        store(&path, Placement { x: 1, y: 2 });
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(STATE_HEADER), "{text:?}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_file_and_a_broken_file_read_as_no_corner() {
        let missing = state_file("missing");
        let _ = std::fs::remove_file(&missing);
        assert_eq!(None, load(&missing));
        let broken = state_file("broken");
        std::fs::write(&broken, "x = left\n").unwrap();
        assert_eq!(None, load(&broken));
        let _ = std::fs::remove_file(&broken);
    }

    #[test]
    fn a_stored_corner_is_written_beside_the_executable() {
        let path = state_path();
        let exe = std::env::current_exe().unwrap();
        assert_eq!(exe.parent().unwrap(), path.parent().unwrap());
        assert!(path.ends_with(STATE_FILE), "{path:?}");
    }
}
