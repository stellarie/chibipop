//! This module loads and saves settings from a TOML file.
//!
//! It stays independent of the `windows` crate so the core remains platform-neutral.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

mod profiles;
mod migration;
pub use profiles::*;

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

/// Keeps application preferences shared across platform-specific Settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ApplicationConfig {
    /// Keeps the daemon reachable after live Settings closes.
    pub background_on_close: bool,
}

/// One effective profile view after resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedConfig {
    pub trigger: TriggerConfig,
    pub popup: PopupConfig,
    pub dictionaries: DictionariesConfig,
    #[serde(default)]
    pub application: ApplicationConfig,
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
    #[serde(default)]
    pub nested_profile: Option<String>,
}

/// Trigger settings for one effective profile view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TriggerConfig {
    pub mode: TriggerMode,
    /// Repeats a lookup for each character.
    pub per_character_lookup: bool,
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            mode: TriggerMode::Live,
            per_character_lookup: false,
        }
    }
}

/// The default OCR language.
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
    /// Shows the popup after one key press and hides it after the next press.
    /// The trigger key latches the hold, so the user can move onto the popup
    /// without a held key. The lookup reads live grabs with the popup masked while latched.
    Toggle,
    /// Runs one lookup at the cursor for each key press, like Yomitan. The
    /// popup stays until the next press finds no text or the user clicks
    /// outside it. The lookup reads a live grab with the popup masked, so a
    /// press over the popup is a miss and hides it. Screen OCR hover never follows.
    /// Hovering existing popup text can still open child popups.
    Press,
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
#[serde(default)]
pub struct PopupConfig {
    pub sub_popups: bool,
    /// The theme name. Use `"dark"` or `"light"`.
    pub theme: String,
    /// Hides the popup from screen capture.
    ///
    /// The default is off so a recorder can capture the popup.
    pub exclude_from_capture: bool,
    /// The allowed popup width as a percent of the monitor.
    pub max_width_percent: u8,
    /// The allowed popup height as a percent of the monitor.
    pub max_height_percent: u8,
    /// The maximum summary length in characters.
    pub summary_chars: usize,
    pub font: String,
    /// Draws a box around the word that the popup defines.
    /// Enables match highlights by default.
    pub highlight_match: bool,
    /// Lets the user scroll a long popup with the wheel.
    /// Enables popup scroll by default.
    pub scroll_popup: bool,
    /// This setting enables auto-scroll when a selection drag reaches the popup edge.
    ///
    /// If `scroll_popup = false`, edge auto-scroll stays disabled.
    /// The default enables edge auto-scroll.
    pub edge_autoscroll: bool,
    /// Places collapsed rows beside the entry instead of below it.
    pub side_panel: bool,
    /// The layer that holds the popup on Linux.
    pub layer: PopupLayer,
    /// Selects a compact or roomy layout.
    pub layout_mode: LayoutMode,
    /// Applies each Dictionary's style.
    ///
    /// When this setting is off, the theme supplies the font and colors for every
    /// entry. The setting also ignores the inline `style` object and the
    /// Dictionary's `styles.css` file.
    /// The default is on.
    /// A Dictionary that styles its entry expects this setting.
    pub dictionary_styling: bool,
    /// Shows example sentences.
    /// The default is on.
    /// A sentence that shows the word in use helps a learner understand
    /// the word.
    pub show_examples: bool,
    /// Shows attributions and footnotes.
    ///
    /// This setting is independent of `show_examples`.
    /// A user can keep the sources without three sentences for each sense.
    /// The default is on.
    /// A license line makes an entry quotable.
    pub show_attributions: bool,
    /// Shows Dictionary images.
    ///
    /// When this setting is off, the code keeps an image's `alt` text because a
    /// gaiji represents a character.
    /// If the code drops the gaiji, a hole appears in the word.
    /// The default is on.
    /// An image node represents a *character* more often than an illustration.
    /// The census found 427 786 nodes with a gaiji marker in
    /// (`docs/research/dict-shapes.md`).
    /// This count supports the default.
    pub show_images: bool,
    /// Shows part-of-speech labels inline.
    ///
    /// The default is off because the card's `pos` field already shows these labels
    /// above the glosses.
    /// Inline labels repeat them.
    /// `gloss::RoleFilter::CARD` drops them for the same reason.
    pub show_part_of_speech: bool,
}

impl Default for PopupConfig {
    fn default() -> Self {
        Self {
            sub_popups: true,
            theme: "dark".to_string(),
            exclude_from_capture: false,
            max_width_percent: 25,
            max_height_percent: 45,
            summary_chars: 40,
            font: Platform::current().default_font().to_string(),
            highlight_match: true,
            scroll_popup: true,
            edge_autoscroll: true,
            side_panel: false,
            layer: PopupLayer::default(),
            layout_mode: LayoutMode::default(),
            dictionary_styling: true,
            show_examples: true,
            show_attributions: true,
            show_images: true,
            show_part_of_speech: false,
        }
    }
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

/// Resolved Dictionary lists for one profile view.
///
/// Each role has ordered enabled and disabled names.
/// Empty lists remain explicit and remain empty.
/// Names stay exact, including unknown Dictionary names.
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
    pub fn listed(
        &self,
        role: crate::library::Role,
    ) -> Vec<(String, bool)> {
        let (on, off) = self.lists(role);
        on.iter()
            .map(|name| (name.clone(), true))
            .chain(off.iter().map(|name| (name.clone(), false)))
            .collect()
    }

    /// Returns enabled Dictionaries for this role in highest-priority order.
    ///
    pub fn enabled(
        &self,
        role: crate::library::Role,
    ) -> Vec<String> {
        self.lists(role).0.to_vec()
    }

    /// Returns the terms that an OCR language searches.
    /// Returns `None` when the language has no own list, so the global terms list decides.
    pub fn language_scope(
        &self,
        language: &str,
    ) -> Option<Vec<String>> {
        self.per_language.get(language).cloned()
    }
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
#[serde(default)]
pub struct OcrConfig {
    /// The number of captures for each hover.
    ///
    /// The default is 1, so multiple captures stay disabled.
    pub max_ocr_passes: u8,
    /// Prefers a tall capture for manga and visual novels.
    pub prefer_vertical: bool,
    /// The capture box width in pixels.
    pub capture_width: i32,
    /// The capture box height in pixels.
    pub capture_height: i32,
    /// Resolves words that contain only Latin characters.
    pub scan_alphanumeric: bool,
    /// Removes geometric ruby lines.
    pub discard_furigana: bool,
    /// The language tag for the OCR recognizer.
    pub language: String,
    /// The OCR engine name. Use `"builtin"` or a plugin name.
    pub engine: String,
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            max_ocr_passes: 1,
            prefer_vertical: false,
            capture_width: 500,
            capture_height: 100,
            scan_alphanumeric: true,
            discard_furigana: true,
            language: default_ocr_language(),
            engine: "builtin".to_string(),
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnkiConfig {
    pub enabled: bool,
    /// Updates one exact duplicate note when enabled.
    pub overwrite_duplicates: bool,
    /// The AnkiConnect URL. The default is the local AnkiConnect endpoint.
    pub url: String,
    /// The deck name. The default is `"Default"`.
    pub deck: String,
    /// The model name. The default is `"Lapis"`.
    pub model: String,
    /// Shows a tray balloon after an add. The default is on.
    pub notify_on_add: bool,
    /// The Anki field for each source value.
    pub field_map: Vec<FieldMapping>,
    /// Defines how the code builds the Anki sentence field.
    /// The default is sentence mode.
    pub sentence_mode: SentenceMode,
    /// The static region as [x, y, w, h], when the user sets one.
    pub static_region: Option<[i32; 4]>,
    /// Makes the teal border visible. The overlay is on by default.
    pub show_static_overlay: bool,
    /// This setting includes each Dictionary name above its Anki glossary group.
    ///
    /// The default keeps Dictionary headings because earlier versions always added them.
    pub include_dictionary_name: bool,
    /// Uses only the entry from the top Dictionary.
    pub first_dict_only: bool,
    /// This setting selects whether the primary button adds to or replaces a selection.
    pub selection_buttons: SelectionButtons,
    /// This setting selects the separator between selected glossary fragments.
    pub selection_separator: SelectionSeparator,
    /// This setting selects the content for a triple-click.
    pub triple_click: TripleClick,
}

impl Default for AnkiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            overwrite_duplicates: false,
            url: "http://localhost:8765".to_string(),
            deck: "Default".to_string(),
            model: "Lapis".to_string(),
            notify_on_add: true,
            field_map: vec![
                FieldMapping { anki_field: "Expression".into(), source: "expression".into() },
                FieldMapping { anki_field: "ExpressionReading".into(), source: "reading".into() },
                FieldMapping { anki_field: "Glossary".into(), source: "glossary".into() },
                FieldMapping { anki_field: "Frequency".into(), source: "frequency".into() },
                FieldMapping { anki_field: "FreqSort".into(), source: "frequency".into() },
            ],
            sentence_mode: SentenceMode::Sentence,
            static_region: None,
            show_static_overlay: true,
            include_dictionary_name: true,
            first_dict_only: false,
            selection_buttons: SelectionButtons::default(),
            selection_separator: SelectionSeparator::default(),
            triple_click: TripleClick::default(),
        }
    }
}


/// Defines the text that the Anki sentence field receives.
///
/// The TOML file uses `lowercase` names: `"sentence"`, `"line"`, `"all"`, and
/// `"static"`.
/// Files from earlier versions use `"line"`, `"all"`, and `"static"`.
/// The parser still reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SentenceMode {
    /// The sentence that contains the hovered word. An add reads a strip around
    /// the word and cuts it at 。！？ (`text::sentence`).
    Sentence,
    /// The OCR line that contains the cursor, cut to the sentence at the cursor.
    Line,
    /// Every line that the hover capture reads.
    All,
    /// Every line inside the region that the user draws. In this mode, a
    /// lookup also reads from that region.
    Static,
}



/// The Ctrl modifier bit.
pub const MOD_CTRL: u8 = 0b001;
/// The Shift modifier bit.
pub const MOD_SHIFT: u8 = 0b010;
/// The Alt modifier bit.
pub const MOD_ALT: u8 = 0b100;

/// The `[actions]` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ActionsConfig {
    /// Enables action handling. The default is on.
    pub enabled: bool,
    pub screenshot: ScreenshotConfig,
    pub ocr_clipboard: Option<OcrClipboardConfig>,
    pub search: SearchConfig,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    pub selected_opens_sentence_search: bool,
}


/// Fixed modes keep a target instead of asking for one on every add.
/// Window identity stays separate from geometry so a moved window remains the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScreenshotMode {
    #[default]
    Region,
    Window,
    FixedRegion,
    FixedWindow,
}

impl ScreenshotMode {
    pub const ALL: [Self; 4] = [Self::Region, Self::Window, Self::FixedRegion, Self::FixedWindow];
}

impl std::fmt::Display for ScreenshotMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Region => "Choose a region",
            Self::Window => "Choose a window",
            Self::FixedRegion => "Reuse one region",
            Self::FixedWindow => "Reuse one window",
        })
    }
}

/// A title alone can identify windows from different applications.
/// Both values must match one window before a fixed screenshot can use its current bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenshotWindow {
    pub app_id: String,
    pub title: String,
}

/// The `[actions.screenshot]` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScreenshotConfig {
    /// The screenshot folder. The default is `screenshots`.
    pub save_dir: String,
    pub include_on_add: bool,
    pub capture_mode: ScreenshotMode,
    /// This rectangle uses global physical pixels, like `anki.static_region`.
    /// An absent target makes the next fixed-region screenshot ask for one.
    pub fixed_region: Option<[i32; 4]>,
    /// The bin resolves this identity for each capture instead of saving stale bounds.
    pub fixed_window: Option<ScreenshotWindow>,
}

/// The `[actions.ocr_clipboard]` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OcrClipboardConfig {
    #[serde(default)]
    pub open_sentence_search: bool,
}



impl Default for ActionsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            screenshot: ScreenshotConfig::default(),
            ocr_clipboard: None,
            search: SearchConfig::default(),
        }
    }
}

impl Default for ScreenshotConfig {
    fn default() -> Self {
        Self {
            save_dir: "screenshots".to_string(),
            include_on_add: false,
            capture_mode: ScreenshotMode::default(),
            fixed_region: None,
            fixed_window: None,
        }
    }
}

/// Returns the VK code and modifier bits from a hotkey string.
pub fn parse_hotkey(s: &str) -> Option<(u16, u8)> {
    let parts: Vec<&str> = s.split('+').collect();
    let (key, mod_parts) = parts.split_last()?;
    let mut mods = 0u8;
    for part in mod_parts {
        match part.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= MOD_CTRL,
            "shift" => mods |= MOD_SHIFT,
            "alt" => mods |= MOD_ALT,
            _ => return None,
        }
    }
    let vk = parse_trigger_key(key.trim())?;
    Some((vk, mods))
}

impl Default for ResolvedConfig {
    /// The values that chibipop uses by default.
    fn default() -> ResolvedConfig {
        ResolvedConfig {
            trigger: TriggerConfig::default(),
            popup: PopupConfig::default(),
            // The default has no Dictionary names.
            // A new installation enables every installed Dictionary in library order.
            // Earlier defaults stored two substrings.
            // Those substrings guessed which Dictionary a user installed.
            dictionaries: DictionariesConfig::default(),
            application: ApplicationConfig::default(),
            plugins: PluginsConfig::default(),
            ocr: OcrConfig::default(),
            debug: DebugConfig::default(),
            anki: AnkiConfig::default(),
            actions: ActionsConfig::default(),
            nested_profile: None,
        }
    }
}


fn linux_hotkey(key: &str) -> String {
    let upper = key.trim().to_ascii_uppercase();
    let mut parts: Vec<_> = upper.split('+').map(str::trim).collect();
    let Some(key) = parts.pop() else { return String::new() };
    let mut mods: Vec<_> = parts.into_iter().map(|m| match m {
        "CONTROL" | "CTRL" => "CTRL", "WIN" | "LOGO" | "SUPER" => "SUPER", _ => m,
    }).collect();
    mods.sort_unstable();
    mods.dedup();
    let key = parse_trigger_key(key).map_or_else(|| key.to_string(), |vk| format!("{vk:X}"));
    format!("{}+{key}", mods.join("+"))
}

impl ResolvedConfig {

    /// Clamps every bounded config value.
    pub(crate) fn clamp_ranges(&mut self, report: Option<&Path>) {
        self.popup.max_width_percent = clamped(
            report,
            "max_width_percent",
            self.popup.max_width_percent,
            MAX_WIDTH_RANGE.0,
            MAX_WIDTH_RANGE.1,
        );
        self.popup.max_height_percent = clamped(
            report,
            "max_height_percent",
            self.popup.max_height_percent,
            MAX_HEIGHT_RANGE.0,
            MAX_HEIGHT_RANGE.1,
        );
        self.popup.summary_chars = clamped(
            report,
            "summary_chars",
            self.popup.summary_chars,
            SUMMARY_RANGE.0,
            SUMMARY_RANGE.1,
        );
        self.ocr.max_ocr_passes = clamped(
            report,
            "max_ocr_passes",
            self.ocr.max_ocr_passes,
            PASSES_RANGE.0,
            PASSES_RANGE.1,
        );
        self.ocr.capture_width = clamped(
            report,
            "ocr.capture_width",
            self.ocr.capture_width,
            CAPTURE_W_RANGE.0,
            CAPTURE_W_RANGE.1,
        );
        self.ocr.capture_height = clamped(
            report,
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
    pub fn present_config(&self) -> crate::present::PresentConfig {
        crate::present::PresentConfig {
            terms: self
                .dictionaries
                .language_scope(&self.ocr.language)
                .unwrap_or_else(|| self.dictionaries.enabled(crate::library::Role::Terms)),
            pitch: self.dictionaries.enabled(crate::library::Role::Pitch),
            summary_chars: self.popup.summary_chars,
        }
    }

}

/// Clamps a value and reports each change.
fn clamped<T>(report: Option<&Path>, field: &str, value: T, lo: T, hi: T) -> T
where
    T: Ord + Copy + std::fmt::Display,
{
    let out = value.clamp(lo, hi);
    if out != value {
        if let Some(path) = report {
            let p = path.display();
            eprintln!("chibipop: {p}: {field} {value} is outside {lo}-{hi}, using {out}");
        }
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
            config.clamp_profiles(Some(path))?;
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
mod tests;
