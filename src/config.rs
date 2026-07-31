//! Settings, from a TOML file.
//!
//! No `windows` crate here.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Width cap, % of monitor.
pub const MAX_WIDTH_RANGE: (u8, u8) = (10, 90);
/// Height cap, % of monitor.
pub const MAX_HEIGHT_RANGE: (u8, u8) = (10, 90);
/// Summary length, in chars.
pub const SUMMARY_RANGE: (usize, usize) = (10, 200);
/// OCR captures per hover.
pub const PASSES_RANGE: (u8, u8) = (1, 5);

/// Root of the TOML file.
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

/// `kebab-case` for the TOML.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TriggerMode {
    Live,
    HoldShift,
}

/// `[popup]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PopupConfig {
    /// `"dark"` | `"light"`.
    pub theme: String,
    /// Hide from screen capture.
    ///
    /// Off - recordable by default.
    pub exclude_from_capture: bool,
    /// Width cap, % of monitor.
    #[serde(default = "default_max_width_percent")]
    pub max_width_percent: u8,
    /// Height cap, % of monitor.
    pub max_height_percent: u8,
    /// Summary length, in chars.
    pub summary_chars: usize,
    pub font: String,
    /// Box the word being defined.
    #[serde(default = "default_highlight_match")]
    pub highlight_match: bool,
    /// Wheel-scroll a long popup.
    #[serde(default = "default_scroll_popup")]
    pub scroll_popup: bool,
}

/// 25% of the monitor.
fn default_max_width_percent() -> u8 {
    25
}

/// On by default.
fn default_highlight_match() -> bool {
    true
}

/// On by default.
fn default_scroll_popup() -> bool {
    true
}

/// `[dictionaries]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DictionariesConfig {
    /// Substrings, priority order.
    pub display_order: Vec<String>,
}

/// `[ocr]`. Optional section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrConfig {
    /// Captures per hover.
    ///
    /// 1 = no tiling, the default.
    #[serde(default = "default_max_ocr_passes")]
    pub max_ocr_passes: u8,
}

/// 1: tiling is off by default.
fn default_max_ocr_passes() -> u8 {
    1
}

impl Default for OcrConfig {
    fn default() -> OcrConfig {
        OcrConfig { max_ocr_passes: default_max_ocr_passes() }
    }
}

/// `[debug]`. Optional section.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DebugConfig {
    /// Outline what a hover captured.
    ///
    /// Off: inert, not just hidden.
    #[serde(default)]
    pub show_scan_region: bool,
    /// A console of each hover.
    #[serde(default)]
    pub show_lookup_log: bool,
}

impl Default for Config {
    /// Spec §4.3's shipped values.
    fn default() -> Config {
        Config {
            trigger: TriggerConfig { mode: TriggerMode::Live },
            popup: PopupConfig {
                theme: "dark".to_string(),
                exclude_from_capture: false,
                max_width_percent: default_max_width_percent(),
                max_height_percent: 45,
                summary_chars: 40,
                font: "Yu Gothic UI".to_string(),
                highlight_match: default_highlight_match(),
                scroll_popup: default_scroll_popup(),
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
    /// Writes the TOML.
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut text = toml::to_string_pretty(self)
            .with_context(|| format!("serialising config for {}", path.display()))?;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        // A torn write loses the lot.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)
            .with_context(|| format!("writing config to {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    /// Clamps bounded numbers.
    fn clamp_ranges(&mut self, path: &Path) {
        self.popup.max_width_percent = clamped(
            path,
            "max_width_percent",
            self.popup.max_width_percent,
            MAX_WIDTH_RANGE.0,
            MAX_WIDTH_RANGE.1,
        );
        self.popup.max_height_percent = clamped(
            path,
            "max_height_percent",
            self.popup.max_height_percent,
            MAX_HEIGHT_RANGE.0,
            MAX_HEIGHT_RANGE.1,
        );
        self.popup.summary_chars = clamped(
            path,
            "summary_chars",
            self.popup.summary_chars,
            SUMMARY_RANGE.0,
            SUMMARY_RANGE.1,
        );
        self.ocr.max_ocr_passes = clamped(
            path,
            "max_ocr_passes",
            self.ocr.max_ocr_passes,
            PASSES_RANGE.0,
            PASSES_RANGE.1,
        );
    }

    /// The bridge to `present.rs`.
    pub fn present_config(&self) -> crate::present::PresentConfig {
        crate::present::PresentConfig {
            dict_order: self.dictionaries.display_order.clone(),
            summary_chars: self.popup.summary_chars,
        }
    }
}

/// Clamps, naming any move.
fn clamped<T>(path: &Path, field: &str, value: T, lo: T, hi: T) -> T
where
    T: Ord + Copy + std::fmt::Display,
{
    let out = value.clamp(lo, hi);
    if out != value {
        let p = path.display();
        eprintln!("chibipop: {p}: {field} {value} is outside {lo}-{hi}, using {out}");
    }
    out
}

/// Loads, creating if absent.
///
/// Malformed TOML is an `Err`.
/// Out-of-range values clamp.
pub fn load_or_create(path: &Path) -> Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let mut config: Config = toml::from_str(&text)
                .with_context(|| format!("parsing config from {}", path.display()))?;
            config.clamp_ranges(path);
            Ok(config)
        }
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

    /// Unique per process and test.
    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("chibipop_cfg_{}_{}.toml", std::process::id(), name))
    }

    #[test]
    fn defaults_match_the_spec() {
        let c = Config::default();
        assert_eq!(TriggerMode::Live, c.trigger.mode);
        assert_eq!("dark", c.popup.theme);
        assert_eq!(25, c.popup.max_width_percent);
        assert_eq!(45, c.popup.max_height_percent);
        assert_eq!(40, c.popup.summary_chars);
        assert_eq!("Yu Gothic UI", c.popup.font);
        assert_eq!(vec!["大辞林".to_string(), "Jitendex".to_string()],
                   c.dictionaries.display_order);
    }

    /// §5.1: exclusion is opt-in.
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

    #[test]
    fn ocr_passes_defaults_to_one() {
        assert_eq!(1, Config::default().ocr.max_ocr_passes);
    }

    /// Re-enabled by one TOML line.
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

    /// A missing section must load.
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

    /// The field-level default.
    #[test]
    fn an_empty_ocr_section_still_defaults_max_ocr_passes_to_one() {
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

    /// A missing section must load.
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
    fn the_match_highlight_defaults_on() {
        assert!(Config::default().popup.highlight_match,
                "the highlight is the everyday answer to 'is this the word I am pointing at?'");
    }

    #[test]
    fn a_disabled_highlight_round_trips() {
        let p = tmp("highlight_off");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.popup.highlight_match = false;
        c.save(&p).unwrap();
        assert!(!load_or_create(&p).unwrap().popup.highlight_match);
        let _ = std::fs::remove_file(&p);
    }

    /// The bare-serde-default trap.
    #[test]
    fn a_config_written_before_the_highlight_existed_loads_with_it_on() {
        let p = tmp("no_highlight_field");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\", \"Jitendex\"]\n",
        )).unwrap();
        let c = load_or_create(&p).expect("a pre-highlight config must still load");
        assert!(c.popup.highlight_match, "a missing field must take the field default, not bool's");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn popup_scrolling_defaults_on() {
        assert!(Config::default().popup.scroll_popup);
    }

    #[test]
    fn disabled_scrolling_round_trips() {
        let p = tmp("scroll_off");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.popup.scroll_popup = false;
        c.save(&p).unwrap();
        assert!(!load_or_create(&p).unwrap().popup.scroll_popup);
        let _ = std::fs::remove_file(&p);
    }

    /// The same trap, for scrolling.
    #[test]
    fn a_config_written_before_scroll_popup_loads_with_it_on() {
        let p = tmp("no_scroll_field");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\", \"Jitendex\"]\n",
        )).unwrap();
        let c = load_or_create(&p).expect("a pre-scroll_popup config must still load");
        assert!(c.popup.scroll_popup, "a missing field must take the field default");
        let _ = std::fs::remove_file(&p);
    }

    /// Loading clamps too.
    #[test]
    fn an_out_of_range_hand_edit_is_clamped_on_load() {
        let p = tmp("out_of_range");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_width_percent = 0\nmax_height_percent = 0\n",
            "summary_chars = 5000\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\"]\n\n",
            "[ocr]\nmax_ocr_passes = 99\n",
        )).unwrap();
        let c = load_or_create(&p).expect("an out-of-range value must load, clamped");
        assert_eq!(MAX_WIDTH_RANGE.0, c.popup.max_width_percent);
        assert_eq!(MAX_HEIGHT_RANGE.0, c.popup.max_height_percent);
        assert_eq!(SUMMARY_RANGE.1, c.popup.summary_chars);
        assert_eq!(PASSES_RANGE.1, c.ocr.max_ocr_passes);
        let _ = std::fs::remove_file(&p);
    }

    /// In-range values must not move.
    #[test]
    fn an_in_range_value_survives_loading_untouched() {
        let p = tmp("in_range");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.popup.max_width_percent = 35;
        c.popup.max_height_percent = 70;
        c.popup.summary_chars = 25;
        c.ocr.max_ocr_passes = 3;
        c.save(&p).unwrap();
        let back = load_or_create(&p).unwrap();
        assert_eq!(35, back.popup.max_width_percent);
        assert_eq!(70, back.popup.max_height_percent);
        assert_eq!(25, back.popup.summary_chars);
        assert_eq!(3, back.ocr.max_ocr_passes);
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

    /// A pre-existing config loads.
    #[test]
    fn a_config_written_before_the_lookup_log_still_loads() {
        let p = tmp("no_lookup_log");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\"]\n\n",
            "[debug]\nshow_scan_region = false\n",
        )).unwrap();
        let c = load_or_create(&p).expect("a pre-lookup-log config must load");
        assert!(!c.debug.show_lookup_log);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_config_without_max_width_percent_defaults_to_25() {
        let p = tmp("no_width_field");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\"]\n",
        )).unwrap();
        let c = load_or_create(&p).expect("a pre-width config must load");
        assert_eq!(25, c.popup.max_width_percent);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn max_width_percent_round_trips() {
        let p = tmp("width_rt");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.popup.max_width_percent = 40;
        c.save(&p).unwrap();
        assert_eq!(40, load_or_create(&p).unwrap().popup.max_width_percent);
        let _ = std::fs::remove_file(&p);
    }
}
