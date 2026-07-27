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

impl Default for Config {
    /// Spec §4.3's shipped defaults, verbatim.
    fn default() -> Config {
        Config {
            trigger: TriggerConfig { mode: TriggerMode::Live },
            popup: PopupConfig {
                theme: "dark".to_string(),
                max_height_percent: 45,
                summary_chars: 40,
                font: "Yu Gothic UI".to_string(),
            },
            dictionaries: DictionariesConfig {
                display_order: vec!["大辞林".to_string(), "Jitendex".to_string()],
            },
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
}
