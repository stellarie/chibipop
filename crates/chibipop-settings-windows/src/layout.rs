//! The settings layout for the page.
//!
//! This module mirrors the types in
//! `crates/chibipop-windows/src/ui/settings_layout.rs` and reads the same
//! asset. Every identifier below the section level stays a string until
//! stage 2 adds typed enums.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const LAYOUT_VERSION: u32 = 1;
const EMBEDDED_LAYOUT: &str = include_str!("../../chibipop-windows/assets/settings-layout.toml");

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutView {
    pub version: u32,
    pub tabs: Vec<TabView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TabView {
    pub id: String,
    pub label: String,
    pub sections: Vec<SectionView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SectionView {
    pub id: String,
    pub label: String,
    pub entries: Vec<EntryView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EntryView {
    pub id: String,
    pub label: String,
    pub help: Option<String>,
    #[serde(default)]
    pub inline_help: bool,
}

impl LayoutView {
    pub fn embedded() -> Result<Self> {
        Self::parse(EMBEDDED_LAYOUT).context("invalid embedded settings layout")
    }

    fn parse(source: &str) -> Result<Self> {
        let layout: Self = toml::from_str(source).context("invalid settings layout TOML")?;
        layout.validate()?;
        Ok(layout)
    }

    fn validate(&self) -> Result<()> {
        if self.version != LAYOUT_VERSION {
            bail!("unsupported settings layout version {}", self.version);
        }
        let mut tabs = HashSet::new();
        for tab in &self.tabs {
            if !tabs.insert(tab.id.as_str()) {
                bail!("duplicate tab identifier {}", tab.id);
            }
            require_label("tab", &tab.label)?;
            let mut sections = HashSet::new();
            for section in &tab.sections {
                if !sections.insert(section.id.as_str()) {
                    bail!("duplicate section identifier {} in tab {}", section.id, tab.id);
                }
                require_label("section", &section.label)?;
                for entry in &section.entries {
                    require_label("entry", &entry.label)?;
                }
            }
        }
        Ok(())
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

    #[test]
    fn parses_shared_asset() {
        let layout = LayoutView::embedded().expect("embedded layout should load");
        let ids: Vec<_> = layout.tabs.iter().map(|tab| tab.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "popup",
                "general",
                "shortcuts",
                "profiles",
                "dictionaries",
                "text-recognition",
                "anki",
                "extensions",
                "debug",
            ]
        );
        assert_eq!(layout.tabs[3].label, "Profiles");
    }
}
