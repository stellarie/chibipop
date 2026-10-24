//! Typed presentation layout for the Windows settings window.
//!
//! The embedded TOML owns labels and ordering. Rust owns controls and behavior.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const LAYOUT_VERSION: u32 = 1;
const EMBEDDED_LAYOUT: &str = include_str!("../../assets/settings-layout.toml");

pub(super) const SETTING_INVENTORY: [SettingId; 62] = [
    SettingId::LookupMode,
    SettingId::SelectedTextSentenceSearch,
    SettingId::OpenDictionarySearch,
    SettingId::OpenSentenceSearch,
    SettingId::OcrSentenceSearch,
    SettingId::ConfiguredBinds,
    SettingId::ProfileCatalog,
    SettingId::PopupSubPopups,
    SettingId::NestedProfile,
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
    SettingId::PopupHideOnCursorExit,
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
    SettingId::ClearLookupCache,
    SettingId::AnkiEnabled,
    SettingId::AnkiNotifyOnAdd,
    SettingId::AnkiOverwriteDuplicates,
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
    SettingId::BackgroundOnClose,
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
    /// Keep the help text on the page instead of a hover tooltip.
    ///
    /// The default is a tooltip, which keeps the window short. Set this only
    /// when the text prevents a privacy or data surprise, because a tooltip is
    /// invisible to a keyboard-only user.
    #[serde(default)]
    pub(super) inline_help: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum TabId {
    Popup,
    General,
    Shortcuts,
    Profiles,
    Dictionaries,
    TextRecognition,
    Anki,
    Extensions,
    Debug,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum SectionId {
    DictionarySearch,
    PopupAppearance,
    PopupSize,
    PopupBehavior,
    PopupContent,
    LookupBehavior,
    ShortcutActions,
    ProfileManagement,
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
    WindowBehavior,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum SettingId {
    LookupMode,
    SelectedTextSentenceSearch,
    OpenDictionarySearch,
    OpenSentenceSearch,
    OcrSentenceSearch,
    ConfiguredBinds,
    ProfileCatalog,
    PopupSubPopups,
    NestedProfile,
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
    PopupHideOnCursorExit,
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
    ClearLookupCache,
    AnkiEnabled,
    AnkiNotifyOnAdd,
    AnkiOverwriteDuplicates,
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
    BackgroundOnClose,
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
                "General",
                "Shortcuts",
                "Profiles",
                "Dictionaries",
                "Text recognition",
                "Anki",
                "Extensions",
                "Debug",
            ]
        );
        assert_eq!(layout.tab_count(), 9);
        assert_eq!(layout.field_map_tab(), Some(6));
        assert!(layout.tab_needs_anki_detection(6));
        assert!(!layout.tab_needs_anki_detection(0));
        assert_eq!(layout.tab_label(2), Some("Shortcuts"));
        assert_eq!(layout.tab_label(3), Some("Profiles"));
        assert_eq!(location(&layout, SettingId::ConfiguredBinds).0, 2);
        assert_eq!(location(&layout, SettingId::ProfileCatalog).0, 3);
        assert_eq!(location(&layout, SettingId::NestedProfile).0, 0);
        for option in [
            SettingId::OcrSentenceSearch,
            SettingId::SelectedTextSentenceSearch,
        ] {
            assert_eq!(location(&layout, option).0, 2);
        }
        let (tab, section, entry) = location(&layout, SettingId::AnkiEnabled);
        assert_eq!(layout.tabs[tab].sections[section].entries[entry].label, "Enable Anki");
        assert_eq!(location(&layout, SettingId::BackgroundOnClose).0, 1);
        assert_eq!(location(&layout, SettingId::DebugCaptureOutline).0, 8);
        assert_eq!(location(&layout, SettingId::DebugEngine).0, 8);
        assert_eq!(location(&layout, SettingId::DebugAdapter).0, 8);
        assert_eq!(location(&layout, SettingId::ShowLiveLogs).0, 8);
        assert_eq!(location(&layout, SettingId::ClearLookupCache).0, 8);

        let ids: HashSet<_> = SETTING_INVENTORY.into_iter().collect();
        assert_eq!(ids.len(), SETTING_INVENTORY.len());
    }

    #[test]
    fn toml_order_controls_tabs_sections_and_entries() {
        let mut layout = SettingsLayout::embedded().expect("embedded layout should load");
        layout.tabs.swap(0, 2);
        layout.tabs[2].sections.swap(0, 1);
        layout.tabs[2].sections[1].entries.swap(0, 1);

        let parsed = SettingsLayout::parse(&serialized(&layout)).expect("layout should load");
        assert_eq!(parsed.tabs[0].id, TabId::Shortcuts);
        assert_eq!(parsed.tabs[2].sections[0].id, SectionId::PopupSize);
        assert_eq!(parsed.tabs[2].sections[1].entries[0].id, SettingId::PopupFont);
    }
}
