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
    #[serde(default)]
    pub anki: AnkiConfig,
}

/// `[trigger]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerConfig {
    pub mode: TriggerMode,
    /// Which key gates popups.
    #[serde(default = "default_trigger_key")]
    pub trigger_key: String,
}

/// `"shift"` for backwards compat.
fn default_trigger_key() -> String {
    "shift".to_string()
}

/// `kebab-case` for the TOML.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TriggerMode {
    Live,
    /// Popup while a key is held.
    HoldKey,
    /// Legacy; maps to `HoldKey`.
    #[serde(rename = "hold-shift")]
    HoldShift,
}

/// VK code from a key name.
pub fn parse_trigger_key(name: &str) -> Option<u16> {
    let lower = name.to_ascii_lowercase();
    let named = match lower.as_str() {
        "shift" => Some(0x10),
        "ctrl" | "control" => Some(0x11),
        "alt" => Some(0x12),
        "f1" => Some(0x70),
        "f2" => Some(0x71),
        "f3" => Some(0x72),
        "f4" => Some(0x73),
        "f5" => Some(0x74),
        "f6" => Some(0x75),
        "f7" => Some(0x76),
        "f8" => Some(0x77),
        "f9" => Some(0x78),
        "f10" => Some(0x79),
        "f11" => Some(0x7A),
        "f12" => Some(0x7B),
        _ => single_char_vk(&lower),
    };
    named.or_else(|| parse_vk_number(name))
}

/// A lone letter or digit key.
fn single_char_vk(lower: &str) -> Option<u16> {
    let mut chars = lower.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    match c {
        'a'..='z' => Some(0x41 + (c as u16 - 'a' as u16)),
        '0'..='9' => Some(0x30 + (c as u16 - '0' as u16)),
        _ => None,
    }
}

/// Hex or decimal VK number.
fn parse_vk_number(name: &str) -> Option<u16> {
    let s = name.trim();
    match s.strip_prefix("0x").or(s.strip_prefix("0X")) {
        Some(hex) => u16::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

/// Display name from a VK code.
pub fn trigger_key_name(vk: u16) -> String {
    match vk {
        0x10 => "Shift".into(),
        0x11 => "Ctrl".into(),
        0x12 => "Alt".into(),
        0x70..=0x7B => format!("F{}", vk - 0x6F),
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        0x20 => "Space".into(),
        0x1B => "Esc".into(),
        0x09 => "Tab".into(),
        0x14 => "CapsLock".into(),
        _ => format!("Key 0x{vk:02X}"),
    }
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
    /// Collapsed rows beside, not below.
    #[serde(default)]
    pub side_panel: bool,
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
    /// Tall capture for manga/VN.
    #[serde(default)]
    pub prefer_vertical: bool,
}

/// 1: tiling is off by default.
fn default_max_ocr_passes() -> u8 {
    1
}

impl Default for OcrConfig {
    fn default() -> OcrConfig {
        OcrConfig {
            max_ocr_passes: default_max_ocr_passes(),
            prefer_vertical: false,
        }
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

/// Maps one field to Anki.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldMapping {
    pub anki_field: String,
    pub source: String,
}

/// `[anki]`. Optional section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnkiConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_anki_url")]
    pub url: String,
    #[serde(default = "default_anki_deck")]
    pub deck: String,
    #[serde(default = "default_anki_model")]
    pub model: String,
    /// Shortcut: add the top card.
    #[serde(default = "default_anki_add_key")]
    pub add_key: String,
    /// Which fields go where.
    #[serde(default = "default_field_map")]
    pub field_map: Vec<FieldMapping>,
}

/// Default Anki URL.
fn default_anki_url() -> String {
    "http://localhost:8765".to_string()
}

/// Default Anki deck name.
fn default_anki_deck() -> String {
    "Default".to_string()
}

/// Default Anki model name.
fn default_anki_model() -> String {
    "Lapis".to_string()
}

/// Default Anki add key.
fn default_anki_add_key() -> String {
    "a".to_string()
}

/// The Lapis field mapping.
fn default_field_map() -> Vec<FieldMapping> {
    vec![
        FieldMapping { anki_field: "Expression".into(), source: "expression".into() },
        FieldMapping { anki_field: "ExpressionReading".into(), source: "reading".into() },
        FieldMapping { anki_field: "Glossary".into(), source: "glossary".into() },
        FieldMapping { anki_field: "Frequency".into(), source: "frequency".into() },
        FieldMapping { anki_field: "FreqSort".into(), source: "frequency".into() },
    ]
}

impl Default for AnkiConfig {
    fn default() -> AnkiConfig {
        AnkiConfig {
            enabled: false,
            url: default_anki_url(),
            deck: default_anki_deck(),
            model: default_anki_model(),
            add_key: default_anki_add_key(),
            field_map: default_field_map(),
        }
    }
}

impl Default for Config {
    /// Spec §4.3's shipped values.
    fn default() -> Config {
        Config {
            trigger: TriggerConfig {
                mode: TriggerMode::Live,
                trigger_key: default_trigger_key(),
            },
            popup: PopupConfig {
                theme: "dark".to_string(),
                exclude_from_capture: false,
                max_width_percent: default_max_width_percent(),
                max_height_percent: 45,
                summary_chars: 40,
                font: "Yu Gothic UI".to_string(),
                highlight_match: default_highlight_match(),
                scroll_popup: default_scroll_popup(),
                side_panel: false,
            },
            dictionaries: DictionariesConfig {
                display_order: vec!["大辞林".to_string(), "Jitendex".to_string()],
            },
            ocr: OcrConfig::default(),
            debug: DebugConfig::default(),
            anki: AnkiConfig::default(),
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
            // Migrate legacy hold-shift.
            if config.trigger.mode == TriggerMode::HoldShift {
                config.trigger.mode = TriggerMode::HoldKey;
                config.trigger.trigger_key = "shift".to_string();
            }
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
        c.trigger.mode = TriggerMode::HoldKey;
        c.save(&p).unwrap();
        let back = load_or_create(&p).unwrap();
        assert_eq!(TriggerMode::HoldKey, back.trigger.mode);
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

    #[test]
    fn prefer_vertical_defaults_to_false() {
        assert!(!Config::default().ocr.prefer_vertical);
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
    fn prefer_vertical_round_trips() {
        let p = tmp("prefer_vert");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.ocr.prefer_vertical = true;
        c.save(&p).unwrap();
        assert!(load_or_create(&p).unwrap().ocr.prefer_vertical);
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

    #[test]
    fn side_panel_defaults_off() {
        assert!(!Config::default().popup.side_panel);
    }

    #[test]
    fn enabled_side_panel_round_trips() {
        let p = tmp("side_on");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.popup.side_panel = true;
        c.save(&p).unwrap();
        assert!(load_or_create(&p).unwrap().popup.side_panel);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_config_without_side_panel_loads_with_it_off() {
        let p = tmp("no_side_panel");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\"]\n",
        )).unwrap();
        let c = load_or_create(&p).expect("a pre-side_panel config must load");
        assert!(!c.popup.side_panel);
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

    #[test]
    fn anki_config_round_trips() {
        let p = tmp("anki_rt");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.anki.enabled = true;
        c.anki.deck = "Mining".to_string();
        c.anki.add_key = "f2".to_string();
        c.save(&p).unwrap();
        let back = load_or_create(&p).unwrap();
        assert!(back.anki.enabled);
        assert_eq!("Mining", back.anki.deck);
        assert_eq!("http://localhost:8765", back.anki.url);
        assert_eq!("Lapis", back.anki.model);
        assert_eq!("f2", back.anki.add_key);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_config_without_anki_section_loads() {
        let p = tmp("no_anki");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\"]\n",
        )).unwrap();
        let c = load_or_create(&p).expect("a pre-anki config must load");
        assert!(!c.anki.enabled);
        assert_eq!("http://localhost:8765", c.anki.url);
        assert_eq!("a", c.anki.add_key);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn anki_add_key_defaults_to_a() {
        assert_eq!("a", Config::default().anki.add_key);
    }

    /// Guards the shipped default.
    #[test]
    fn anki_add_key_default_parses_to_vk_a() {
        let vk = parse_trigger_key(&Config::default().anki.add_key);
        assert_eq!(Some(0x41), vk);
    }

    /// The bare-serde-default trap.
    #[test]
    fn an_anki_section_without_add_key_still_defaults_to_a() {
        let p = tmp("anki_no_add_key");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\"]\n\n",
            "[anki]\nenabled = true\n",
        )).unwrap();
        let c = load_or_create(&p).expect("a pre-add_key config must load");
        assert!(c.anki.enabled, "the rest of the section must still apply");
        assert_eq!("a", c.anki.add_key, "a missing key takes the field default");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn default_field_map_matches_lapis() {
        let want = vec![
            FieldMapping { anki_field: "Expression".into(), source: "expression".into() },
            FieldMapping { anki_field: "ExpressionReading".into(), source: "reading".into() },
            FieldMapping { anki_field: "Glossary".into(), source: "glossary".into() },
            FieldMapping { anki_field: "Frequency".into(), source: "frequency".into() },
            FieldMapping { anki_field: "FreqSort".into(), source: "frequency".into() },
        ];
        assert_eq!(want, Config::default().anki.field_map);
    }

    #[test]
    fn anki_field_map_round_trips() {
        let p = tmp("field_map_rt");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.anki.field_map = vec![
            FieldMapping { anki_field: "Front".into(), source: "expression".into() },
            FieldMapping { anki_field: "Back".into(), source: "glossary".into() },
        ];
        c.save(&p).unwrap();
        let back = load_or_create(&p).unwrap();
        assert_eq!(c.anki.field_map, back.anki.field_map);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_config_without_anki_section_defaults_field_map() {
        let p = tmp("no_anki_field_map");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\"]\n",
        )).unwrap();
        let c = load_or_create(&p).expect("a pre-anki config must load");
        assert_eq!(Config::default().anki.field_map, c.anki.field_map);
        let _ = std::fs::remove_file(&p);
    }

    /// The bare-serde-default trap.
    #[test]
    fn an_anki_section_without_field_map_still_defaults_to_lapis() {
        let p = tmp("anki_no_field_map");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\"]\n\n",
            "[anki]\nenabled = true\n",
        )).unwrap();
        let c = load_or_create(&p).expect("a pre-field_map config must load");
        assert!(c.anki.enabled, "the rest of the section must still apply");
        assert_eq!(
            Config::default().anki.field_map, c.anki.field_map,
            "a missing key takes the field default"
        );
        let _ = std::fs::remove_file(&p);
    }

    // ---- trigger key ----

    #[test]
    fn parse_trigger_key_shift() {
        assert_eq!(Some(0x10), parse_trigger_key("shift"));
    }

    #[test]
    fn parse_trigger_key_case_insensitive() {
        assert_eq!(Some(0x10), parse_trigger_key("SHIFT"));
        assert_eq!(Some(0x10), parse_trigger_key("Shift"));
    }

    #[test]
    fn parse_trigger_key_ctrl() {
        assert_eq!(Some(0x11), parse_trigger_key("ctrl"));
        assert_eq!(Some(0x11), parse_trigger_key("control"));
    }

    #[test]
    fn parse_trigger_key_alt() {
        assert_eq!(Some(0x12), parse_trigger_key("alt"));
    }

    #[test]
    fn parse_trigger_key_f1() {
        assert_eq!(Some(0x70), parse_trigger_key("f1"));
    }

    #[test]
    fn parse_trigger_key_f12() {
        assert_eq!(Some(0x7B), parse_trigger_key("f12"));
    }

    #[test]
    fn parse_trigger_key_lowercase_letter() {
        assert_eq!(Some(0x41), parse_trigger_key("a"));
    }

    #[test]
    fn parse_trigger_key_uppercase_letter() {
        assert_eq!(Some(0x41), parse_trigger_key("A"));
    }

    #[test]
    fn parse_trigger_key_digit_key() {
        assert_eq!(Some(0x35), parse_trigger_key("5"));
    }

    /// Agrees with the display name.
    #[test]
    fn parse_trigger_key_single_char_matches_trigger_key_name() {
        for c in 'a'..='z' {
            let vk = parse_trigger_key(&c.to_string()).unwrap();
            assert_eq!(c.to_ascii_uppercase().to_string(), trigger_key_name(vk));
        }
        for c in '0'..='9' {
            let vk = parse_trigger_key(&c.to_string()).unwrap();
            assert_eq!(c.to_string(), trigger_key_name(vk));
        }
    }

    #[test]
    fn parse_trigger_key_garbage() {
        assert_eq!(None, parse_trigger_key("garbage"));
    }

    #[test]
    fn parse_trigger_key_hex() {
        assert_eq!(Some(0x41), parse_trigger_key("0x41"));
    }

    #[test]
    fn parse_trigger_key_hex_uppercase_prefix() {
        assert_eq!(Some(0x41), parse_trigger_key("0X41"));
    }

    #[test]
    fn parse_trigger_key_decimal() {
        assert_eq!(Some(0x41), parse_trigger_key("65"));
    }

    /// Overflows `u16`: not silently wrapped.
    #[test]
    fn parse_trigger_key_out_of_range_decimal_is_rejected() {
        assert_eq!(None, parse_trigger_key("99999"));
    }

    #[test]
    fn trigger_key_name_round_trips() {
        for (name, want) in &[
            ("shift", "Shift"), ("ctrl", "Ctrl"), ("alt", "Alt"),
            ("f1", "F1"), ("f2", "F2"), ("f3", "F3"), ("f4", "F4"),
            ("f5", "F5"), ("f6", "F6"), ("f7", "F7"), ("f8", "F8"),
            ("f9", "F9"), ("f10", "F10"), ("f11", "F11"), ("f12", "F12"),
        ] {
            let vk = parse_trigger_key(name).unwrap();
            assert_eq!(*want, trigger_key_name(vk));
        }
    }

    #[test]
    fn trigger_key_name_letter() {
        assert_eq!("A", trigger_key_name(0x41));
    }

    #[test]
    fn trigger_key_name_digit() {
        assert_eq!("5", trigger_key_name(0x35));
    }

    #[test]
    fn trigger_key_name_space() {
        assert_eq!("Space", trigger_key_name(0x20));
    }

    #[test]
    fn trigger_key_name_named_specials() {
        assert_eq!("Esc", trigger_key_name(0x1B));
        assert_eq!("Tab", trigger_key_name(0x09));
        assert_eq!("CapsLock", trigger_key_name(0x14));
    }

    #[test]
    fn trigger_key_name_unknown_falls_back_to_hex() {
        assert_eq!("Key 0xBA", trigger_key_name(0xBA));
    }

    #[test]
    fn trigger_key_round_trips_in_config() {
        let p = tmp("trigger_key_rt");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.trigger.mode = TriggerMode::HoldKey;
        c.trigger.trigger_key = "ctrl".to_string();
        c.save(&p).unwrap();
        let back = load_or_create(&p).unwrap();
        assert_eq!(TriggerMode::HoldKey, back.trigger.mode);
        assert_eq!("ctrl", back.trigger.trigger_key);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn old_hold_shift_config_migrates() {
        let p = tmp("legacy_hold_shift");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"hold-shift\"\n\n",
            "[popup]\ntheme = \"dark\"\n",
            "exclude_from_capture = false\n",
            "max_height_percent = 45\n",
            "summary_chars = 40\n",
            "font = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\n",
            "display_order = [\"大辞林\"]\n",
        )).unwrap();
        let c = load_or_create(&p).unwrap();
        assert_eq!(TriggerMode::HoldKey, c.trigger.mode);
        assert_eq!("shift", c.trigger.trigger_key);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn trigger_key_defaults_to_shift() {
        let p = tmp("no_trigger_key");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"hold-key\"\n\n",
            "[popup]\ntheme = \"dark\"\n",
            "exclude_from_capture = false\n",
            "max_height_percent = 45\n",
            "summary_chars = 40\n",
            "font = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\n",
            "display_order = [\"大辞林\"]\n",
        )).unwrap();
        let c = load_or_create(&p).unwrap();
        assert_eq!(TriggerMode::HoldKey, c.trigger.mode);
        assert_eq!("shift", c.trigger.trigger_key);
        let _ = std::fs::remove_file(&p);
    }
}
