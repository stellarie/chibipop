use super::*;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LegacyDictionaries {
    pub settings: DictionariesConfig,
    pub display_order: Vec<String>,
}

impl LegacyDictionaries {
    pub fn listed(&self, role: crate::library::Role, installed: &[crate::present::DictInfo]) -> Vec<(String, bool)> {
        if self.display_order.is_empty() {
            self.settings.listed(role)
        } else {
            resolve_substrings(&self.display_order, installed).into_iter().map(|name| (name, true)).collect()
        }
    }

    pub fn language_scope(&self, language: &str, installed: &[crate::present::DictInfo]) -> Option<Vec<String>> {
        self.settings.per_language.get(language).map(|names| {
            if self.display_order.is_empty() { names.clone() } else { resolve_substrings(names, installed) }
        })
    }
}

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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedConfig {
    version: u32,
    next_id: u64,
    default_profile: String,
    live_lookup: bool,
    profiles: Vec<Profile>,
    #[serde(default)]
    binds: Vec<Bind>,
    #[serde(default)]
    dictionaries: FrequencyConfig,
    #[serde(default)]
    application: ApplicationConfig,
    #[serde(default)]
    plugins: PluginsConfig,
    #[serde(default)]
    debug: DebugConfig,
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = toml::Value::deserialize(deserializer)?;
        let config = if value.get("profiles").is_some() || value.get("version").is_some() {
            let saved: SavedConfig = value.try_into().map_err(serde::de::Error::custom)?;
            Self {
                version: saved.version, next_id: saved.next_id, default_profile: saved.default_profile,
                live_lookup: saved.live_lookup, profiles: saved.profiles, binds: saved.binds,
                dictionaries: saved.dictionaries, application: saved.application,
                plugins: saved.plugins, debug: saved.debug, legacy_dictionaries: None,
            }
        } else {
            migrate(value).map_err(serde::de::Error::custom)?
        };
        config.validate().map_err(serde::de::Error::custom)?;
        Ok(config)
    }
}

impl Default for Config {
    fn default() -> Self {
        let settings = ProfileSettings::default();
        let mut lookup = Bind::new("lookup".to_string(), BindAction::Lookup);
        lookup.windows = "shift".to_string();
        lookup.linux = "ALT+F".to_string();
        lookup.enabled = false;
        let mut add = Bind::new("anki-add".to_string(), BindAction::AnkiAdd);
        add.windows = "a".to_string();
        add.linux = "ALT+A".to_string();
        add.enabled = false;
        Self {
            version: 1, next_id: 1, default_profile: "default".to_string(), live_lookup: true,
            profiles: vec![Profile { id: "default".to_string(), name: "Default".to_string(),
                data: ProfileData::Full { settings: Box::new(settings) } }],
            binds: vec![lookup, add], dictionaries: FrequencyConfig::default(),
            application: ApplicationConfig::default(), plugins: PluginsConfig::default(),
            debug: DebugConfig::default(), legacy_dictionaries: None,
        }
    }
}

fn old_string(value: &toml::Value, section: &[&str], default: &str) -> String {
    let mut entry = value;
    for part in section {
        let Some(next) = entry.get(part) else { return default.to_string() };
        entry = next;
    }
    entry.as_str().unwrap_or(default).to_string()
}

fn migrate(mut value: toml::Value) -> Result<Config> {
    let display_order: Vec<String> = value.get_mut("dictionaries")
        .and_then(toml::Value::as_table_mut)
        .and_then(|dicts| dicts.remove("display_order"))
        .map(|order| order.try_into()).transpose().context("Read the legacy dictionary order.")?
        .unwrap_or_default();
    let mut old: ResolvedConfig = value.clone().try_into().context("Read the legacy configuration.")?;
    let hold_shift = old.trigger.mode == TriggerMode::HoldShift;
    if hold_shift { old.trigger.mode = TriggerMode::HoldKey; }
    old.clamp_ranges(None);
    let bindings = [
        ("lookup", BindAction::Lookup, &["trigger", "trigger_key"][..], &["trigger", "trigger_key_linux"][..], "shift", "ALT+F"),
        ("anki-add", BindAction::AnkiAdd, &["anki", "add_key"][..], &["anki", "add_key_linux"][..], "a", "ALT+A"),
        ("static-region", BindAction::StaticRegion, &["anki", "static_region_key"][..], &["anki", "static_region_key_linux"][..], "", ""),
        ("ocr-clipboard", BindAction::OcrClipboard, &["actions", "ocr_clipboard", "hotkey"][..], &["actions", "ocr_clipboard", "hotkey_linux"][..], "", ""),
        ("search", BindAction::Search, &["actions", "search", "hotkey"][..], &["actions", "search", "hotkey_linux"][..], "", ""),
        ("sentence-search", BindAction::SentenceSearch, &["actions", "search", "sentence_hotkey"][..], &["actions", "search", "sentence_hotkey_linux"][..], "", ""),
        ("selected-text", BindAction::SelectedText, &["actions", "search", "selected_hotkey"][..], &["actions", "search", "selected_hotkey_linux"][..], "", ""),
    ];
    let binds = bindings.into_iter().map(|(id, action, windows, linux, win_default, linux_default)| {
        let mut bind = Bind::new(id.to_string(), action);
        bind.windows = if action == BindAction::Lookup && hold_shift {
            "shift".to_string()
        } else {
            old_string(&value, windows, win_default)
        };
        bind.linux = old_string(&value, linux, linux_default);
        bind.enabled = match action {
            BindAction::Lookup => old.trigger.mode != TriggerMode::Live,
            BindAction::AnkiAdd => old.anki.enabled,
            BindAction::StaticRegion => old.actions.enabled && old.anki.sentence_mode == SentenceMode::Static,
            _ => old.actions.enabled,
        };
        if action == BindAction::Lookup {
            bind.mode = if old.trigger.mode == TriggerMode::Live { TriggerMode::Press } else { old.trigger.mode };
        }
        bind
    }).collect();
    let settings = ProfileSettings::from_resolved(&old);
    Ok(Config {
        next_id: 1,
        version: 1, default_profile: "default".to_string(), live_lookup: old.trigger.mode == TriggerMode::Live,
        profiles: vec![Profile { id: "default".to_string(), name: "Default".to_string(),
            data: ProfileData::Full { settings: Box::new(settings) } }],
        binds, dictionaries: FrequencyConfig {
            frequency: old.dictionaries.frequency.clone(), frequency_disabled: old.dictionaries.frequency_disabled.clone(),
            ranking_strategy: old.dictionaries.ranking_strategy,
        }, application: old.application, plugins: old.plugins, debug: old.debug,
        legacy_dictionaries: Some(LegacyDictionaries { settings: old.dictionaries, display_order }),
    })
}
