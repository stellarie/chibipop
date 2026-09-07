//! Typed presentation layout for the Windows settings window.
//!
//! The embedded TOML owns labels and ordering. Rust owns controls and behavior.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const LAYOUT_VERSION: u32 = 1;
const EMBEDDED_LAYOUT: &str = include_str!("../../assets/settings-layout.toml");

pub(super) const SETTING_INVENTORY: [SettingId; 57] = [
    SettingId::ClosePopup,
    SettingId::LookupMode,
    SettingId::LookupKey,
    SettingId::AnkiAddKey,
    SettingId::StaticRegionKey,
    SettingId::ScreenshotKey,
    SettingId::OcrClipboardKey,
    SettingId::SearchKey,
    SettingId::PopupTheme,
    SettingId::PopupFont,
    SettingId::PopupCustomStyle,
    SettingId::PopupMaxWidth,
    SettingId::PopupMaxHeight,
    SettingId::PopupSummaryLength,
    SettingId::PopupHighlight,
    SettingId::PopupScroll,
    SettingId::PopupEdgeAutoscroll,
    SettingId::PopupSidePanel,
    SettingId::PopupCaptureExclusion,
    SettingId::PopupLayout,
    SettingId::PopupDictionaryStyling,
    SettingId::PopupExamples,
    SettingId::PopupAttributions,
    SettingId::PopupImages,
    SettingId::PopupPartOfSpeech,
    SettingId::DictionaryTerms,
    SettingId::DictionaryFrequency,
    SettingId::DictionaryPitch,
    SettingId::OcrEngine,
    SettingId::OcrLanguage,
    SettingId::OcrPasses,
    SettingId::OcrCaptureSize,
    SettingId::OcrPreferVertical,
    SettingId::OcrScanAlphanumeric,
    SettingId::OcrDiscardFurigana,
    SettingId::OcrPerCharacter,
    SettingId::DebugCaptureOutline,
    SettingId::DebugEngine,
    SettingId::DebugAdapter,
    SettingId::ShowLiveLogs,
    SettingId::AnkiEnabled,
    SettingId::AnkiNotifyOnAdd,
    SettingId::AnkiUrl,
    SettingId::AnkiDeck,
    SettingId::AnkiModel,
    SettingId::AnkiRefresh,
    SettingId::AnkiIncludeScreenshot,
    SettingId::ScreenshotTargets,
    SettingId::AnkiIncludeDictionaryName,
    SettingId::AnkiFirstDictionaryOnly,
    SettingId::AnkiSelectionButtons,
    SettingId::AnkiSelectionSeparator,
    SettingId::AnkiTripleClick,
    SettingId::AnkiSentenceMode,
    SettingId::AnkiStaticOverlay,
    SettingId::AnkiFieldMap,
    SettingId::PluginList,
];

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SettingsLayout {
    pub(super) version: u32,
    pub(super) tabs: Vec<TabSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TabSpec {
    pub(super) id: TabId,
    pub(super) label: String,
    pub(super) sections: Vec<SectionSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SectionSpec {
    pub(super) id: SectionId,
    pub(super) label: String,
    pub(super) entries: Vec<EntrySpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EntrySpec {
    pub(super) id: SettingId,
    pub(super) label: String,
    pub(super) help: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum TabId {
    Popup,
    Shortcuts,
    Dictionaries,
    TextRecognition,
    Anki,
    Extensions,
    Debug,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum SectionId {
    PopupAppearance,
    PopupSize,
    PopupBehavior,
    PopupContent,
    ShortcutPopup,
    ShortcutActions,
    DictionaryTerms,
    DictionaryFrequency,
    DictionaryPitch,
    RecognitionEngine,
    RecognitionCapture,
    RecognitionDiagnostics,
    AnkiConnection,
    AnkiCard,
    AnkiSelection,
    AnkiSentence,
    AnkiFieldMap,
    ExtensionPlugins,
    DebugDiagnostics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum SettingId {
    ClosePopup,
    LookupMode,
    LookupKey,
    AnkiAddKey,
    StaticRegionKey,
    ScreenshotKey,
    OcrClipboardKey,
    SearchKey,
    PopupTheme,
    PopupFont,
    PopupCustomStyle,
    PopupMaxWidth,
    PopupMaxHeight,
    PopupSummaryLength,
    PopupHighlight,
    PopupScroll,
    PopupEdgeAutoscroll,
    PopupSidePanel,
    PopupCaptureExclusion,
    PopupLayout,
    PopupDictionaryStyling,
    PopupExamples,
    PopupAttributions,
    PopupImages,
    PopupPartOfSpeech,
    DictionaryTerms,
    DictionaryFrequency,
    DictionaryPitch,
    OcrEngine,
    OcrLanguage,
    OcrPasses,
    OcrCaptureSize,
    OcrPreferVertical,
    OcrScanAlphanumeric,
    OcrDiscardFurigana,
    OcrPerCharacter,
    DebugCaptureOutline,
    DebugEngine,
    DebugAdapter,
    ShowLiveLogs,
    AnkiEnabled,
    AnkiNotifyOnAdd,
    AnkiUrl,
    AnkiDeck,
    AnkiModel,
    AnkiRefresh,
    AnkiIncludeScreenshot,
    ScreenshotTargets,
    AnkiIncludeDictionaryName,
    AnkiFirstDictionaryOnly,
    AnkiSelectionButtons,
    AnkiSelectionSeparator,
    AnkiTripleClick,
    AnkiSentenceMode,
    AnkiStaticOverlay,
    AnkiFieldMap,
    PluginList,
}

impl SettingsLayout {
    pub(super) fn parse(source: &str) -> Result<Self> {
        let layout: Self = toml::from_str(source).context("invalid settings layout TOML")?;
        layout.validate()?;
        Ok(layout)
    }

    pub(super) fn embedded() -> Result<Self> {
        Self::parse(EMBEDDED_LAYOUT).context("invalid embedded settings layout")
    }

    #[cfg(test)]
    pub(super) fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    #[cfg(test)]
    pub(super) fn tab_label(&self, index: usize) -> Option<&str> {
        self.tabs.get(index).map(|tab| tab.label.as_str())
    }

    #[cfg(test)]
    pub(super) fn field_map_tab(&self) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.contains(SettingId::AnkiFieldMap))
    }

    #[cfg(test)]
    pub(super) fn tab_needs_anki_detection(&self, index: usize) -> bool {
        self.tabs.get(index).is_some_and(|tab| {
            [
                SettingId::AnkiDeck,
                SettingId::AnkiModel,
                SettingId::AnkiRefresh,
                SettingId::AnkiFieldMap,
            ]
            .iter()
            .any(|&id| tab.contains(id))
        })
    }

    pub(super) fn validate(&self) -> Result<()> {
        if self.version != LAYOUT_VERSION {
            bail!("unsupported settings layout version {}", self.version);
        }

        let mut tabs = HashSet::new();
        let mut settings = HashSet::new();
        for tab in &self.tabs {
            if !tabs.insert(tab.id) {
                bail!("duplicate tab identifier {:?}", tab.id);
            }
            require_label("tab", &tab.label)?;

            let mut sections = HashSet::new();
            for section in &tab.sections {
                if !sections.insert(section.id) {
                    bail!("duplicate section identifier {:?} in tab {:?}", section.id, tab.id);
                }
                require_label("section", &section.label)?;
                for entry in &section.entries {
                    if !settings.insert(entry.id) {
                        bail!("duplicate setting identifier {:?}", entry.id);
                    }
                    require_label("entry", &entry.label)?;
                }
            }
        }

        for required in SETTING_INVENTORY {
            if !settings.contains(&required) {
                bail!("missing required setting identifier {:?}", required);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
impl TabSpec {
    fn contains(&self, id: SettingId) -> bool {
        self.sections
            .iter()
            .any(|section| section.entries.iter().any(|entry| entry.id == id))
    }
}

fn require_label(kind: &str, label: &str) -> Result<()> {
    if label.trim().is_empty() {
        bail!("{kind} label must not be empty");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serialized(layout: &SettingsLayout) -> String {
        toml::to_string(layout).expect("test layout should serialize")
    }

    fn location(layout: &SettingsLayout, id: SettingId) -> (usize, usize, usize) {
        layout
            .tabs
            .iter()
            .enumerate()
            .find_map(|(tab_index, tab)| {
                tab.sections
                    .iter()
                    .enumerate()
                    .find_map(|(section_index, section)| {
                        section
                            .entries
                            .iter()
                            .position(|entry| entry.id == id)
                            .map(|entry_index| (tab_index, section_index, entry_index))
                    })
            })
            .expect("setting should exist")
    }

    #[test]
    fn embedded_layout_is_complete_and_ordered() {
        let layout = SettingsLayout::embedded().expect("embedded layout should load");
        let labels: Vec<_> = layout.tabs.iter().map(|tab| tab.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Popup",
                "Shortcuts",
                "Dictionaries",
                "Text recognition",
                "Anki",
                "Extensions",
                "Debug",
            ]
        );
        assert_eq!(layout.tab_count(), 7);
        assert_eq!(layout.field_map_tab(), Some(4));
        assert!(layout.tab_needs_anki_detection(4));
        assert!(!layout.tab_needs_anki_detection(0));
        assert_eq!(layout.tab_label(1), Some("Shortcuts"));
        assert_eq!(location(&layout, SettingId::DebugCaptureOutline).0, 6);
        assert_eq!(location(&layout, SettingId::DebugEngine).0, 6);
        assert_eq!(location(&layout, SettingId::DebugAdapter).0, 6);
        assert_eq!(location(&layout, SettingId::ShowLiveLogs).0, 6);

        let ids: HashSet<_> = SETTING_INVENTORY.into_iter().collect();
        assert_eq!(ids.len(), SETTING_INVENTORY.len());
    }

    #[test]
    fn toml_order_controls_tabs_sections_and_entries() {
        let mut layout = SettingsLayout::embedded().expect("embedded layout should load");
        layout.tabs.swap(0, 1);
        layout.tabs[1].sections.swap(0, 1);
        layout.tabs[1].sections[1].entries.swap(0, 1);

        let parsed = SettingsLayout::parse(&serialized(&layout)).expect("layout should load");
        assert_eq!(parsed.tabs[0].id, TabId::Shortcuts);
        assert_eq!(parsed.tabs[1].sections[0].id, SectionId::PopupSize);
        assert_eq!(parsed.tabs[1].sections[1].entries[0].id, SettingId::PopupFont);
    }

    #[test]
    fn moving_entry_between_sections_changes_placement() {
        let mut layout = SettingsLayout::embedded().expect("embedded layout should load");
        let (tab_index, section_index, entry_index) = location(&layout, SettingId::PopupTheme);
        let entry = layout.tabs[tab_index].sections[section_index]
            .entries
            .remove(entry_index);
        layout.tabs[tab_index].sections[2].entries.push(entry);

        let parsed = SettingsLayout::parse(&serialized(&layout)).expect("layout should load");
        assert_eq!(location(&parsed, SettingId::PopupTheme), (0, 2, 5));
    }

    #[test]
    fn accepts_removing_an_empty_tab() {
        let mut layout = SettingsLayout::embedded().expect("embedded layout should load");
        let removed = layout.tabs.remove(1);
        layout.tabs[0].sections.extend(removed.sections);

        let parsed = SettingsLayout::parse(&serialized(&layout)).expect("layout should load");
        assert_eq!(parsed.tab_count(), 6);
        assert_eq!(location(&parsed, SettingId::LookupMode).0, 0);
    }

    #[test]
    fn rejects_malformed_and_missing_fields() {
        let malformed = SettingsLayout::parse("version = [");
        assert!(malformed.unwrap_err().to_string().contains("invalid settings layout TOML"));

        let missing_label = EMBEDDED_LAYOUT.replacen("label = \"Popup\"", "", 1);
        assert!(SettingsLayout::parse(&missing_label).is_err());
    }

    #[test]
    fn rejects_unknown_fields_and_identifiers() {
        let unknown_field = EMBEDDED_LAYOUT.replacen(
            "label = \"Appearance\"",
            "label = \"Appearance\"\ncollapsed = true\n",
            1,
        );
        assert!(SettingsLayout::parse(&unknown_field).is_err());

        let unknown_id = EMBEDDED_LAYOUT.replacen(
            "id = \"popup-theme\"",
            "id = \"unknown-setting\"",
            1,
        );
        assert!(SettingsLayout::parse(&unknown_id).is_err());
    }

    #[test]
    fn rejects_unsupported_version_and_empty_labels() {
        let unsupported = EMBEDDED_LAYOUT.replacen("version = 1", "version = 2", 1);
        let error = SettingsLayout::parse(&unsupported).unwrap_err();
        assert!(error.to_string().contains("unsupported settings layout version 2"));

        let mut layout = SettingsLayout::embedded().expect("embedded layout should load");
        layout.tabs[0].sections[0].entries[0].label = "  ".to_string();
        assert!(SettingsLayout::parse(&serialized(&layout)).is_err());
    }

    #[test]
    fn rejects_duplicate_and_missing_required_entries() {
        let mut duplicate = SettingsLayout::embedded().expect("embedded layout should load");
        let entry = duplicate.tabs[0].sections[0].entries[0].clone();
        duplicate.tabs[0].sections[0].entries.push(entry);
        let error = SettingsLayout::parse(&serialized(&duplicate)).unwrap_err();
        assert!(error.to_string().contains("duplicate setting identifier"));

        let mut missing = SettingsLayout::embedded().expect("embedded layout should load");
        missing.tabs[0].sections[0].entries.remove(0);
        let error = SettingsLayout::parse(&serialized(&missing)).unwrap_err();
        assert!(error.to_string().contains("missing required setting identifier"));
    }

    #[test]
    fn rejects_duplicate_tabs_and_sections() {
        let mut duplicate_tab = SettingsLayout::embedded().expect("embedded layout should load");
        duplicate_tab.tabs.push(duplicate_tab.tabs[0].clone());
        let error = SettingsLayout::parse(&serialized(&duplicate_tab)).unwrap_err();
        assert!(error.to_string().contains("duplicate tab identifier"));

        let mut duplicate_section =
            SettingsLayout::embedded().expect("embedded layout should load");
        let section = duplicate_section.tabs[0].sections[0].clone();
        duplicate_section.tabs[0].sections.push(section);
        let error = SettingsLayout::parse(&serialized(&duplicate_section)).unwrap_err();
        assert!(error.to_string().contains("duplicate section identifier"));
    }
}
