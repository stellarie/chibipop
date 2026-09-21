//! The remembered position of the settings window.
//!
//! Windows restores no window position by itself. The settings window is sized
//! to its content, so a default top edge near the top of a monitor pushes the
//! bottom edge of the window off the screen. Issue #112 reports that crop.
//! This module holds the top-left corner of the window between runs, and it
//! trims a corner that leaves a work area.
//!
//! The shared `Config` was rejected for this corner. A window corner is a
//! Windows-only value, and ARCHITECTURE.md#settings-and-config refuses a
//! platform-interpreted field there. The Windows registry was rejected because
//! it needs a new dependency.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The file that holds the corner. It sits beside `chibipop.toml`.
const STATE_FILE: &str = "chibipop.window.toml";

/// The line that explains the file to a reader.
const STATE_HEADER: &str = "# The position of the settings window. chibipop writes this file.\n";

/// The top-left corner of the settings window.
///
/// The values are physical pixels in screen coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    /// The distance from the left edge of the virtual screen.
    pub x: i32,
    /// The distance from the top edge of the virtual screen.
    pub y: i32,
}

/// The part of a monitor that a window may cover.
///
/// A work area excludes the taskbar and every other docked bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Area {
    /// Returns the corner that keeps a window of `size` inside this area.
    ///
    /// A window larger than the area aligns to the top-left corner of the
    /// area. Every other position hides the top edge or the left edge as well.
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
///
/// A missing file, an unreadable file, and a malformed file all return `None`.
/// The window then opens at the position that the system chooses.
pub fn load(path: &Path) -> Option<Placement> {
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

/// Writes the remembered corner.
///
/// A failed write is not fatal. The window works without the file.
pub fn store(path: &Path, placement: Placement) {
    let Ok(body) = toml::to_string(&placement) else { return };
    // A torn write loses the corner only. Write beside, then rename.
    let tmp = path.with_extension("toml.tmp");
    if std::fs::write(&tmp, format!("{STATE_HEADER}{body}")).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A screen of 1920 by 1080 without a taskbar.
    const SCREEN: Area = Area { left: 0, top: 0, right: 1920, bottom: 1080 };
    /// The same screen with a 40 pixel taskbar at the bottom.
    const DESKTOP: Area = Area { left: 0, top: 0, right: 1920, bottom: 1040 };

    fn state_file(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("chibipop_window_{}_{name}.toml", std::process::id()))
    }

    #[test]
    fn a_corner_that_clears_the_taskbar_is_kept() {
        assert_eq!((100, 60), DESKTOP.fit((100, 60), (760, 900)));
    }

    /// The reported crop. A window as tall as the work area can start at one
    /// row only, and that row is the top edge of the work area.
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

    /// A window wider than the screen must expose its top-left corner.
    #[test]
    fn a_window_larger_than_the_work_area_aligns_to_the_top_left() {
        assert_eq!((0, 0), SCREEN.fit((400, 400), (2400, 1400)));
    }

    /// A corner on a monitor that is no longer attached must come back.
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

    /// A hand-edited file must not stop the window from opening.
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
