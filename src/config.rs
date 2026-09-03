//! This module loads and saves settings from a TOML file.
//!
//! It stays independent of the `windows` crate so the core remains platform-neutral.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// The allowed popup width as a percent of the monitor.
pub const MAX_WIDTH_RANGE: (u8, u8) = (10, 90);
/// The allowed popup height as a percent of the monitor.
pub const MAX_HEIGHT_RANGE: (u8, u8) = (10, 90);
/// The maximum summary length in characters.
pub const SUMMARY_RANGE: (usize, usize) = (10, 200);
/// The number of OCR captures for each hover.
pub const PASSES_RANGE: (u8, u8) = (1, 5);
/// The allowed capture width range in pixels.
pub const CAPTURE_W_RANGE: (i32, i32) = (100, 1600);

/// The allowed capture height range in pixels.
///
/// The lower limit sets the reach of the hit scan.
pub const CAPTURE_H_RANGE: (i32, i32) = (80, 600);

/// The root section of the TOML configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub trigger: TriggerConfig,
    pub popup: PopupConfig,
    pub dictionaries: DictionariesConfig,
    #[serde(default)]
    pub plugins: PluginsConfig,
    #[serde(default)]
    pub ocr: OcrConfig,
    #[serde(default)]
    pub debug: DebugConfig,
    #[serde(default)]
    pub anki: AnkiConfig,
    #[serde(default)]
    pub actions: ActionsConfig,
}

/// The `[trigger]` section of the configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerConfig {
    pub mode: TriggerMode,
    /// The key that controls popup display.
    #[serde(default = "default_trigger_key")]
    pub trigger_key: String,
    /// The chord that controls popup display on Linux.
    ///
    /// The value uses the XDG GlobalShortcuts preferred-binding syntax.
    /// The wlr-native channel treats this value as advisory because the compositor
    /// bind controls behavior.
    #[serde(default = "default_trigger_key_linux")]
    pub trigger_key_linux: String,
    /// Repeats a lookup for each character.
    #[serde(default)]
    pub per_character_lookup: bool,
}

/// Uses `"shift"` for backward compatibility.
fn default_trigger_key() -> String {
    "shift".to_string()
}

/// Uses a chord instead of a bare key because a portal binding is system-wide.
fn default_trigger_key_linux() -> String {
    "ALT+F".to_string()
}

pub fn default_ocr_language() -> String {
    "ja".to_string()
}

/// The TOML file uses `kebab-case` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TriggerMode {
    Live,
    /// Shows the popup while the user holds a key.
    HoldKey,
    /// Accepts a legacy name and maps it to `HoldKey`.
    #[serde(rename = "hold-shift")]
    HoldShift,
}

/// Returns the VK code for a key name.
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

/// Returns the VK code for one letter or digit.
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

/// Parses a hexadecimal or decimal VK number.
fn parse_vk_number(name: &str) -> Option<u16> {
    let s = name.trim();
    match s.strip_prefix("0x").or(s.strip_prefix("0X")) {
        Some(hex) => u16::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

/// Returns the display name for a VK code.
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

/// The `[popup]` section of the configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PopupConfig {
    /// The theme name. Use `"dark"` or `"light"`.
    pub theme: String,
    /// Hides the popup from screen capture.
    ///
    /// The default is off so a recorder can capture the popup.
    pub exclude_from_capture: bool,
    /// The allowed popup width as a percent of the monitor.
    #[serde(default = "default_max_width_percent")]
    pub max_width_percent: u8,
    /// The allowed popup height as a percent of the monitor.
    pub max_height_percent: u8,
    /// The maximum summary length in characters.
    pub summary_chars: usize,
    pub font: String,
    /// Draws a box around the word that the popup defines.
    #[serde(default = "default_highlight_match")]
    pub highlight_match: bool,
    /// Lets the user scroll a long popup with the wheel.
    #[serde(default = "default_scroll_popup")]
    pub scroll_popup: bool,
    /// This setting enables auto-scroll when a selection drag reaches the popup edge.
    ///
    /// If `scroll_popup = false`, edge auto-scroll stays disabled.
    #[serde(default = "default_edge_autoscroll")]
    pub edge_autoscroll: bool,
    /// Places collapsed rows beside the entry instead of below it.
    #[serde(default)]
    pub side_panel: bool,
    /// The layer that holds the popup on Linux.
    #[serde(default)]
    pub layer: PopupLayer,
    /// Selects a compact or roomy layout.
    #[serde(default)]
    pub layout_mode: LayoutMode,
    /// Applies each Dictionary's style.
    ///
    /// When this setting is off, the theme supplies the font and colors for every
    /// entry. The setting also ignores the inline `style` object and the
    /// Dictionary's `styles.css` file.
    #[serde(default = "default_dictionary_styling")]
    pub dictionary_styling: bool,
    /// Shows example sentences.
    #[serde(default = "default_show_examples")]
    pub show_examples: bool,
    /// Shows attributions and footnotes.
    ///
    /// This setting is independent of `show_examples`.
    /// A user can keep the sources without three sentences for each sense.
    #[serde(default = "default_show_attributions")]
    pub show_attributions: bool,
    /// Shows Dictionary images.
    ///
    /// When this setting is off, the code keeps an image's `alt` text because a
    /// gaiji represents a character.
    /// If the code drops the gaiji, a hole appears in the word.
    #[serde(default = "default_show_images")]
    pub show_images: bool,
    /// Shows part-of-speech labels inline.
    ///
    /// The default is off because the card's `pos` field already shows these labels
    /// above the glosses.
    /// Inline labels repeat them.
    /// `gloss::RoleFilter::CARD` drops them for the same reason.
    #[serde(default)]
    pub show_part_of_speech: bool,
}

/// Uses 25 percent of the monitor by default.
fn default_max_width_percent() -> u8 {
    25
}

/// Enables match highlights by default.
fn default_highlight_match() -> bool {
    true
}

/// Enables popup scroll by default.
fn default_scroll_popup() -> bool {
    true
}

/// This function enables edge auto-scroll by default.
fn default_edge_autoscroll() -> bool {
    true
}

/// The default is on.
/// A Dictionary that styles its entry expects this setting.
fn default_dictionary_styling() -> bool {
    true
}

/// The default is on.
/// A sentence that shows the word in use helps a learner understand
/// the word.
fn default_show_examples() -> bool {
    true
}

/// The default is on.
/// A license line makes an entry quotable.
fn default_show_attributions() -> bool {
    true
}

/// The default is on.
/// An image node represents a *character* more often than an illustration.
/// The census found 427 786 nodes with a gaiji marker in
/// (`docs/research/dict-shapes.md`).
/// This count supports the default.
fn default_show_images() -> bool {
    true
}

/// Selects the wlr layer that holds the Linux popup.
///
/// `overlay` clears every surface and fullscreen client.
/// `top` stays below them, and some compositors handle it better.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PopupLayer {
    /// Places the popup above everything. This value is the default.
    #[default]
    Overlay,
    /// Places the popup below fullscreen clients.
    Top,
}

/// Selects the room that the entry structure receives.
///
/// Yomitan exposes a small fixed set of root attributes that drive a CSS decision table.
/// This setting mirrors the attribute that changes the most: a glossary list
/// stacks or reads as one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayoutMode {
    /// Places one marked and indented item on each line, as a browser draws a list.
    /// This mode is the default because the popup must render the structure
    /// of the parsed entry.
    #[default]
    Roomy,
    /// Places one paragraph on one line with a separator between items.
    /// This mode keeps the compact layout as a user choice, not the only option.
    /// Yomitan and Hoshi Reader implement compact mode with
    /// `li { display: inline }` and a separator after the first item.
    /// The separator follows the first item only.
    Compact,
}

/// This enum selects the physical button that applies a glossary selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectionButtons {
    /// This variant adds to the current selection when the user uses the primary button.
    #[default]
    PrimaryAdditive,
    /// This variant replaces the current selection when the user uses the primary button.
    PrimaryReplacing,
}

impl SelectionButtons {
    /// This method returns the kebab-case value stored in TOML.
    pub const fn as_str(self) -> &'static str {
        match self {
            SelectionButtons::PrimaryAdditive => "primary-additive",
            SelectionButtons::PrimaryReplacing => "primary-replacing",
        }
    }

    /// This method returns the label that the settings windows show.
    pub const fn label(self) -> &'static str {
        match self {
            SelectionButtons::PrimaryAdditive => "Primary additive",
            SelectionButtons::PrimaryReplacing => "Primary replacing",
        }
    }
}

/// This enum selects the separator between selected glossary fragments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectionSeparator {
    /// This variant joins fragments with an ellipsis.
    #[default]
    Ellipsis,
    /// This variant joins fragments with one space.
    Space,
    /// This variant joins fragments with a line break.
    LineBreak,
    /// This variant joins fragments as separate list items.
    ListItems,
}

impl SelectionSeparator {
    /// This method returns the kebab-case value stored in TOML.
    pub const fn as_str(self) -> &'static str {
        match self {
            SelectionSeparator::Ellipsis => "ellipsis",
            SelectionSeparator::Space => "space",
            SelectionSeparator::LineBreak => "line-break",
            SelectionSeparator::ListItems => "list-items",
        }
    }

    /// This method returns the label that the settings windows show.
    pub const fn label(self) -> &'static str {
        match self {
            SelectionSeparator::Ellipsis => "Ellipsis (…)",
            SelectionSeparator::Space => "Space",
            SelectionSeparator::LineBreak => "Line break",
            SelectionSeparator::ListItems => "List items",
        }
    }
}

impl From<SelectionSeparator> for crate::dict::gloss::Separator {
    fn from(separator: SelectionSeparator) -> Self {
        match separator {
            SelectionSeparator::Ellipsis => crate::dict::gloss::Separator::Ellipsis,
            SelectionSeparator::Space => crate::dict::gloss::Separator::Space,
            SelectionSeparator::LineBreak => crate::dict::gloss::Separator::LineBreak,
            SelectionSeparator::ListItems => crate::dict::gloss::Separator::ListItems,
        }
    }
}

/// This enum selects what a triple-click on glossary text selects.
///
/// `Sense` selects one meaning without its examples. `SenseWithExamples` selects
/// one meaning with the examples that belong to it. `Line` follows browser
/// paragraph boundaries and ignores Sense markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TripleClick {
    /// This variant selects a Sense without its examples.
    Sense,
    /// This variant selects a Sense with the examples that belong to it.
    #[default]
    SenseWithExamples,
    /// This variant selects the block or the text line under the pointer.
    Line,
}

impl TripleClick {
    /// This method returns the kebab-case value stored in TOML.
    pub const fn as_str(self) -> &'static str {
        match self {
            TripleClick::Sense => "sense",
            TripleClick::SenseWithExamples => "sense-with-examples",
            TripleClick::Line => "line",
        }
    }

    /// This method returns the label that the settings windows show.
    pub const fn label(self) -> &'static str {
        match self {
            TripleClick::Sense => "Sense",
            TripleClick::SenseWithExamples => "Sense with examples",
            TripleClick::Line => "Line",
        }
    }
}

impl PopupConfig {
    /// Returns the popup render settings that the scene builder uses.
    ///
    /// The method has the same shape as [`Config::present_config`] for the same reason.
    /// One resolved record gives each bin one complete state.
    /// The method gives `ui::layout::build_elements` one place to read the six settings.
    ///
    /// This filter differs from the Anki card filter.
    /// The card renderer uses `RoleFilter::CARD`, and no setting reaches it.
    /// Hiding examples on screen leaves them on a mined card.
    /// The card keeps examples that the popup hides.
    pub fn render_settings(&self) -> crate::ui::layout::RenderSettings {
        crate::ui::layout::RenderSettings {
            stack_items: self.layout_mode == LayoutMode::Roomy,
            styling: self.dictionary_styling,
            images: self.show_images,
            roles: crate::dict::gloss::RoleFilter {
                examples: self.show_examples,
                attributions: self.show_attributions,
                part_of_speech: self.show_part_of_speech,
            },
        }
    }
}

/// The platform for which a caller reads a field.
///
/// Every field stays on the shared `Config`.
/// This enum selects the field from a per-platform pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Linux,
}

impl Platform {
    /// Returns the platform that runs this build.
    pub const fn current() -> Platform {
        if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }

    /// Returns the font for a new config.
    pub const fn default_font(self) -> &'static str {
        match self {
            Platform::Windows => "Yu Gothic UI",
            Platform::Linux => "Noto Sans CJK JP",
        }
    }
}

/// The font family that the popup must use.
///
/// `popup.font` is a literal. A config from the other platform can name a
/// family that this platform lacks.
/// The caller renders the warning, and this type selects the font family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontChoice {
    /// The resolver found the configured family.
    Configured(String),
    /// The resolver did not find the configured family. The platform default replaces it.
    Fallback {
        /// The family that the config requested.
        requested: String,
        /// The family that the popup uses instead.
        family: &'static str,
    },
}

impl FontChoice {
    /// Returns the family for both choices.
    pub fn family(&self) -> &str {
        match self {
            FontChoice::Configured(f) => f,
            FontChoice::Fallback { family, .. } => family,
        }
    }
}

/// Selects the family to render.
///
/// `resolvable` reports whether the font stack contains the family.
/// The code does not query an empty literal.
/// An empty literal always selects the platform default.
pub fn resolve_font(
    configured: &str,
    platform: Platform,
    resolvable: impl FnOnce(&str) -> bool,
) -> FontChoice {
    if !configured.is_empty() && resolvable(configured) {
        return FontChoice::Configured(configured.to_string());
    }
    FontChoice::Fallback {
        requested: configured.to_string(),
        family: platform.default_font(),
    }
}

/// The `[dictionaries]` section.
///
/// Each Dictionary role has one enabled array and one disabled array. Six
/// flat arrays hold names without records. Array position sets priority for
/// that role.
/// The `_disabled` array records checkbox state.
/// Both platform TOML writers save the pair without another struct
/// (ARCHITECTURE.md#dictionary-and-lookup).
///
/// Every name is the **exact** name of a Dictionary, and equality matches it.
/// The config keeps names that no installed Dictionary answers to.
/// A disconnected library drive therefore does not delete the list.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DictionariesConfig {
    /// Term Dictionaries in highest-priority order.
    #[serde(default)]
    pub terms: Vec<String>,
    #[serde(default)]
    pub terms_disabled: Vec<String>,
    /// Frequency Dictionaries in highest-priority order. This is the order that
    /// [`crate::dict::frequency::RankingStrategy::Priority`] reads.
    #[serde(default)]
    pub frequency: Vec<String>,
    #[serde(default)]
    pub frequency_disabled: Vec<String>,
    /// Pitch Dictionaries in highest-priority order.
    #[serde(default)]
    pub pitch: Vec<String>,
    #[serde(default)]
    pub pitch_disabled: Vec<String>,
    /// The rule that reduces Reported frequencies from enabled
    /// Dictionaries to one Frequency rank in `term.freq`.
    ///
    /// The spelling matches `meta.frequency_strategy` exactly, so the file
    /// and database stay consistent.
    #[serde(default)]
    pub ranking_strategy: crate::dict::frequency::RankingStrategy,
    /// The terms that one OCR language searches, in priority order.
    ///
    /// This map holds terms only. The OCR language is the key.
    /// Its value lists the definitions that this language must search.
    /// Frequency and pitch use one list for every language.
    #[serde(default)]
    pub per_language: BTreeMap<String, Vec<String>>,
    /// The legacy substring list. The code reads it but never writes it.
    ///
    /// `display_order` stores **name substrings** matched with `contains`.
    /// It is the only field here that names a Dictionary inexactly.
    /// [`DictionariesConfig::listed`] resolves each substring against installed
    /// names, and every consumer uses that method.
    /// `skip_serializing` means that the first save after an upgrade writes the six
    /// arrays and removes this key.
    /// The field is crate-private, so no other binary can add a reader.
    #[serde(default, skip_serializing)]
    pub(crate) display_order: Vec<String>,
}

impl DictionariesConfig {
    /// Returns the pair for one role: the enabled array, then its disabled twin.
    pub fn lists(&self, role: crate::library::Role) -> (&[String], &[String]) {
        match role {
            crate::library::Role::Terms => (&self.terms, &self.terms_disabled),
            crate::library::Role::Frequency => (&self.frequency, &self.frequency_disabled),
            crate::library::Role::Pitch => (&self.pitch, &self.pitch_disabled),
        }
    }

    /// Writes both arrays for one role.
    pub fn set_lists(&mut self, role: crate::library::Role, on: Vec<String>, off: Vec<String>) {
        match role {
            crate::library::Role::Terms => (self.terms, self.terms_disabled) = (on, off),
            crate::library::Role::Frequency => {
                (self.frequency, self.frequency_disabled) = (on, off);
            }
            crate::library::Role::Pitch => (self.pitch, self.pitch_disabled) = (on, off),
        }
    }

    /// Returns every Dictionary that this role list names, in priority order,
    /// with its checkbox state.
    ///
    /// This method resolves the legacy `display_order` list.
    /// A config with this field predates roles.
    /// The code resolves each substring against `installed` and enables every
    /// name that it finds.
    /// It drops substrings with no match.
    /// A substring that matches two installed names contributes both names in
    /// library order. Other configs use exact names, which the code reads unchanged.
    pub fn listed(
        &self,
        role: crate::library::Role,
        installed: &[crate::present::DictInfo],
    ) -> Vec<(String, bool)> {
        if self.is_pre_roles() {
            return resolve_substrings(&self.display_order, installed)
                .into_iter()
                .map(|name| (name, true))
                .collect();
        }
        let (on, off) = self.lists(role);
        on.iter()
            .map(|name| (name.clone(), true))
            .chain(off.iter().map(|name| (name.clone(), false)))
            .collect()
    }

    /// Returns enabled Dictionaries for this role in highest-priority order.
    ///
    /// An installed Dictionary absent from both arrays is new.
    /// It goes to the bottom in the enabled state.
    /// The code does not reorder a curated list.
    /// It appends each new Dictionary at the end.
    /// The disabled array excludes its names.
    /// An empty result means the user chose to search nothing in this role.
    /// The code keeps that choice.
    /// It does not replace the choice with a default.
    pub fn enabled(
        &self,
        role: crate::library::Role,
        installed: &[crate::present::DictInfo],
    ) -> Vec<String> {
        let listed = self.listed(role, installed);
        let mut out: Vec<String> =
            listed.iter().filter(|(_, on)| *on).map(|(name, _)| name.clone()).collect();
        for dict in installed {
            if !listed.iter().any(|(name, _)| *name == dict.name) {
                out.push(dict.name.clone());
            }
        }
        out
    }

    /// Returns the terms that an OCR language searches.
    /// Returns `None` when the language has no own list, so the global terms list decides.
    pub fn language_scope(
        &self,
        language: &str,
        installed: &[crate::present::DictInfo],
    ) -> Option<Vec<String>> {
        let list = self.per_language.get(language).filter(|list| !list.is_empty())?;
        Some(if self.is_pre_roles() {
            resolve_substrings(list, installed)
        } else {
            list.clone()
        })
    }

    /// Confirms whether this config predates Dictionary roles.
    ///
    /// The legacy key provides the whole test.
    /// A migrated config never writes the key again.
    /// This method identifies one upgrade state.
    fn is_pre_roles(&self) -> bool {
        !self.display_order.is_empty()
    }
}

/// Resolves a legacy substring list to exact installed names.
///
/// Each substring contributes every name that matches, in library order and list
/// order.
/// The code adds each name once.
/// It drops substrings with no match because no exact installed name exists
/// for them.
fn resolve_substrings(
    list: &[String],
    installed: &[crate::present::DictInfo],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in list.iter().filter(|entry| !entry.trim().is_empty()) {
        let needle = entry.to_lowercase();
        for dict in installed {
            if dict.name.to_lowercase().contains(&needle) && !out.contains(&dict.name) {
                out.push(dict.name.clone());
            }
        }
    }
    out
}

/// The `[plugins]` section. It is optional.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PluginsConfig {
    /// The names of plugins that can run.
    #[serde(default)]
    pub enabled: Vec<String>,
}

/// The `[ocr]` section. It is optional.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OcrConfig {
    /// The number of captures for each hover.
    ///
    /// The default is 1, so multiple captures stay disabled.
    #[serde(default = "default_max_ocr_passes")]
    pub max_ocr_passes: u8,
    /// Prefers a tall capture for manga and visual novels.
    #[serde(default)]
    pub prefer_vertical: bool,
    /// The capture box width in pixels.
    #[serde(default = "default_capture_width")]
    pub capture_width: i32,
    /// The capture box height in pixels.
    #[serde(default = "default_capture_height")]
    pub capture_height: i32,
    /// Resolves words that contain only Latin characters.
    #[serde(default = "default_scan_alphanumeric")]
    pub scan_alphanumeric: bool,
    /// The language tag for the OCR recognizer.
    #[serde(default = "default_ocr_language")]
    pub language: String,
    /// The OCR engine name. Use `"builtin"` or a plugin name.
    #[serde(default = "default_ocr_engine")]
    pub engine: String,
}

/// The default is 1, so multiple captures stay disabled.
fn default_max_ocr_passes() -> u8 {
    1
}

fn default_capture_width() -> i32 {
    500
}

fn default_capture_height() -> i32 {
    100
}

fn default_scan_alphanumeric() -> bool {
    true
}

fn default_ocr_engine() -> String {
    "builtin".to_string()
}

impl Default for OcrConfig {
    fn default() -> OcrConfig {
        OcrConfig {
            max_ocr_passes: default_max_ocr_passes(),
            prefer_vertical: false,
            capture_width: default_capture_width(),
            capture_height: default_capture_height(),
            scan_alphanumeric: default_scan_alphanumeric(),
            language: default_ocr_language(),
            engine: default_ocr_engine(),
        }
    }
}

/// Represents the OCR engine after config resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineChoice {
    /// The Windows built-in engine.
    Builtin,
    /// Names an enabled plugin.
    Plugin(String),
    /// The config named a plugin that is neither enabled nor found.
    /// The resolver uses this choice when no enabled plugin matches.
    FellBack(String),
}

/// Selects the OCR engine from the config.
pub fn resolve_engine(engine: &str, enabled: &[String]) -> EngineChoice {
    if engine == "builtin" {
        return EngineChoice::Builtin;
    }
    if enabled.iter().any(|e| e == engine) {
        EngineChoice::Plugin(engine.to_string())
    } else {
        EngineChoice::FellBack(engine.to_string())
    }
}

/// The `[debug]` section. It is optional.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DebugConfig {
    /// Draws an outline around the region that a hover captured.
    ///
    /// When this setting is off, the outline stays inactive instead of merely hidden.
    #[serde(default)]
    pub show_scan_region: bool,
    /// Shows a console record for each hover.
    #[serde(default)]
    pub show_lookup_log: bool,
    /// Shows the active engine name.
    #[serde(default)]
    pub show_engine_log: bool,
    /// Shows the adapter log.
    #[serde(default)]
    pub show_adapter_log: bool,
}

/// Maps one source to one Anki field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldMapping {
    pub anki_field: String,
    pub source: String,
}

/// Lists each `source` that a field-map row can name, in picker order.
///
/// A `source` names a key that [`crate::anki::mapped_fields`] reads from
/// the note's `fields` map.
/// A row with a missing source value adds nothing to the note.
/// `expression`, `reading`, `glossary`, and `glossary_html` always come
/// from `anki::fields_from_card`.
/// That function adds `frequency` only when the card has one.
/// It adds `pitch_html` only when an enabled pitch Dictionary has the
/// card's reading.
/// `controller::note_payload` adds `sentence` when the hover produces one.
/// `screenshot` is not a `fields` key.
/// `shot::plan` reads it directly from this list and selects the Anki field
/// for the picture.
///
/// The Windows combo box puts `"(none)"` first. This string means that no
/// field is mapped. `row_mapping` removes it before a save.
/// The string is never stored, so it is not a source.
pub const FIELD_SOURCES: [&str; 8] = [
    "expression",
    "reading",
    "glossary",
    "frequency",
    "glossary_html",
    "pitch_html",
    "screenshot",
    "sentence",
];

/// The `[anki]` section. It is optional.
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
    /// The shortcut that adds the top card.
    #[serde(default = "default_anki_add_key")]
    pub add_key: String,
    /// The same shortcut on Linux, in portal syntax.
    #[serde(default = "default_anki_add_key_linux")]
    pub add_key_linux: String,
    /// Shows a tray balloon after an add.
    #[serde(default = "default_notify_on_add")]
    pub notify_on_add: bool,
    /// The Anki field for each source value.
    #[serde(default = "default_field_map")]
    pub field_map: Vec<FieldMapping>,
    /// Defines how the code builds the Anki sentence field.
    #[serde(default = "default_sentence_mode")]
    pub sentence_mode: SentenceMode,
    /// Sets the static region for sentence capture.
    #[serde(default = "default_static_region_key")]
    pub static_region_key: String,
    /// The same static-region key on Linux, in portal syntax.
    #[serde(default = "default_static_region_key_linux")]
    pub static_region_key_linux: String,
    /// The static region as [x, y, w, h], when the user sets one.
    #[serde(default)]
    pub static_region: Option<[i32; 4]>,
    /// Makes the teal border visible.
    #[serde(default = "default_show_static_overlay")]
    pub show_static_overlay: bool,
    /// This setting includes each Dictionary name above its Anki glossary group.
    #[serde(default = "default_include_dictionary_name")]
    pub include_dictionary_name: bool,
    /// Uses only the entry from the top Dictionary.
    #[serde(default)]
    pub first_dict_only: bool,
    /// This setting selects whether the primary button adds to or replaces a selection.
    #[serde(default)]
    pub selection_buttons: SelectionButtons,
    /// This setting selects the separator between selected glossary fragments.
    #[serde(default)]
    pub selection_separator: SelectionSeparator,
    /// This setting selects the content for a triple-click.
    #[serde(default)]
    pub triple_click: TripleClick,
}


/// The default Anki URL.
fn default_anki_url() -> String {
    "http://localhost:8765".to_string()
}

/// The default Anki deck name.
fn default_anki_deck() -> String {
    "Default".to_string()
}
/// The default Anki model name.
fn default_anki_model() -> String {
    "Lapis".to_string()
}
/// The default Anki add key.
fn default_anki_add_key() -> String {
    "a".to_string()
}

/// Uses the same modifier family as the trigger.
/// A portal binding is system-wide, so a bare letter is not enough.
fn default_anki_add_key_linux() -> String {
    "ALT+A".to_string()
}

/// The default is on.
fn default_notify_on_add() -> bool {
    true
}

/// Defines the text that the Anki sentence field receives.
///
/// The TOML file uses `lowercase` names.
/// Files from earlier versions use `"line"`, `"all"`, and `"static"`.
/// The parser still reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SentenceMode {
    /// The OCR line that contains the cursor.
    Line,
    /// Every line that the hover capture reads.
    All,
    /// Every line inside the region that the user draws. In this mode, a
    /// lookup also reads from that region.
    Static,
}

/// The default sentence mode.
fn default_sentence_mode() -> SentenceMode {
    SentenceMode::Line
}

/// The default key for the static region.
fn default_static_region_key() -> String {
    String::new()
}

/// Leaves the key unbound, like its Windows twin.
/// The portal shortcut list holds only the trigger and Anki add.
/// No action binds this chord.
fn default_static_region_key_linux() -> String {
    String::new()
}

/// The overlay is on by default.
fn default_show_static_overlay() -> bool {
    true
}

/// This function keeps Dictionary headings because earlier versions always added them.
fn default_include_dictionary_name() -> bool {
    true
}

/// Returns the Lapis field mapping.
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
            add_key_linux: default_anki_add_key_linux(),
            notify_on_add: default_notify_on_add(),
            field_map: default_field_map(),
            sentence_mode: default_sentence_mode(),
            static_region_key: default_static_region_key(),
            static_region_key_linux: default_static_region_key_linux(),
            static_region: None,
            show_static_overlay: default_show_static_overlay(),
            include_dictionary_name: default_include_dictionary_name(),
            first_dict_only: false,
            selection_buttons: SelectionButtons::default(),
            selection_separator: SelectionSeparator::default(),
            triple_click: TripleClick::default(),
        }
    }
}

/// The Ctrl modifier bit.
pub const MOD_CTRL: u8 = 0b001;
/// The Shift modifier bit.
pub const MOD_SHIFT: u8 = 0b010;
/// The Alt modifier bit.
pub const MOD_ALT: u8 = 0b100;

/// The `[actions]` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionsConfig {
    #[serde(default = "default_actions_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub screenshot: ScreenshotConfig,
    #[serde(default)]
    pub ocr_clipboard: Option<OcrClipboardConfig>,
}

/// The `[actions.screenshot]` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenshotConfig {
    #[serde(default = "default_screenshot_hotkey")]
    pub hotkey: String,
    /// The same action on Linux. No value leaves the action unbound.
    ///
    /// This value is not portal syntax, unlike `anki.add_key_linux`.
    /// The portal shortcut ID set stays fixed at two, so this action uses the control socket.
    /// The Linux settings window gives this chord as a copyable compositor binding snippet.
    /// The field uses `Option`, like the OCR-clipboard twin.
    /// Absence stays distinct from an empty string.
    #[serde(default)]
    pub hotkey_linux: Option<String>,
    #[serde(default = "default_screenshot_save_dir")]
    pub save_dir: String,
    #[serde(default)]
    pub include_on_add: bool,
}

/// The `[actions.ocr_clipboard]` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OcrClipboardConfig {
    /// An empty value or no value disables the action.
    #[serde(default)]
    pub hotkey: Option<String>,
    /// The same action on Linux, in portal syntax. No value disables the action.
    #[serde(default)]
    pub hotkey_linux: Option<String>,
}

/// The default is on.
fn default_actions_enabled() -> bool {
    true
}

/// The default screenshot hotkey.
fn default_screenshot_hotkey() -> String {
    "ctrl+shift+s".to_string()
}

/// The default screenshot folder.
fn default_screenshot_save_dir() -> String {
    "screenshots".to_string()
}


impl Default for ActionsConfig {
    fn default() -> ActionsConfig {
        ActionsConfig {
            enabled: default_actions_enabled(),
            screenshot: ScreenshotConfig::default(),
            ocr_clipboard: None,
        }
    }
}

impl Default for ScreenshotConfig {
    fn default() -> ScreenshotConfig {
        ScreenshotConfig {
            hotkey: default_screenshot_hotkey(),
            hotkey_linux: None,
            save_dir: default_screenshot_save_dir(),
            include_on_add: false,
        }
    }
}

/// Returns the VK code and modifier bits from a hotkey string.
pub fn parse_hotkey(s: &str) -> Option<(u16, u8)> {
    let parts: Vec<&str> = s.split('+').collect();
    let (key, mod_parts) = parts.split_last()?;
    let mut mods = 0u8;
    for part in mod_parts {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= MOD_CTRL,
            "shift" => mods |= MOD_SHIFT,
            "alt" => mods |= MOD_ALT,
            _ => return None,
        }
    }
    let vk = parse_trigger_key(key)?;
    Some((vk, mods))
}

impl Default for Config {
    /// The values that chibipop uses by default.
    fn default() -> Config {
        Config {
            trigger: TriggerConfig {
                mode: TriggerMode::Live,
                trigger_key: default_trigger_key(),
                trigger_key_linux: default_trigger_key_linux(),
                per_character_lookup: false,
            },
            popup: PopupConfig {
                theme: "dark".to_string(),
                exclude_from_capture: false,
                max_width_percent: default_max_width_percent(),
                max_height_percent: 45,
                summary_chars: 40,
                font: Platform::current().default_font().to_string(),
                highlight_match: default_highlight_match(),
                scroll_popup: default_scroll_popup(),
                edge_autoscroll: default_edge_autoscroll(),
                side_panel: false,
                layer: PopupLayer::default(),
                layout_mode: LayoutMode::default(),
                dictionary_styling: default_dictionary_styling(),
                show_examples: default_show_examples(),
                show_attributions: default_show_attributions(),
                show_images: default_show_images(),
                show_part_of_speech: false,
            },
            // The default has no Dictionary names.
            // A new installation enables every installed Dictionary in library order.
            // Earlier defaults stored two substrings.
            // Those substrings guessed which Dictionary a user installed.
            dictionaries: DictionariesConfig::default(),
            plugins: PluginsConfig::default(),
            ocr: OcrConfig::default(),
            debug: DebugConfig::default(),
            anki: AnkiConfig::default(),
            actions: ActionsConfig::default(),
        }
    }
}

impl Config {
    /// Saves the config as a TOML file.
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut text = toml::to_string_pretty(self)
            .with_context(|| format!("serialising config for {}", path.display()))?;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        // Write to a temporary file first. A torn write otherwise loses the whole config.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)
            .with_context(|| format!("writing config to {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    /// Clamps every bounded config value.
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
        self.ocr.capture_width = clamped(
            path,
            "ocr.capture_width",
            self.ocr.capture_width,
            CAPTURE_W_RANGE.0,
            CAPTURE_W_RANGE.1,
        );
        self.ocr.capture_height = clamped(
            path,
            "ocr.capture_height",
            self.ocr.capture_height,
            CAPTURE_H_RANGE.0,
            CAPTURE_H_RANGE.1,
        );
    }

    /// Builds the [`crate::present::PresentConfig`] for this OCR language.
    /// It resolves the term scope before it returns.
    ///
    /// It passes exact Dictionary names in priority order, without fallback guards.
    /// A name that matches no installed Dictionary remains in the result.
    /// An empty result remains valid when the user disables every Dictionary.
    /// Older guards treated missing entries, empty entries, unmatched lists, and
    /// recognizer differences as errors.
    /// Exact names now express identity, so the presentation path does not add those guards.
    /// See (ARCHITECTURE.md#dictionary-and-lookup).
    ///
    /// `dictionaries.per_language[ocr.language]` can narrow the terms list.
    /// It never narrows the pitch list because pitch has no per-language scope.
    pub fn present_config(
        &self,
        dicts: &[crate::present::DictInfo],
    ) -> crate::present::PresentConfig {
        crate::present::PresentConfig {
            terms: self
                .dictionaries
                .language_scope(&self.ocr.language, dicts)
                .unwrap_or_else(|| self.dictionaries.enabled(crate::library::Role::Terms, dicts)),
            pitch: self.dictionaries.enabled(crate::library::Role::Pitch, dicts),
            summary_chars: self.popup.summary_chars,
        }
    }

    /// Returns the layer that holds the Linux popup.
    ///
    /// Linux reads this field. Windows ignores it.
    pub fn popup_layer(&self) -> PopupLayer {
        self.popup.layer
    }

    /// Returns the trigger chord for a platform.
    pub fn trigger_key_for(&self, platform: Platform) -> &str {
        match platform {
            Platform::Windows => &self.trigger.trigger_key,
            Platform::Linux => &self.trigger.trigger_key_linux,
        }
    }

    /// Returns the Anki-add chord for a platform.
    pub fn add_key_for(&self, platform: Platform) -> &str {
        match platform {
            Platform::Windows => &self.anki.add_key,
            Platform::Linux => &self.anki.add_key_linux,
        }
    }
}

/// Clamps a value and reports each change.
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

/// Loads the config and creates it when the file does not exist.
///
/// Returns an error for malformed TOML.
/// Clamps an out-of-range value.
pub fn load_or_create(path: &Path) -> Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let mut config: Config = toml::from_str(&text)
                .with_context(|| format!("parsing config from {}", path.display()))?;
            // Convert the legacy hold-shift mode.
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

    /// A path that is unique for each process and each test.
    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("chibipop_cfg_{}_{}.toml", std::process::id(), name))
    }

    /// `controller::note_payload` can supply a sentence. A picker must route it.
    #[test]
    fn field_sources_offers_sentence() {
        assert!(FIELD_SOURCES.contains(&"sentence"));
    }

    /// A mining screenshot needs this row to find its field.
    /// `shot::plan` finds the picture field from this source.
    #[test]
    fn field_sources_offers_screenshot() {
        assert!(FIELD_SOURCES.contains(&"screenshot"));
    }

    /// A picker must offer every shipped default.
    /// Without that option, the settings window cannot reproduce the default.
    #[test]
    fn every_default_field_map_source_is_offered() {
        for mapping in default_field_map() {
            assert!(
                FIELD_SOURCES.contains(&mapping.source.as_str()),
                "default row {} maps source {:?}, which no picker offers",
                mapping.anki_field,
                mapping.source
            );
        }
    }

    #[test]
    fn defaults_match_the_spec() {
        let c = Config::default();
        assert_eq!(TriggerMode::Live, c.trigger.mode);
        assert_eq!("dark", c.popup.theme);
        assert_eq!(25, c.popup.max_width_percent);
        assert_eq!(45, c.popup.max_height_percent);
        assert_eq!(40, c.popup.summary_chars);
        assert_eq!(Platform::current().default_font(), c.popup.font);
        assert_eq!("ALT+F", c.trigger.trigger_key_linux);
        assert_eq!("ALT+A", c.anki.add_key_linux);
        assert_eq!("", c.anki.static_region_key_linux, "unbound, like its Windows twin");
        assert_eq!(
            None, c.actions.screenshot.hotkey_linux,
            "unbound: the control-socket verb has no compositor bind until a human writes one"
        );
        assert_eq!(PopupLayer::Overlay, c.popup.layer);
        assert_eq!(
            DictionariesConfig::default(),
            c.dictionaries,
            "no list ships: a Dictionary no array names is new and enabled",
        );
    }

    /// Confirms that the user must enable capture exclusion.
    #[test]
    fn capture_exclusion_defaults_to_false() {
        assert!(
            !Config::default().popup.exclude_from_capture,
            "the popup must be recordable out of the box - exclusion is the opt-in"
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

    /// One TOML value can enable multiple OCR passes.
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

    /// Confirms that a missing section loads with defaults.
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

    /// Confirms that an empty section uses the field default.
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

    /// Confirms that a missing section loads with defaults.
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

    /// Confirms that the field default differs from Serde's bare default.
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
    fn edge_autoscrolling_defaults_on() {
        assert!(Config::default().popup.edge_autoscroll);
    }

    #[test]
    fn disabled_scrolling_round_trips() {
        let p = tmp("scroll_off");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.popup.scroll_popup = false;
        c.popup.edge_autoscroll = false;
        c.save(&p).unwrap();
        let back = load_or_create(&p).unwrap();
        assert!(!back.popup.scroll_popup);
        assert!(!back.popup.edge_autoscroll);
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

    /// Confirms that the field default enables popup scroll.
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
        assert!(c.popup.edge_autoscroll, "a missing edge field must take the field default");
        let _ = std::fs::remove_file(&p);
    }

    /// Confirms that the loader clamps an out-of-range value.
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

    /// Confirms that values inside the range remain unchanged.
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

    /// Confirms that an earlier config loads.
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

    #[test]
    fn notify_on_add_defaults_to_true() {
        assert!(Config::default().anki.notify_on_add);
    }

    #[test]
    fn sentence_mode_defaults_to_line() {
        assert_eq!(SentenceMode::Line, Config::default().anki.sentence_mode);
    }

    #[test]
    fn static_region_key_defaults_to_empty() {
        assert_eq!("", Config::default().anki.static_region_key);
    }

    #[test]
    fn show_static_overlay_defaults_to_true() {
        assert!(Config::default().anki.show_static_overlay);
    }

    #[test]
    fn include_dictionary_name_defaults_to_true() {
        assert!(Config::default().anki.include_dictionary_name);
    }

    #[test]
    fn first_dict_only_defaults_to_false() {
        assert!(!Config::default().anki.first_dict_only);
    }

    #[test]
    fn selection_defaults_are_primary_additive_ellipsis_and_sense_with_examples() {
        let anki = &Config::default().anki;
        assert_eq!(SelectionButtons::PrimaryAdditive, anki.selection_buttons);
        assert_eq!(SelectionSeparator::Ellipsis, anki.selection_separator);
        assert_eq!(TripleClick::SenseWithExamples, anki.triple_click);
    }

    #[test]
    fn selection_enum_names_match_toml_values() {
        assert_eq!("primary-replacing", SelectionButtons::PrimaryReplacing.as_str());
        assert_eq!("line-break", SelectionSeparator::LineBreak.as_str());
        assert_eq!("sense-with-examples", TripleClick::SenseWithExamples.as_str());
        // `toml` refuses a bare scalar at the top level, so wrap the enums.
        #[derive(serde::Serialize)]
        struct Wrap {
            buttons: SelectionButtons,
            separator: SelectionSeparator,
            triple: TripleClick,
        }
        let text = toml::to_string(&Wrap {
            buttons: SelectionButtons::PrimaryReplacing,
            separator: SelectionSeparator::LineBreak,
            triple: TripleClick::Line,
        })
        .unwrap();
        assert!(text.contains("buttons = \"primary-replacing\""), "{text}");
        assert!(text.contains("separator = \"line-break\""), "{text}");
        assert!(text.contains("triple = \"line\""), "{text}");
    }

    #[test]
    fn selection_settings_round_trip() {
        let p = tmp("selection_settings_rt");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.anki.first_dict_only = true;
        c.anki.selection_buttons = SelectionButtons::PrimaryReplacing;
        c.anki.selection_separator = SelectionSeparator::ListItems;
        c.anki.triple_click = TripleClick::Sense;
        c.save(&p).unwrap();
        let back = load_or_create(&p).unwrap();
        assert!(back.anki.first_dict_only);
        assert_eq!(SelectionButtons::PrimaryReplacing, back.anki.selection_buttons);
        assert_eq!(SelectionSeparator::ListItems, back.anki.selection_separator);
        assert_eq!(TripleClick::Sense, back.anki.triple_click);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn an_anki_section_without_selection_settings_uses_defaults() {
        let p = tmp("anki_no_selection_settings");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\"]\n\n",
            "[anki]\nenabled = true\n",
        )).unwrap();
        let anki = &load_or_create(&p).expect("an old Anki config must load").anki;
        assert_eq!(SelectionButtons::PrimaryAdditive, anki.selection_buttons);
        assert_eq!(SelectionSeparator::Ellipsis, anki.selection_separator);
        assert_eq!(TripleClick::SenseWithExamples, anki.triple_click);
        let _ = std::fs::remove_file(&p);
    }

    /// Confirms that a missing Anki field uses its field default.
    #[test]
    fn an_anki_section_without_first_dict_only_still_defaults_off() {
        let p = tmp("anki_no_first_dict_only");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\"]\n\n",
            "[anki]\nenabled = true\n",
        )).unwrap();
        let c = load_or_create(&p).expect("a pre-first_dict_only config must load");
        assert!(!c.anki.first_dict_only, "a missing key takes the field default");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn an_anki_section_without_include_dictionary_name_keeps_dictionary_headings() {
        let p = tmp("anki_no_include_dictionary_name");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\"]\n\n",
            "[anki]\nenabled = true\n",
        )).unwrap();
        let c = load_or_create(&p).expect("a pre-toggle Anki config must load");
        assert!(c.anki.include_dictionary_name, "a missing key keeps the old output");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn static_region_defaults_to_none() {
        assert_eq!(None, Config::default().anki.static_region);
    }

    #[test]
    fn static_region_round_trips() {
        let p = tmp("static_region_rt");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.anki.static_region = Some([100, 800, 1200, 200]);
        c.save(&p).unwrap();
        let back = load_or_create(&p).unwrap();
        assert_eq!(Some([100, 800, 1200, 200]), back.anki.static_region);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn static_region_key_round_trips() {
        let p = tmp("static_key_rt");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.anki.static_region_key = "alt+r".to_string();
        c.save(&p).unwrap();
        let back = load_or_create(&p).unwrap();
        assert_eq!("alt+r", back.anki.static_region_key);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn static_region_key_empty_survives_round_trip() {
        let p = tmp("static_key_empty_rt");
        let _ = std::fs::remove_file(&p);
        let c = Config::default();
        c.save(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("static_region_key = \"\""));
        assert!(!text.contains("static_region_key = \"r\""));
        assert_eq!("", load_or_create(&p).unwrap().anki.static_region_key);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_config_with_static_region_key_r_still_loads() {
        let p = tmp("static_key_legacy");
        let _ = std::fs::remove_file(&p);
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\"]\n\n",
            "[anki]\nstatic_region_key = \"r\"\n",
        )).unwrap();
        assert_eq!("r", load_or_create(&p).unwrap().anki.static_region_key);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn sentence_mode_static_round_trips() {
        let p = tmp("sentence_static_rt");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.anki.sentence_mode = SentenceMode::Static;
        c.save(&p).unwrap();
        let back = load_or_create(&p).unwrap();
        assert_eq!(SentenceMode::Static, back.anki.sentence_mode);
        let _ = std::fs::remove_file(&p);
    }

    /// A 0.9.x file still uses these mode names.
    /// The enum is a Rust type, not a file-format change.
    #[test]
    fn a_config_written_before_the_enum_still_names_every_mode() {
        for (written, expected) in [
            ("line", SentenceMode::Line),
            ("all", SentenceMode::All),
            ("static", SentenceMode::Static),
        ] {
            let p = tmp(&format!("sentence_legacy_{written}"));
            let _ = std::fs::remove_file(&p);
            std::fs::write(&p, format!(concat!(
                "[trigger]\nmode = \"live\"\n\n",
                "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
                "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
                "[dictionaries]\ndisplay_order = [\"大辞林\"]\n\n",
                "[anki]\nsentence_mode = \"{}\"\n",
            ), written)).unwrap();
            let loaded = load_or_create(&p).unwrap();
            assert_eq!(expected, loaded.anki.sentence_mode, "{written} must still load");
            // Save the same file and confirm that the parser reads it again.
            loaded.save(&p).unwrap();
            assert!(std::fs::read_to_string(&p)
                .unwrap()
                .contains(&format!("sentence_mode = \"{written}\"")));
            let _ = std::fs::remove_file(&p);
        }
    }

    /// Confirms that the shipped default parses as the expected VK code.
    #[test]
    fn anki_add_key_default_parses_to_vk_a() {
        let vk = parse_trigger_key(&Config::default().anki.add_key);
        assert_eq!(Some(0x41), vk);
    }

    /// Confirms that a missing Anki key uses its field default.
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

    /// Both settings windows can save an empty map.
    /// Serde must apply the default only when the key is absent.
    /// A present `field_map = []` records the user's choice, not a missing value.
    /// See `an_anki_section_without_field_map_still_defaults_to_lapis`.
    #[test]
    fn an_emptied_anki_field_map_survives_a_save_and_reload() {
        let p = tmp("field_map_emptied");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.anki.field_map = Vec::new();
        c.save(&p).unwrap();
        let back = load_or_create(&p).unwrap();
        assert!(
            back.anki.field_map.is_empty(),
            "a user who mapped nothing must not get Lapis back on reload"
        );
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

    /// Confirms that a missing field-map key uses the Lapis default.
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

    // Tests for trigger keys.

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

    /// Confirms that the parser and display use the same key names.
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

    /// The value overflows `u16`. The parser rejects it and does not wrap it.
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

    #[test]
    fn capture_defaults_match_the_old_constants() {
        let c = Config::default();
        assert_eq!(500, c.ocr.capture_width);
        assert_eq!(100, c.ocr.capture_height);
        assert!(c.ocr.scan_alphanumeric);
    }

    #[test]
    fn a_missing_capture_section_uses_the_defaults() {
        let p = tmp("no_capture_fields");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\", \"Jitendex\"]\n\n",
            "[ocr]\nmax_ocr_passes = 1\n",
        )).unwrap();
        let c = load_or_create(&p).unwrap();
        assert_eq!(500, c.ocr.capture_width);
        assert_eq!(100, c.ocr.capture_height);
        assert!(c.ocr.scan_alphanumeric);
    }

    #[test]
    fn capture_values_below_the_floor_are_clamped_up() {
        let mut c = Config::default();
        c.ocr.capture_width = 1;
        c.ocr.capture_height = 1;
        c.clamp_ranges(Path::new("test.toml"));
        assert_eq!(CAPTURE_W_RANGE.0, c.ocr.capture_width);
        assert_eq!(CAPTURE_H_RANGE.0, c.ocr.capture_height);
    }

    #[test]
    fn capture_values_above_the_ceiling_are_clamped_down() {
        let mut c = Config::default();
        c.ocr.capture_width = 99_999;
        c.ocr.capture_height = 99_999;
        c.clamp_ranges(Path::new("test.toml"));
        assert_eq!(CAPTURE_W_RANGE.1, c.ocr.capture_width);
        assert_eq!(CAPTURE_H_RANGE.1, c.ocr.capture_height);
    }

    /// Confirms that both boundary values are valid.
    #[test]
    fn capture_values_exactly_on_the_bounds_are_untouched() {
        let mut c = Config::default();
        c.ocr.capture_width = CAPTURE_W_RANGE.0;
        c.ocr.capture_height = CAPTURE_H_RANGE.1;
        c.clamp_ranges(Path::new("test.toml"));
        assert_eq!(CAPTURE_W_RANGE.0, c.ocr.capture_width);
        assert_eq!(CAPTURE_H_RANGE.1, c.ocr.capture_height);
    }

    #[test]
    fn per_character_lookup_defaults_off() {
        assert!(!Config::default().trigger.per_character_lookup);
    }

    #[test]
    fn ocr_language_defaults_to_japanese() {
        assert_eq!("ja", Config::default().ocr.language);
    }

    #[test]
    fn a_config_without_the_new_keys_still_loads() {
        let p = tmp("no_v07_keys");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\", \"Jitendex\"]\n\n",
            "[ocr]\nmax_ocr_passes = 1\n",
        )).unwrap();
        let c = load_or_create(&p).unwrap();
        assert!(!c.trigger.per_character_lookup);
        assert_eq!("ja", c.ocr.language);
    }

    #[test]
    fn per_language_defaults_to_empty() {
        assert!(Config::default().dictionaries.per_language.is_empty());
    }

    #[test]
    fn a_config_without_per_language_still_loads() {
        let p = tmp("no_per_language");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\"]\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[ocr]\nmax_ocr_passes = 1\n",
        )).unwrap();
        let c = load_or_create(&p).unwrap();
        assert!(c.dictionaries.per_language.is_empty());
        assert_eq!(vec!["大辞林".to_string()], c.dictionaries.display_order);
    }

    #[test]
    fn per_language_round_trips_through_toml() {
        let p = tmp("per_language_round_trip");
        let mut c = Config::default();
        c.dictionaries.per_language.insert(
            "zh-Hans-CN".to_string(),
            vec!["中日大辞典".to_string()],
        );
        c.save(&p).unwrap();
        let back = load_or_create(&p).unwrap();
        assert_eq!(c.dictionaries.per_language, back.dictionaries.per_language);
    }

    /// Confirms that config creation uses one literal font per platform.
    #[test]
    fn each_platform_creates_its_own_default_font() {
        assert_eq!("Yu Gothic UI", Platform::Windows.default_font());
        assert_eq!("Noto Sans CJK JP", Platform::Linux.default_font());
    }

    /// Every field serializes on every platform.
    /// A config with fields from both platforms survives save and load unchanged.
    #[test]
    fn a_config_with_both_platforms_fields_round_trips_losslessly() {
        let p = tmp("both_platforms_round_trip");
        let mut c = Config::default();
        c.trigger.mode = TriggerMode::HoldKey;
        c.trigger.trigger_key = "f2".to_string();
        c.trigger.trigger_key_linux = "CTRL+ALT+F".to_string();
        c.trigger.per_character_lookup = true;
        c.popup.theme = "light".to_string();
        c.popup.exclude_from_capture = true;
        c.popup.max_width_percent = 30;
        c.popup.max_height_percent = 50;
        c.popup.summary_chars = 60;
        c.popup.font = "IPAexGothic".to_string();
        c.popup.highlight_match = false;
        c.popup.scroll_popup = false;
        c.popup.side_panel = true;
        c.popup.layer = PopupLayer::Top;
        c.popup.layout_mode = LayoutMode::Compact;
        c.popup.dictionary_styling = false;
        c.popup.show_examples = false;
        c.popup.show_attributions = false;
        c.popup.show_images = false;
        c.popup.show_part_of_speech = true;
        c.dictionaries.per_language.insert("ja".to_string(), vec!["大辞林".to_string()]);
        c.ocr.max_ocr_passes = 3;
        c.ocr.prefer_vertical = true;
        c.ocr.capture_width = 640;
        c.ocr.capture_height = 120;
        c.ocr.scan_alphanumeric = false;
        c.ocr.language = "en".to_string();
        c.debug.show_scan_region = true;
        c.anki.enabled = true;
        c.anki.url = "http://localhost:1234".to_string();
        c.anki.deck = "Mining".to_string();
        c.anki.model = "Kaishi".to_string();
        c.anki.add_key = "d".to_string();
        c.anki.add_key_linux = "CTRL+ALT+A".to_string();
        c.anki.static_region_key = "0x52".to_string();
        c.anki.static_region_key_linux = "ALT+R".to_string();
        c.actions.screenshot.hotkey_linux = Some("ALT+S".to_string());
        c.actions.ocr_clipboard = Some(OcrClipboardConfig {
            hotkey: Some("f9".to_string()),
            hotkey_linux: Some("ALT+C".to_string()),
        });
        c.save(&p).unwrap();
        assert_eq!(c, load_or_create(&p).unwrap());
        let _ = std::fs::remove_file(&p);
    }

    /// A Windows-shaped file from before the Linux fields keeps every stored value.
    /// The new fields take their documented defaults.
    #[test]
    fn a_windows_shaped_legacy_file_loads_unchanged_and_gains_defaults() {
        let p = tmp("windows_legacy");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"hold-key\"\ntrigger_key = \"f2\"\n\n",
            "[popup]\ntheme = \"light\"\nexclude_from_capture = true\n",
            "max_height_percent = 50\nsummary_chars = 60\nfont = \"Meiryo\"\n\n",
            "[dictionaries]\ndisplay_order = [\"Jitendex\"]\n\n",
            "[anki]\nadd_key = \"d\"\n",
        )).unwrap();
        let mut c = load_or_create(&p).unwrap();
        // Windows fields keep the values from the file.
        assert_eq!(TriggerMode::HoldKey, c.trigger.mode);
        assert_eq!("f2", c.trigger.trigger_key);
        assert_eq!("light", c.popup.theme);
        assert!(c.popup.exclude_from_capture);
        assert_eq!("Meiryo", c.popup.font);
        assert_eq!("d", c.anki.add_key);
        // New fields use documented defaults without migration.
        assert_eq!("ALT+F", c.trigger.trigger_key_linux);
        assert_eq!("ALT+A", c.anki.add_key_linux);
        assert_eq!("", c.anki.static_region_key_linux);
        assert_eq!(None, c.actions.screenshot.hotkey_linux);
        assert_eq!(PopupLayer::Overlay, c.popup.layer);
        // A whole-struct save writes the new keys with their defaults and preserves the old values.
        c.save(&p).unwrap();
        let saved = std::fs::read_to_string(&p).unwrap();
        assert!(saved.contains("trigger_key = \"f2\""));
        assert!(saved.contains("trigger_key_linux = \"ALT+F\""));
        assert!(saved.contains("add_key = \"d\""));
        assert!(saved.contains("add_key_linux = \"ALT+A\""));
        assert!(saved.contains("static_region_key_linux = \"\""));
        assert!(saved.contains("layer = \"overlay\""));
        assert!(saved.contains("font = \"Meiryo\""));
        assert!(!saved.contains("display_order"), "the save retires the pre-roles key");
        // The other values survive save and load.
        // The save intentionally removes the substring list and migrates the file once.
        c.dictionaries.display_order.clear();
        assert_eq!(c, load_or_create(&p).unwrap());
        let _ = std::fs::remove_file(&p);
    }

    /// Confirms that a save preserves fields for the other platform.
    #[test]
    fn a_save_preserves_the_other_platforms_fields() {
        let p = tmp("preserve_other_platform");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\ntrigger_key_linux = \"SUPER+J\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Noto Sans CJK JP\"\n",
            "layer = \"top\"\n\n",
            "[dictionaries]\ndisplay_order = [\"Jitendex\"]\n\n",
            "[ocr]\nlanguage = \"en\"\n\n",
            "[anki]\nadd_key_linux = \"SUPER+K\"\nstatic_region_key_linux = \"SUPER+R\"\n\n",
            "[actions.screenshot]\nhotkey_linux = \"SUPER+S\"\n\n",
            "[actions.ocr_clipboard]\nhotkey_linux = \"SUPER+C\"\n",
        )).unwrap();
        // Change a Windows-rendered field and save the struct.
        let mut c = load_or_create(&p).unwrap();
        c.popup.theme = "light".to_string();
        c.save(&p).unwrap();
        let back = load_or_create(&p).unwrap();
        assert_eq!("SUPER+J", back.trigger.trigger_key_linux);
        assert_eq!("SUPER+K", back.anki.add_key_linux);
        assert_eq!("SUPER+R", back.anki.static_region_key_linux);
        assert_eq!(
            Some("SUPER+C".to_string()),
            back.actions.ocr_clipboard.as_ref().and_then(|a| a.hotkey_linux.clone()),
            "the Linux OCR-clipboard chord survives a Windows-side save"
        );
        assert_eq!(
            Some("SUPER+S".to_string()),
            back.actions.screenshot.hotkey_linux,
            "the Linux screenshot chord survives a Windows-side save"
        );
        assert_eq!(PopupLayer::Top, back.popup.layer);
        assert_eq!("en", back.ocr.language, "hidden on Linux, never dropped");
        assert_eq!("light", back.popup.theme);
        let _ = std::fs::remove_file(&p);
    }

    /// `popup.layer` accepts exactly `overlay` and `top`.
    #[test]
    fn popup_layer_parses_overlay_and_top_and_rejects_garbage() {
        let base = concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[dictionaries]\ndisplay_order = []\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"X\"\n",
        );
        let parse = |layer_line: &str| {
            toml::from_str::<Config>(&format!("{base}{layer_line}"))
        };
        assert_eq!(PopupLayer::Overlay, parse("layer = \"overlay\"\n").unwrap().popup.layer);
        assert_eq!(PopupLayer::Top, parse("layer = \"top\"\n").unwrap().popup.layer);
        assert_eq!(PopupLayer::Overlay, parse("").unwrap().popup.layer, "absent takes the default");
        assert!(parse("layer = \"bottom\"\n").is_err(), "garbage layers are a parse error");
    }

    /// Confirms that the Linux popup accessor returns the configured layer.
    #[test]
    fn popup_layer_reaches_the_accessor() {
        let mut c = Config::default();
        assert_eq!(PopupLayer::Overlay, c.popup_layer());
        c.popup.layer = PopupLayer::Top;
        assert_eq!(PopupLayer::Top, c.popup_layer());
    }

    /// `popup.layout_mode` accepts exactly `roomy` and `compact`.
    ///
    /// This test matches [`PopupLayer`] and rejects an unknown enum.
    /// It does not choose a silent default.
    /// A config with an unknown mode came from a build that this build does not understand.
    #[test]
    fn layout_mode_parses_roomy_and_compact_and_rejects_garbage() {
        let base = concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[dictionaries]\ndisplay_order = []\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"X\"\n",
        );
        let parse = |line: &str| toml::from_str::<Config>(&format!("{base}{line}"));
        let mode = |line: &str| parse(line).unwrap().popup.layout_mode;
        assert_eq!(LayoutMode::Roomy, mode("layout_mode = \"roomy\"\n"));
        assert_eq!(LayoutMode::Compact, mode("layout_mode = \"compact\"\n"));
        assert_eq!(LayoutMode::Roomy, mode(""), "absent takes the default");
        assert!(parse("layout_mode = \"terse\"\n").is_err(), "garbage modes are a parse error");
    }

    /// A file from before render settings loads with documented defaults.
    /// A save writes those settings.
    ///
    /// The test reads a config from the other platform or an older build.
    /// It makes no changes to the stored values.
    /// The save adds six keys and preserves the rest of the config.
    #[test]
    fn a_file_without_render_settings_takes_the_documented_defaults() {
        let p = tmp("render_defaults");
        let _ = std::fs::remove_file(&p);
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"light\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Meiryo\"\n\n",
            "[dictionaries]\ndisplay_order = [\"Jitendex\"]\n",
        )).unwrap();
        let mut c = load_or_create(&p).unwrap();
        assert_eq!(LayoutMode::Roomy, c.popup.layout_mode);
        assert!(c.popup.dictionary_styling);
        assert!(c.popup.show_examples);
        assert!(c.popup.show_attributions);
        assert!(c.popup.show_images);
        assert!(!c.popup.show_part_of_speech, "the card's own pos field already prints them");
        assert_eq!("Meiryo", c.popup.font, "the stored values are untouched");
        c.save(&p).unwrap();
        let saved = std::fs::read_to_string(&p).unwrap();
        assert!(saved.contains("layout_mode = \"roomy\""));
        assert!(saved.contains("dictionary_styling = true"));
        assert!(saved.contains("show_examples = true"));
        assert!(saved.contains("show_attributions = true"));
        assert!(saved.contains("show_images = true"));
        assert!(saved.contains("show_part_of_speech = false"));
        assert!(!saved.contains("display_order"), "the save retires the pre-roles key");
        c.dictionaries.display_order.clear();
        assert_eq!(c, load_or_create(&p).unwrap());
        let _ = std::fs::remove_file(&p);
    }

    /// The shipped defaults resolve to the scene builder's defaults.
    ///
    /// This test keeps a geometry fixture and a fresh install on the same panel.
    /// Fixtures use `RenderSettings::default()`. A fresh install uses the config settings.
    /// A difference between them moves a golden.
    #[test]
    fn the_shipped_popup_resolves_to_the_default_render_settings() {
        assert_eq!(
            crate::ui::layout::RenderSettings::default(),
            Config::default().popup.render_settings(),
        );
    }

    /// Confirms that each setting reaches its field in the resolved record.
    /// It also confirms both enum choices.
    #[test]
    fn every_render_knob_reaches_the_resolved_record() {
        type Edit = fn(&mut PopupConfig);
        type Want = fn(&crate::ui::layout::RenderSettings) -> bool;
        let resolve = |edit: Edit| {
            let mut c = Config::default();
            edit(&mut c.popup);
            c.popup.render_settings()
        };
        let cases: [(&str, Edit, Want); 7] = [
            ("roomy stacks", |p| p.layout_mode = LayoutMode::Roomy, |r| r.stack_items),
            ("compact joins", |p| p.layout_mode = LayoutMode::Compact, |r| !r.stack_items),
            ("styling off", |p| p.dictionary_styling = false, |r| !r.styling),
            ("images off", |p| p.show_images = false, |r| !r.images),
            ("examples off", |p| p.show_examples = false, |r| !r.roles.examples),
            ("attributions off", |p| p.show_attributions = false, |r| !r.roles.attributions),
            ("pos on", |p| p.show_part_of_speech = true, |r| r.roles.part_of_speech),
        ];
        for (what, edit, want) in cases {
            assert!(want(&resolve(edit)), "{what} did not reach the record");
        }
    }

    /// Confirms that each bin reads only its own platform fields.
    #[test]
    fn key_accessors_pick_the_platforms_field() {
        let mut c = Config::default();
        c.trigger.trigger_key = "f2".to_string();
        c.anki.add_key = "d".to_string();
        assert_eq!("f2", c.trigger_key_for(Platform::Windows));
        assert_eq!("ALT+F", c.trigger_key_for(Platform::Linux));
        assert_eq!("d", c.add_key_for(Platform::Windows));
        assert_eq!("ALT+A", c.add_key_for(Platform::Linux));
    }

    /// Confirms that the code keeps a resolvable literal unchanged.
    #[test]
    fn a_resolvable_font_is_kept() {
        let choice = resolve_font("IPAexGothic", Platform::Linux, |_| true);
        assert_eq!(FontChoice::Configured("IPAexGothic".to_string()), choice);
        assert_eq!("IPAexGothic", choice.family());
    }

    /// Confirms that an unresolvable literal uses the platform default.
    /// It also records the requested family.
    #[test]
    fn an_unresolvable_font_falls_back_to_the_platform_default() {
        let choice = resolve_font("Yu Gothic UI", Platform::Linux, |_| false);
        assert_eq!(
            FontChoice::Fallback {
                requested: "Yu Gothic UI".to_string(),
                family: "Noto Sans CJK JP",
            },
            choice
        );
        assert_eq!("Noto Sans CJK JP", choice.family());
        let windows = resolve_font("Noto Sans CJK JP", Platform::Windows, |_| false);
        assert_eq!("Yu Gothic UI", windows.family());
    }

    /// Confirms that an empty literal skips the resolver and uses the platform default.
    #[test]
    fn an_empty_font_falls_back_without_asking() {
        let choice = resolve_font("", Platform::Linux, |_| panic!("asked about an empty literal"));
        assert_eq!("Noto Sans CJK JP", choice.family());
    }

    // Tests for the plugin engine.

    #[test]
    fn builtin_is_the_default_engine() {
        let c = Config::default();
        assert_eq!(c.ocr.engine, "builtin");
    }

    #[test]
    fn an_engine_naming_a_plugin_that_is_not_enabled_falls_back() {
        let chosen = resolve_engine("manga-ocr", &["meikiocr".to_string()]);
        assert_eq!(chosen, EngineChoice::FellBack("manga-ocr".into()));
    }

    #[test]
    fn an_enabled_plugin_is_chosen() {
        let chosen = resolve_engine("meikiocr", &["meikiocr".to_string()]);
        assert_eq!(chosen, EngineChoice::Plugin("meikiocr".into()));
    }

    /// Confirms that the builtin engine ignores the plugin list.
    #[test]
    fn builtin_wins_even_with_plugins_enabled() {
        let chosen = resolve_engine("builtin", &["meikiocr".to_string()]);
        assert_eq!(chosen, EngineChoice::Builtin);
    }

    #[test]
    fn an_unknown_engine_falls_back_with_no_plugins_enabled() {
        let chosen = resolve_engine("meikiocr", &[]);
        assert_eq!(chosen, EngineChoice::FellBack("meikiocr".into()));
    }

    #[test]
    fn plugins_enabled_defaults_to_empty() {
        assert!(Config::default().plugins.enabled.is_empty());
    }

    #[test]
    fn plugins_enabled_round_trips() {
        let p = tmp("plugins_rt");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.plugins.enabled = vec!["meikiocr".to_string()];
        c.save(&p).unwrap();
        let back = load_or_create(&p).unwrap();
        assert_eq!(vec!["meikiocr".to_string()], back.plugins.enabled);
        let _ = std::fs::remove_file(&p);
    }

    /// Confirms that a missing section loads with defaults.
    #[test]
    fn a_config_without_plugins_section_still_loads() {
        let p = tmp("no_plugins_section");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\", \"Jitendex\"]\n",
        )).unwrap();
        let c = load_or_create(&p).expect("a pre-plugins config must load");
        assert!(c.plugins.enabled.is_empty());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn ocr_engine_round_trips() {
        let p = tmp("engine_rt");
        let _ = std::fs::remove_file(&p);
        let mut c = Config::default();
        c.ocr.engine = "meikiocr".to_string();
        c.save(&p).unwrap();
        assert_eq!("meikiocr", load_or_create(&p).unwrap().ocr.engine);
        let _ = std::fs::remove_file(&p);
    }

    /// Confirms that a missing engine key uses the builtin default.
    #[test]
    fn an_ocr_section_without_engine_still_defaults_to_builtin() {
        let p = tmp("ocr_no_engine");
        std::fs::write(&p, concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = [\"大辞林\"]\n\n",
            "[ocr]\nmax_ocr_passes = 1\n",
        )).unwrap();
        let c = load_or_create(&p).expect("a pre-engine config must load");
        assert_eq!("builtin", c.ocr.engine, "a missing key takes the field default");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn parse_hotkey_single_key() {
        let (vk, mods) = parse_hotkey("a").unwrap();
        assert_eq!(0x41, vk);
        assert_eq!(0, mods);
    }

    #[test]
    fn parse_hotkey_ctrl_shift_s() {
        let (vk, mods) = parse_hotkey("ctrl+shift+s").unwrap();
        assert_eq!(0x53, vk);
        assert_eq!(0b011, mods);
    }

    #[test]
    fn parse_hotkey_alt_f10() {
        let (vk, mods) = parse_hotkey("alt+f10").unwrap();
        assert_eq!(0x79, vk);
        assert_eq!(0b100, mods);
    }

    #[test]
    fn parse_hotkey_case_insensitive() {
        let (vk, mods) = parse_hotkey("Ctrl+Shift+S").unwrap();
        assert_eq!(0x53, vk);
        assert_eq!(0b011, mods);
    }

    #[test]
    fn parse_hotkey_garbage() {
        assert!(parse_hotkey("garbage+garbage").is_none());
    }

    #[test]
    fn parse_hotkey_empty() {
        assert!(parse_hotkey("").is_none());
    }

    #[test]
    fn actions_config_defaults() {
        let cfg = Config::default();
        assert!(cfg.actions.enabled);
        assert_eq!("ctrl+shift+s", cfg.actions.screenshot.hotkey);
        assert_eq!("screenshots", cfg.actions.screenshot.save_dir);
        assert_eq!(None, cfg.actions.ocr_clipboard);
    }

    /// Both platform chords use one nested section.
    /// See ARCHITECTURE.md#settings-and-config.
    #[test]
    fn ocr_clipboard_hotkey_round_trips() {
        let mut cfg = Config::default();
        cfg.actions.ocr_clipboard = Some(OcrClipboardConfig {
            hotkey: Some("ctrl+shift+o".into()),
            hotkey_linux: Some("CTRL+SHIFT+O".into()),
        });
        let text = toml::to_string(&cfg).unwrap();
        let loaded: Config = toml::from_str(&text).unwrap();
        assert_eq!(
            Some(OcrClipboardConfig {
                hotkey: Some("ctrl+shift+o".to_string()),
                hotkey_linux: Some("CTRL+SHIFT+O".to_string()),
            }),
            loaded.actions.ocr_clipboard
        );
    }

    /// Confirms that a Windows-shaped section from before the Linux twin loads.
    #[test]
    fn an_ocr_clipboard_section_without_the_linux_twin_loads_with_it_absent() {
        let toml = concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = []\n\n",
            "[actions.ocr_clipboard]\nhotkey = \"f9\"\n",
        );
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(
            Some(OcrClipboardConfig { hotkey: Some("f9".to_string()), hotkey_linux: None }),
            cfg.actions.ocr_clipboard
        );
    }

    /// Confirms that a config without an `[actions]` section uses defaults.
    #[test]
    fn actions_config_missing_section_uses_defaults() {
        let toml = concat!(
            "[trigger]\nmode = \"live\"\n\n",
            "[popup]\ntheme = \"dark\"\nexclude_from_capture = false\n",
            "max_height_percent = 45\nsummary_chars = 40\nfont = \"Yu Gothic UI\"\n\n",
            "[dictionaries]\ndisplay_order = []\n",
        );
        let cfg: Config = toml::from_str(toml).unwrap();
        assert!(cfg.actions.enabled);
        assert_eq!("ctrl+shift+s", cfg.actions.screenshot.hotkey);
    }

    fn di(id: i64, name: &str) -> crate::present::DictInfo {
        crate::present::DictInfo { dict_id: id, name: name.to_string() }
    }

    fn installed() -> [crate::present::DictInfo; 2] {
        [di(1, "大辞林　第四版"), di(2, "中日大辞典　第二版")]
    }

    /// Builds a pre-roles config in the form used before roles existed.
    fn pre_roles(order: &[&str]) -> Config {
        let mut cfg = Config::default();
        cfg.dictionaries.display_order = order.iter().map(|s| (*s).to_string()).collect();
        cfg
    }

    #[test]
    fn the_active_language_selects_its_own_list_by_exact_name() {
        let mut cfg = Config::default();
        cfg.ocr.language = "zh-Hans-CN".to_string();
        cfg.dictionaries
            .per_language
            .insert("zh-Hans-CN".to_string(), vec!["中日大辞典　第二版".to_string()]);
        let out = cfg.present_config(&installed());
        assert_eq!(vec!["中日大辞典　第二版".to_string()], out.terms);
        assert!(
            !crate::present::keeps_dict("大辞林　第四版", &out.terms),
            "a dictionary the language's list does not name is not searched"
        );
    }

    #[test]
    fn a_language_with_no_list_searches_the_enabled_terms_list() {
        let cfg = Config::default();
        let out = cfg.present_config(&installed());
        assert_eq!(
            vec!["大辞林　第四版".to_string(), "中日大辞典　第二版".to_string()],
            out.terms,
            "a config naming nothing enables every installed dictionary, in library order",
        );
    }

    #[test]
    fn an_empty_language_list_leaves_the_global_list_deciding() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries.per_language.insert("ja".to_string(), Vec::new());
        assert_eq!(2, cfg.present_config(&installed()).terms.len());
    }

    /// An older guard used the unrestricted order when a list matched no installed name.
    /// A mistyped substring could then blank the popup.
    /// Exact names cannot match another Dictionary by mistake.
    /// A list with an absent name searches that name and finds nothing.
    /// That result reflects the config.
    #[test]
    fn a_language_list_naming_nothing_installed_searches_nothing() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries.per_language.insert("ja".to_string(), vec!["Typoo".to_string()]);
        let out = cfg.present_config(&installed());
        assert_eq!(vec!["Typoo".to_string()], out.terms);
        assert!(!crate::present::keeps_dict("大辞林　第四版", &out.terms));
    }

    /// Clearing every checkbox is a valid "search nothing" choice.
    /// The code must not restore the Dictionaries.
    #[test]
    fn a_terms_list_with_every_row_disabled_enables_nothing() {
        let mut cfg = Config::default();
        cfg.dictionaries.terms_disabled =
            vec!["大辞林　第四版".to_string(), "中日大辞典　第二版".to_string()];
        assert!(cfg.present_config(&installed()).terms.is_empty());
    }

    #[test]
    fn an_installed_dictionary_no_array_names_lands_at_the_bottom_enabled() {
        let mut cfg = Config::default();
        cfg.dictionaries.terms = vec!["中日大辞典　第二版".to_string()];
        assert_eq!(
            vec!["中日大辞典　第二版".to_string(), "大辞林　第四版".to_string()],
            cfg.present_config(&installed()).terms,
        );
    }

    /// A disconnected drive must not delete a list.
    /// The config keeps the name, and resolution finds no installed Dictionary for it.
    #[test]
    fn a_name_no_installed_dictionary_answers_to_is_kept_and_ignored() {
        let mut cfg = Config::default();
        cfg.dictionaries.terms = vec!["On the USB stick".to_string()];
        let out = cfg.present_config(&installed());
        assert_eq!(
            vec![
                "On the USB stick".to_string(),
                "大辞林　第四版".to_string(),
                "中日大辞典　第二版".to_string()
            ],
            out.terms,
        );
        assert_eq!(
            vec!["On the USB stick".to_string()],
            cfg.dictionaries.terms,
            "and the file still names it",
        );
    }

    /// Tests the migration through the one method that resolves the legacy list.
    /// Each substring contributes every installed name that matches, in library and list order.
    ///
    /// `大辞` matches both installed names. The exact-name model removes that ambiguity.
    /// A user who wrote this substring selected one Dictionary but received two matches.
    /// Migration to both exact names preserves the result that the old config produced.
    #[test]
    fn a_pre_roles_substring_resolves_to_the_exact_names_it_matched() {
        let one = pre_roles(&["大辞林"]);
        assert_eq!(
            vec![("大辞林　第四版".to_string(), true)],
            one.dictionaries.listed(crate::library::Role::Terms, &installed()),
            "the substring named one dictionary, so it resolves to that one",
        );

        let both = pre_roles(&["大辞"]);
        assert_eq!(
            vec![
                ("大辞林　第四版".to_string(), true),
                ("中日大辞典　第二版".to_string(), true)
            ],
            both.dictionaries.listed(crate::library::Role::Terms, &installed()),
            "one substring, both names it matched, in library order",
        );
    }

    #[test]
    fn a_pre_roles_substring_matching_nothing_is_dropped() {
        let cfg = pre_roles(&["Typoo", "中日大辞典"]);
        assert_eq!(
            vec!["中日大辞典　第二版".to_string(), "大辞林　第四版".to_string()],
            cfg.present_config(&installed()).terms,
            "the matched substring resolved, the missed one contributed nothing, and \
             the dictionary no substring named lands at the bottom",
        );
    }

    #[test]
    fn a_pre_roles_language_list_resolves_the_same_way() {
        let mut cfg = pre_roles(&["大辞", "中日"]);
        cfg.ocr.language = "zh-Hans-CN".to_string();
        cfg.dictionaries
            .per_language
            .insert("zh-Hans-CN".to_string(), vec!["中日大辞典".to_string()]);
        assert_eq!(
            vec!["中日大辞典　第二版".to_string()],
            cfg.present_config(&installed()).terms,
        );
    }

    /// Pitch uses its own list and membership. A per-language list does not narrow it.
    #[test]
    fn the_pitch_list_is_resolved_independently_of_the_terms_list() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries.terms_disabled = vec!["大辞林　第四版".to_string()];
        cfg.dictionaries.pitch = vec!["大辞林　第四版".to_string()];
        cfg.dictionaries.per_language.insert("ja".to_string(), vec!["中日大辞典　第二版".into()]);
        let out = cfg.present_config(&installed());
        assert_eq!(vec!["中日大辞典　第二版".to_string()], out.terms);
        assert_eq!(
            vec!["大辞林　第四版".to_string(), "中日大辞典　第二版".to_string()],
            out.pitch,
            "disabled in terms, still leading the pitch list",
        );
    }

    #[test]
    fn summary_chars_rides_along() {
        let mut cfg = Config::default();
        cfg.popup.summary_chars = 55;
        assert_eq!(55, cfg.present_config(&[]).summary_chars);
    }

    /// Confirms that all six arrays and the ranking strategy survive the TOML
    /// writer that the other platform uses.
    #[test]
    fn all_six_arrays_and_the_strategy_round_trip_through_toml() {
        let mut cfg = Config::default();
        cfg.dictionaries.terms = vec!["A".into(), "B".into()];
        cfg.dictionaries.terms_disabled = vec!["C".into()];
        cfg.dictionaries.frequency = vec!["D".into()];
        cfg.dictionaries.frequency_disabled = vec!["E".into(), "F".into()];
        cfg.dictionaries.pitch = vec!["G".into()];
        cfg.dictionaries.pitch_disabled = vec!["H".into()];
        cfg.dictionaries.ranking_strategy = crate::dict::frequency::RankingStrategy::Median;

        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();

        assert_eq!(cfg.dictionaries, back.dictionaries);
        assert!(!text.contains("display_order"), "the retired key is not written: {text}");
    }

    /// The database records the strategy with `as_str`, and the file records it with serde.
    /// Both paths must use the same three names.
    /// Otherwise the config and its ranking can disagree without an error.
    #[test]
    fn the_toml_spelling_is_the_one_the_database_records() {
        use crate::dict::frequency::RankingStrategy;
        for strategy in
            [RankingStrategy::BestRank, RankingStrategy::Priority, RankingStrategy::Median]
        {
            let mut cfg = Config::default();
            cfg.dictionaries.ranking_strategy = strategy;
            let text = toml::to_string_pretty(&cfg).unwrap();
            assert!(
                text.contains(&format!("ranking_strategy = \"{}\"", strategy.as_str())),
                "{text}",
            );
        }
    }
}
