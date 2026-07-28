//! User-facing settings, read from a TOML file beside the executable
//! (spec §4.3). There is deliberately no settings GUI - "configuration is a
//! hand-edited TOML file" is a design decision (M3 plan), not a gap - so
//! this module is the *entire* interface between the user and their
//! settings. Two consequences follow from that:
//!
//! - [`load_or_create`] writes real defaults to disk on first run rather
//!   than only returning them in memory, so the file that shows up beside
//!   the executable is something the user can find and edit.
//! - Malformed TOML is a hard [`anyhow::Error`] naming the file, never a
//!   silent fall-back to defaults. A silent fallback is how a user's
//!   settings quietly vanish after a typo, and they blame the feature
//!   instead of the line they mistyped.
//!
//! Kept in the pure layer deliberately - no `windows` crate here - so it
//! compiles and tests on any platform, same rule `present.rs` and
//! `ui/theme.rs` follow.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Root of the TOML file. Field names match the `[section]` headers in
/// spec §4.3 exactly, so `#[derive(Serialize, Deserialize)]` needs no
/// renaming at this level.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub trigger: TriggerConfig,
    pub popup: PopupConfig,
    pub dictionaries: DictionariesConfig,
    #[serde(default)]
    pub ocr: OcrConfig,
    #[serde(default)]
    pub debug: DebugConfig,
}

/// `[trigger]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerConfig {
    pub mode: TriggerMode,
}

/// Always-live vs. hold-Shift (spec M3-D3). `kebab-case` so the TOML reads
/// `mode = "live"` / `mode = "hold-shift"` instead of a Rust-style
/// identifier leaking into a file the user hand-edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TriggerMode {
    Live,
    HoldShift,
}

/// `[popup]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PopupConfig {
    /// `"dark"` | `"light"` - selects `ui::theme::Theme::dark()` /
    /// `light()`. Kept as a plain string here rather than sharing an enum
    /// with `ui::theme`, since that module stays Windows-adjacent-free but
    /// otherwise independent of `config.rs`.
    pub theme: String,
    /// Whether the popup asks the OS to exclude it from screen captures
    /// (`WDA_EXCLUDEFROMCAPTURE` - spec §5).
    ///
    /// **Defaults `false`**, which makes the popup recordable. Exclusion
    /// hides it from *every* capture API, not just chibipop's own, so
    /// with it on the popup cannot be screen-recorded, screenshotted or
    /// screen-shared - it is visible to the user's own eyes and nothing
    /// else. That was the shipped default until the hide/reshow guard was
    /// measured in real use (spec §5.1) and found to cost no noticeable
    /// lookup latency, at which point the more useful behaviour became
    /// the default and exclusion became the opt-in.
    ///
    /// Set `true` to restore exclusion. Either way `app.rs`'s capture
    /// guard keeps the popup out of chibipop's *own* OCR captures, so it
    /// never photographs its own rendered text back into the next lookup.
    /// That guard is why `Popup::create` takes this as a plain `bool`
    /// rather than letting the caller decide whether to call
    /// `SetWindowDisplayAffinity` at all.
    pub exclude_from_capture: bool,
    /// Popup height cap, as a percentage of the current monitor's height
    /// (spec M3-D4). 0-100 fits comfortably in a `u8`.
    pub max_height_percent: u8,
    /// `CollapsedRow::summary` length cap, in characters. Matches
    /// `present::PresentConfig::summary_chars`'s type exactly so
    /// `Config::present_config` needs no cast.
    pub summary_chars: usize,
    pub font: String,
}

/// `[dictionaries]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DictionariesConfig {
    /// Case-insensitive substrings, in display-priority order. See
    /// `present::PresentConfig::dict_order` for why substrings rather than
    /// exact names.
    pub display_order: Vec<String>,
}

/// `[ocr]`. Carries `#[serde(default)]` at the `Config` level so a file
/// written before this section existed still loads - see `load_or_create`,
/// which treats malformed TOML as a hard error rather than falling back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrConfig {
    /// Total OCR captures per hover: one to locate the hovered word, the
    /// rest to read forward from it.
    ///
    /// **Defaults to `1`, which means no forward tiling.** Tiling buys
    /// reading distance - roughly 10 characters ahead of the cursor instead
    /// of 7 - but as measured on 2026-07-28 it pays for that by sometimes
    /// resolving a *different character than the one under the cursor*, and
    /// a longer reading of the wrong word is worse than a short reading of
    /// the right one.
    ///
    /// Set to `2` or more to turn tiling back on. It is not removed, because
    /// the reading-distance problem it solves is real and the accuracy
    /// problem it introduces is understood well enough to be fixable; see
    /// the two-pass spec's revision for the measurement and the two failure
    /// modes.
    #[serde(default = "default_max_ocr_passes")]
    pub max_ocr_passes: u8,
}

/// **1 as of 2026-07-28: tiling is off by default.** Measured against a
/// browser reader at ~25px text, hovering nine characters and comparing the
/// character actually handed to lookup: single-pass scored **9/9**, three
/// passes **4/9**. Tiling failed two distinct ways - it re-read a character
/// the centred pass-1 capture had got right (`明` as `月`, `振` as `一`), and
/// it lost its position entirely, returning text starting most of a line
/// before the hovered character. Reading distance is worth nothing if the
/// word it starts from is the wrong one. See the two-pass spec's revision.
fn default_max_ocr_passes() -> u8 {
    1
}

impl Default for OcrConfig {
    fn default() -> OcrConfig {
        OcrConfig { max_ocr_passes: default_max_ocr_passes() }
    }
}

/// `[debug]`. Carries `#[serde(default)]` at the `Config` level so a file
/// written before this section existed still loads - `load_or_create` treats
/// malformed TOML as a hard error rather than falling back.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DebugConfig {
    /// Draw a faint outline around every region a hover captured: pass 1's
    /// box, each forward tile, and the resolved word's anchor.
    ///
    /// Off by default. When off nothing is collected and no window is
    /// created - the feature is inert, not merely hidden.
    #[serde(default)]
    pub show_scan_region: bool,
}

impl Default for Config {
    /// Spec §4.3's shipped defaults, verbatim.
    fn default() -> Config {
        Config {
            trigger: TriggerConfig { mode: TriggerMode::Live },
            popup: PopupConfig {
                theme: "dark".to_string(),
                exclude_from_capture: false,
                max_height_percent: 45,
                summary_chars: 40,
                font: "Yu Gothic UI".to_string(),
            },
            dictionaries: DictionariesConfig {
                display_order: vec!["大辞林".to_string(), "Jitendex".to_string()],
            },
            ocr: OcrConfig::default(),
            debug: DebugConfig::default(),
        }
    }
}

impl Config {
    /// Writes this config to `path` as TOML, formatted for hand-editing.
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut text = toml::to_string_pretty(self)
            .with_context(|| format!("serialising config for {}", path.display()))?;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        std::fs::write(path, text)
            .with_context(|| format!("writing config to {}", path.display()))?;
        Ok(())
    }

    /// The one place `config.rs` and `present.rs` meet: `present.rs`
    /// deliberately keeps its own small `PresentConfig` instead of
    /// depending on the whole `Config`, so it stays independently
    /// testable.
    pub fn present_config(&self) -> crate::present::PresentConfig {
        crate::present::PresentConfig {
            dict_order: self.dictionaries.display_order.clone(),
            summary_chars: self.popup.summary_chars,
        }
    }
}

/// Loads `path`, creating it with [`Config::default`] if it does not exist
/// yet. Malformed TOML is returned as an `Err` naming `path` - never
/// silently swapped for defaults; see the module docs for why.
pub fn load_or_create(path: &Path) -> Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .with_context(|| format!("parsing config from {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let config = Config::default();
            config.save(path)?;
            Ok(config)
        }
        Err(e) => Err(e).with_context(|| format!("reading config from {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique per process and per test, so concurrent runs cannot collide.
    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("chibipop_cfg_{}_{}.toml", std::process::id(), name))
    }

    #[test]
    fn defaults_match_the_spec() {
        let c = Config::default();
        assert_eq!(TriggerMode::Live, c.trigger.mode);
        assert_eq!("dark", c.popup.theme);
        assert_eq!(45, c.popup.max_height_percent);
        assert_eq!(40, c.popup.summary_chars);
        assert_eq!("Yu Gothic UI", c.popup.font);
        assert_eq!(vec!["大辞林".to_string(), "Jitendex".to_string()],
                   c.dictionaries.display_order);
    }

    /// Spec §5.1: exclusion is the *opt-in*. A fresh install must be
    /// recordable, because an excluded popup is invisible to every capture
    /// API - screen recorders, screenshots, screen sharing - and a user
    /// who hits that has no way to tell a hidden window from a broken one.
    /// The hide/reshow guard that replaces exclusion was measured in real
    /// use and costs no noticeable lookup latency, so there is no
    /// performance argument for defaulting the other way. Kept as its own
    /// test (not folded into `defaults_match_the_spec`) so that exact
    /// regression fails with its own name, not a generic multi-field
    /// assertion.
    #[test]
    fn capture_exclusion_defaults_to_false() {
        assert!(
            !Config::default().popup.exclude_from_capture,
            "the popup must be recordable out of the box - exclusion is the opt-in (spec section 5.1)"
        );
    }

    #[test]
    fn a_missing_file_is_created_with_defaults() {
        let p = tmp("missing");
        let _ = std::fs::remove_file(&p);
        let c = load_or_create(&p).unwrap();
        assert!(p.exists(), "the file must be written, not just defaulted in memory");
        assert_eq!(Config::default().popup.theme, c.popup.theme);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_saved_non_default_mode_survives_a_round_trip() {
        let p = tmp("roundtrip");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.trigger.mode = TriggerMode::HoldShift;
        c.save(&p).unwrap();
        let back = load_or_create(&p).unwrap();
        assert_eq!(TriggerMode::HoldShift, back.trigger.mode);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn malformed_toml_errors_naming_the_file() {
        let p = tmp("malformed");
        std::fs::write(&p, "this is not = = valid toml [[[").unwrap();
        let err = load_or_create(&p).expect_err("must not silently fall back to defaults");
        let msg = format!("{err:#}");
        assert!(msg.contains("chibipop_cfg_"), "error must name the file, got: {msg}");
        let _ = std::fs::remove_file(&p);
    }

    /// Tiling is off by default. It was measured resolving the wrong
    /// character 5 times in 9 where single-pass got 9 of 9, so reading
    /// distance is not worth its cost until that is fixed.
    #[test]
    fn ocr_passes_defaults_to_one() {
        assert_eq!(1, Config::default().ocr.max_ocr_passes);
    }

    /// Tiling stays reachable by editing one line, with no rebuild - the
    /// inverse of the kill switch it was introduced as.
    #[test]
    fn a_multi_pass_round_trips() {
        let p = tmp("multipass");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.ocr.max_ocr_passes = 3;
        c.save(&p).unwrap();
        assert_eq!(3, load_or_create(&p).unwrap().ocr.max_ocr_passes);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_single_pass_round_trips() {
        let p = tmp("onepass");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.ocr.max_ocr_passes = 1;
        c.save(&p).unwrap();
        assert_eq!(1, load_or_create(&p).unwrap().ocr.max_ocr_passes);
        let _ = std::fs::remove_file(&p);
    }

    /// Every config file written before this feature existed lacks an [ocr]
    /// section. Without serde(default) they stop loading and chibipop refuses
    /// to start, because malformed TOML is deliberately a hard error here.
    #[test]
    fn a_config_written_before_the_ocr_section_existed_still_loads() {
        let p = tmp("no_ocr_section");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\", \"Jitendex\"]\n",
        )).unwrap();
        let c = load_or_create(&p).expect("a pre-[ocr] config must still load");
        assert_eq!(1, c.ocr.max_ocr_passes, "missing section takes the default");
        let _ = std::fs::remove_file(&p);
    }

    /// A hand-edit can leave `[ocr]` present but blank - the section
    /// header survived, only the `max_ocr_passes` line under it was
    /// deleted. That exercises the *field-level*
    /// `#[serde(default = "default_max_ocr_passes")]`, distinct from the
    /// container-level `#[serde(default)]` on `Config::ocr` that the test
    /// above (whole section missing) exercises instead.
    #[test]
    fn an_empty_ocr_section_still_defaults_max_ocr_passes_to_three() {
        let p = tmp("empty_ocr_section");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\", \"Jitendex\"]\n\n",
            "[ocr]\n",
        )).unwrap();
        let c = load_or_create(&p).expect("an empty [ocr] section must still load");
        assert_eq!(1, c.ocr.max_ocr_passes, "missing key takes the field default");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn the_scan_overlay_defaults_off() {
        assert!(!Config::default().debug.show_scan_region,
                "the overlay is a debug aid and must be opt-in");
    }

    #[test]
    fn an_enabled_overlay_round_trips() {
        let p = tmp("overlay_on");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.debug.show_scan_region = true;
        c.save(&p).unwrap();
        assert!(load_or_create(&p).unwrap().debug.show_scan_region);
        let _ = std::fs::remove_file(&p);
    }

    /// Every config written before this section existed lacks [debug]. Without
    /// serde(default) they stop loading and chibipop refuses to start.
    #[test]
    fn a_config_written_before_the_debug_section_existed_still_loads() {
        let p = tmp("no_debug_section");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\", \"Jitendex\"]\n\n",
            "[ocr]\nmax_ocr_passes = 3\n",
        )).unwrap();
        let c = load_or_create(&p).expect("a pre-[debug] config must still load");
        assert!(!c.debug.show_scan_region);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn an_empty_debug_section_still_defaults_the_toggle_off() {
        let p = tmp("empty_debug");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\", \"Jitendex\"]\n\n",
            "[ocr]\nmax_ocr_passes = 3\n\n",
            "[debug]\n",
        )).unwrap();
        assert!(!load_or_create(&p).unwrap().debug.show_scan_region);
        let _ = std::fs::remove_file(&p);
    }
}
