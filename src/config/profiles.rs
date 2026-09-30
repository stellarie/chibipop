use super::*;
use std::collections::BTreeSet;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Config {
    pub version: u32,
    pub next_id: u64,
    pub default_profile: String,
    pub live_lookup: bool,
    pub profiles: Vec<Profile>,
    pub binds: Vec<Bind>,
    pub dictionaries: FrequencyConfig,
    pub application: ApplicationConfig,
    pub plugins: PluginsConfig,
    pub debug: DebugConfig,
    #[serde(skip)]
    pub(crate) legacy_dictionaries: Option<super::migration::LegacyDictionaries>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FrequencyConfig {
    pub frequency: Vec<String>,
    pub frequency_disabled: Vec<String>,
    pub ranking_strategy: crate::dict::frequency::RankingStrategy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub data: ProfileData,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ProfileData {
    Full { settings: Box<ProfileSettings> },
    Derived { parent: String, #[serde(default)] overrides: BTreeMap<String, FieldOverride> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "kebab-case")]
pub enum FieldOverride {
    Set(toml::Value),
    Clear,
}

impl FieldOverride {
    pub fn from_value<T: Serialize>(value: T) -> Result<Self> {
        Ok(Self::Set(toml::Value::try_from(value)?))
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProfileSettings {
    pub popup: PopupConfig,
    pub ocr: OcrConfig,
    pub per_character_lookup: bool,
    pub dictionaries: ProfileDictionaries,
    pub anki: AnkiConfig,
    pub actions: ActionsConfig,
    pub nested_profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProfileDictionaries {
    pub terms: RoleList,
    pub pitch: RoleList,
    pub per_language: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RoleList {
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BindAction {
    Lookup,
    SelectedText,
    Search,
    SentenceSearch,
    OcrClipboard,
    AnkiAdd,
    StaticRegion,
}

impl BindAction {
    pub const ALL: [Self; 7] = [Self::Lookup, Self::SelectedText, Self::Search,
        Self::SentenceSearch, Self::OcrClipboard, Self::AnkiAdd, Self::StaticRegion];

    pub fn name(self) -> &'static str {
        match self {
            Self::Lookup => "Screen lookup",
            Self::SelectedText => "Selected text",
            Self::Search => "Search",
            Self::SentenceSearch => "Sentence search",
            Self::OcrClipboard => "OCR to clipboard",
            Self::AnkiAdd => "Anki add",
            Self::StaticRegion => "Set sentence area",
        }
    }

    pub fn allows_profile(self) -> bool {
        !matches!(self, Self::AnkiAdd | Self::StaticRegion)
    }
}

impl std::fmt::Display for BindAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bind {
    pub id: String,
    pub action: BindAction,
    pub windows: String,
    pub linux: String,
    pub mode: TriggerMode,
    pub profile: Option<String>,
    pub enabled: bool,
}

impl Bind {
    pub fn new(id: String, action: BindAction) -> Self {
        Self { id, action, windows: String::new(), linux: String::new(),
            mode: TriggerMode::Press, profile: None, enabled: true }
    }

    pub fn chord(&self, platform: Platform) -> &str {
        if platform == Platform::Windows { &self.windows } else { &self.linux }
    }

    pub fn windows_key(&self) -> Option<(u16, Option<u8>)> {
        let (key, modifiers) = parse_hotkey(&self.windows)?;
        let wildcard = !self.windows.contains('+')
            && matches!(self.action, BindAction::Lookup | BindAction::AnkiAdd);
        Some((key, (!wildcard).then_some(modifiers)))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedProfile {
    pub id: String,
    pub name: String,
    pub config: ResolvedConfig,
    pub nested_profile: Option<String>,
    present_config: crate::present::PresentConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProfileCatalog {
    pub config: Config,
    profiles: BTreeMap<String, Arc<ResolvedProfile>>,
}

#[derive(Debug, Clone)]
pub struct ProfileSession {
    pub catalog: Arc<ProfileCatalog>,
    pub profile: Arc<ResolvedProfile>,
}

impl PartialEq for ProfileSession {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.catalog, &other.catalog) && Arc::ptr_eq(&self.profile, &other.profile)
    }
}

impl Eq for ProfileSession {}

impl ProfileCatalog {
    pub fn new(config: &Config, dicts: &[crate::present::DictInfo]) -> Result<Arc<Self>> {
        let mut config = config.clone();
        config.migrate_dictionary_lists(dicts);
        config.validate()?;
        let mut profiles = BTreeMap::new();
        for profile in &config.profiles {
            let settings = config.resolve(&profile.id)?;
            let resolved = settings.materialize(&config);
            let present_config = resolved.present_config();
            profiles.insert(profile.id.clone(), Arc::new(ResolvedProfile {
                id: profile.id.clone(), name: profile.name.clone(),
                nested_profile: settings.nested_profile.clone(),
                config: resolved, present_config,
            }));
        }
        Ok(Arc::new(Self { config, profiles }))
    }

    pub fn session(self: &Arc<Self>, id: Option<&str>) -> Result<ProfileSession> {
        let id = id.unwrap_or(&self.config.default_profile);
        let profile = self.profiles.get(id)
            .with_context(|| format!("Profile {id:?} does not exist."))?;
        Ok(ProfileSession { catalog: Arc::clone(self), profile: Arc::clone(profile) })
    }

    pub fn for_bind(self: &Arc<Self>, id: &str) -> Result<ProfileSession> {
        let bind = self.config.binds.iter().find(|bind| bind.id == id && bind.enabled)
            .with_context(|| format!("Bind {id:?} is not configured."))?;
        self.session(bind.profile.as_deref())
    }
}

impl ProfileSession {
    pub fn id(&self) -> &str { &self.profile.id }

    pub fn config(&self) -> &ResolvedConfig { &self.profile.config }

    pub fn present_config(&self) -> &crate::present::PresentConfig { &self.profile.present_config }

    pub fn nested(&self) -> Self {
        let id = self.profile.nested_profile.as_deref().unwrap_or(self.id());
        Self { catalog: Arc::clone(&self.catalog), profile: Arc::clone(&self.catalog.profiles[id]) }
    }
}

impl ProfileSettings {
    pub fn from_resolved(config: &ResolvedConfig) -> Self {
        Self {
            popup: config.popup.clone(), ocr: config.ocr.clone(),
            per_character_lookup: config.trigger.per_character_lookup,
            dictionaries: ProfileDictionaries {
                terms: RoleList { enabled: config.dictionaries.terms.clone(),
                    disabled: config.dictionaries.terms_disabled.clone() },
                pitch: RoleList { enabled: config.dictionaries.pitch.clone(),
                    disabled: config.dictionaries.pitch_disabled.clone() },
                per_language: config.dictionaries.per_language.clone(),
            },
            anki: config.anki.clone(), actions: config.actions.clone(), nested_profile: config.nested_profile.clone(),
        }
    }

    pub fn materialize(&self, config: &Config) -> ResolvedConfig {
        ResolvedConfig {
            trigger: TriggerConfig { mode: if config.live_lookup { TriggerMode::Live } else { TriggerMode::Press },
                per_character_lookup: self.per_character_lookup },
            popup: self.popup.clone(), ocr: self.ocr.clone(), anki: self.anki.clone(),
            actions: self.actions.clone(), application: config.application.clone(),
            plugins: config.plugins.clone(), debug: config.debug.clone(),
            nested_profile: self.nested_profile.clone(),
            dictionaries: DictionariesConfig {
                terms: self.dictionaries.terms.enabled.clone(), terms_disabled: self.dictionaries.terms.disabled.clone(),
                pitch: self.dictionaries.pitch.enabled.clone(), pitch_disabled: self.dictionaries.pitch.disabled.clone(),
                per_language: self.dictionaries.per_language.clone(),
                frequency: config.dictionaries.frequency.clone(),
                frequency_disabled: config.dictionaries.frequency_disabled.clone(),
                ranking_strategy: config.dictionaries.ranking_strategy,
            },
        }
    }

    pub fn field(&self, path: &str) -> Result<FieldOverride> {
        check_field(path)?;
        let value = toml::Value::try_from(self)?;
        Ok(field_value(&value, path).cloned().map_or(FieldOverride::Clear, FieldOverride::Set))
    }

    pub fn set_field(&mut self, path: &str, value: FieldOverride) -> Result<()> {
        let mut fields = BTreeMap::new();
        fields.insert(path.to_string(), value);
        *self = apply_overrides(self, &fields)?;
        Ok(())
    }
}

pub const PROFILE_FIELDS: &[&str] = &[
    "popup.sub_popups", "popup.theme", "popup.exclude_from_capture", "popup.max_width_percent",
    "popup.max_height_percent", "popup.summary_chars", "popup.font", "popup.highlight_match",
    "popup.scroll_popup", "popup.edge_autoscroll", "popup.side_panel", "popup.layer", "popup.layout_mode",
    "popup.dictionary_styling", "popup.show_examples", "popup.show_attributions", "popup.show_images",
    "popup.show_part_of_speech", "ocr.max_ocr_passes", "ocr.prefer_vertical", "ocr.capture_width",
    "ocr.capture_height", "ocr.scan_alphanumeric", "ocr.discard_furigana", "ocr.language", "ocr.engine",
    "per_character_lookup", "dictionaries.terms", "dictionaries.pitch", "dictionaries.per_language",
    "anki.enabled", "anki.overwrite_duplicates", "anki.url", "anki.deck", "anki.model", "anki.notify_on_add",
    "anki.field_map", "anki.sentence_mode", "anki.static_region", "anki.show_static_overlay",
    "anki.include_dictionary_name", "anki.first_dict_only", "anki.selection_buttons",
    "anki.selection_separator", "anki.triple_click", "actions.enabled", "actions.screenshot.save_dir",
    "actions.screenshot.include_on_add", "actions.screenshot.capture_mode", "actions.screenshot.fixed_region",
    "actions.screenshot.fixed_window", "actions.ocr_clipboard", "actions.search.selected_opens_sentence_search",
    "nested_profile",
];

fn check_field(path: &str) -> Result<()> {
    anyhow::ensure!(PROFILE_FIELDS.contains(&path), "Profile field {path:?} does not exist.");
    Ok(())
}

fn field_value<'a>(mut value: &'a toml::Value, path: &str) -> Option<&'a toml::Value> {
    for part in path.split('.') { value = value.get(part)?; }
    Some(value)
}

fn apply_overrides(settings: &ProfileSettings, overrides: &BTreeMap<String, FieldOverride>) -> Result<ProfileSettings> {
    let mut value = toml::Value::try_from(settings)?;
    for (path, change) in overrides {
        check_field(path)?;
        let mut parts = path.split('.').peekable();
        let mut table = value.as_table_mut().expect("profile table");
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                match change {
                    FieldOverride::Set(value) => { table.insert(part.to_string(), value.clone()); }
                    FieldOverride::Clear => {
                        anyhow::ensure!(matches!(path.as_str(), "nested_profile" | "anki.static_region"
                            | "actions.screenshot.fixed_region" | "actions.screenshot.fixed_window"
                            | "actions.ocr_clipboard"), "Profile field {path:?} cannot be cleared.");
                        table.remove(part);
                    }
                }
                break;
            }
            table = table.entry(part).or_insert_with(|| toml::Value::Table(toml::Table::new()))
                .as_table_mut().with_context(|| format!("Profile field {path:?} is not a section."))?;
        }
    }
    value.try_into().context("Read profile overrides.")
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

impl Config {
    pub fn resolve(&self, id: &str) -> Result<ProfileSettings> {
        let profile = self.profiles.iter().find(|profile| profile.id == id)
            .with_context(|| format!("Profile {id:?} does not exist."))?;
        match &profile.data {
            ProfileData::Full { settings } => Ok((**settings).clone()),
            ProfileData::Derived { parent, overrides } => {
                let parent = self.profiles.iter().find(|profile| &profile.id == parent)
                    .with_context(|| format!("Parent profile {parent:?} does not exist."))?;
                let ProfileData::Full { settings } = &parent.data else {
                    anyhow::bail!("A derived profile must inherit a full profile.");
                };
                apply_overrides(settings, overrides)
            }
        }
    }

    pub fn resolved(&self, id: Option<&str>) -> Result<ResolvedConfig> {
        Ok(self.resolve(id.unwrap_or(&self.default_profile))?.materialize(self))
    }

    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.version == 1, "Configuration version {} is not supported.", self.version);
        let mut ids = BTreeSet::new();
        for profile in &self.profiles {
            anyhow::ensure!(valid_id(&profile.id), "Profile ID {:?} is invalid.", profile.id);
            anyhow::ensure!(ids.insert(profile.id.as_str()), "Profile ID {:?} occurs more than once.", profile.id);
            anyhow::ensure!(!profile.name.trim().is_empty(), "A profile name cannot be empty.");
        }
        anyhow::ensure!(ids.contains(self.default_profile.as_str()), "The default profile does not exist.");
        for profile in &self.profiles {
            let settings = self.resolve(&profile.id)?;
            if let Some(child) = &settings.nested_profile {
                anyhow::ensure!(ids.contains(child.as_str()), "Nested profile {child:?} does not exist.");
            }
        }
        let mut bind_ids = BTreeSet::new();
        for bind in &self.binds {
            anyhow::ensure!(valid_id(&bind.id), "Bind ID {:?} is invalid.", bind.id);
            anyhow::ensure!(bind_ids.insert(bind.id.as_str()), "Bind ID {:?} occurs more than once.", bind.id);
            anyhow::ensure!(matches!(bind.mode, TriggerMode::Press | TriggerMode::HoldKey | TriggerMode::Toggle),
                "A bind must use Press, Hold, or Toggle mode.");
            anyhow::ensure!(bind.action == BindAction::Lookup || bind.mode == TriggerMode::Press,
                "Only screen lookup binds have trigger modes.");
            if let Some(profile) = &bind.profile {
                anyhow::ensure!(bind.action.allows_profile(), "This action uses the displayed popup's profile.");
                anyhow::ensure!(ids.contains(profile.as_str()), "Bind profile {profile:?} does not exist.");
            }
        }
        Ok(())
    }

    pub(crate) fn profile_references<'a>(&'a self, profile: &'a Profile) -> [Option<&'a str>; 2] {
        match &profile.data {
            ProfileData::Full { settings } => [None, settings.nested_profile.as_deref()],
            ProfileData::Derived { parent, overrides } => {
                let nested = match overrides.get("nested_profile") {
                    Some(FieldOverride::Set(value)) => value.as_str(),
                    Some(FieldOverride::Clear) => None,
                    None => self.profiles.iter().find(|source| source.id == *parent)
                        .and_then(|source| match &source.data {
                            ProfileData::Full { settings } => settings.nested_profile.as_deref(),
                            ProfileData::Derived { .. } => None,
                        }),
                };
                [Some(parent.as_str()), nested]
            }
        }
    }

    pub fn references_to(&self, id: &str) -> Vec<String> {
        let mut refs = Vec::new();
        if self.default_profile == id { refs.push("Default profile".to_string()); }
        for bind in &self.binds {
            if bind.profile.as_deref() == Some(id) { refs.push(format!("Bind {}", bind.id)); }
        }
        for profile in &self.profiles {
            let [parent, nested] = self.profile_references(profile);
            if parent == Some(id) { refs.push(format!("Parent of {}", profile.name)); }
            if nested == Some(id) { refs.push(format!("Nested lookup from {}", profile.name)); }
        }
        refs
    }

    pub fn remove_profile(&mut self, id: &str) -> Result<()> {
        let refs = self.references_to(id);
        anyhow::ensure!(refs.is_empty(), "Reassign these profile references first: {}.", refs.join(", "));
        let at = self.profiles.iter().position(|profile| profile.id == id)
            .with_context(|| format!("Profile {id:?} does not exist."))?;
        self.profiles.remove(at);
        Ok(())
    }

    pub fn next_profile_id(&mut self) -> String {
        next_id("profile", &mut self.next_id, self.profiles.iter().map(|profile| profile.id.as_str()))
    }

    pub fn next_bind_id(&mut self) -> String {
        next_id("bind", &mut self.next_id, self.binds.iter().map(|bind| bind.id.as_str()))
    }

    pub fn update_profile(&mut self, id: &str, settings: &ProfileSettings) -> Result<()> {
        let before = self.resolve(id)?;
        let profile = self.profiles.iter_mut().find(|profile| profile.id == id).expect("resolved profile");
        match &mut profile.data {
            ProfileData::Full { settings: saved } => **saved = settings.clone(),
            ProfileData::Derived { overrides, .. } => {
                let before = toml::Value::try_from(&before)?;
                let after = toml::Value::try_from(settings)?;
                for path in PROFILE_FIELDS {
                    let value = field_value(&after, path);
                    if field_value(&before, path) != value {
                        overrides.insert((*path).to_string(),
                            value.cloned().map_or(FieldOverride::Clear, FieldOverride::Set));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn reset_override(&mut self, id: &str, path: &str) -> Result<()> {
        check_field(path)?;
        let profile = self.profiles.iter_mut().find(|profile| profile.id == id)
            .with_context(|| format!("Profile {id:?} does not exist."))?;
        let ProfileData::Derived { overrides, .. } = &mut profile.data else {
            anyhow::bail!("A full profile has no inherited fields.");
        };
        overrides.remove(path);
        Ok(())
    }

    pub(crate) fn clamp_profiles(&mut self, report: Option<&Path>) -> Result<()> {
        let ids: Vec<_> = [false, true].into_iter().flat_map(|derived| self.profiles.iter()
            .filter(move |profile| matches!(profile.data, ProfileData::Derived { .. }) == derived)
            .map(|profile| profile.id.clone())).collect();
        for id in ids {
            let mut resolved = self.resolved(Some(&id))?;
            resolved.clamp_ranges(report);
            self.update_profile(&id, &ProfileSettings::from_resolved(&resolved))?;
        }
        Ok(())
    }

    pub fn migrate_dictionary_lists(&mut self, dicts: &[crate::present::DictInfo]) {
        let Some(legacy) = self.legacy_dictionaries.take() else { return };
        let ProfileData::Full { settings } = &mut self.profiles[0].data else { return };
        let enabled = |role| {
            let listed = legacy.listed(role, dicts);
            let mut names: Vec<String> = listed.iter()
                .filter(|(_, enabled)| *enabled).map(|(name, _)| name.clone()).collect();
            for dict in dicts {
                if !listed.iter().any(|(name, _)| name == &dict.name) { names.push(dict.name.clone()); }
            }
            names
        };
        settings.dictionaries.terms = RoleList {
            enabled: enabled(crate::library::Role::Terms),
            disabled: legacy.settings.terms_disabled.clone(),
        };
        settings.dictionaries.pitch = RoleList {
            enabled: enabled(crate::library::Role::Pitch),
            disabled: legacy.settings.pitch_disabled.clone(),
        };
        self.dictionaries.frequency = enabled(crate::library::Role::Frequency);
        settings.dictionaries.per_language.clear();
        for (language, names) in &legacy.settings.per_language {
            if names.is_empty() { continue; }
            if let Some(scope) = legacy.language_scope(language, dicts) {
                settings.dictionaries.per_language.insert(language.clone(), scope);
            }
        }
    }

    pub fn validate_hotkeys(&self, platform: Platform) -> Result<()> {
        self.validate()?;
        if platform == Platform::Windows {
            let mut accepted = vec![("Escape", (0x1B, None))];
            for bind in self.binds.iter().filter(|bind| bind.enabled && !bind.windows.trim().is_empty()) {
                let key = bind.windows_key()
                    .with_context(|| format!("Shortcut for bind {:?} is invalid.", bind.id))?;
                for (id, previous) in &accepted {
                    anyhow::ensure!(!configured_windows_overlap(*previous, key),
                        "Bind {:?} conflicts with {id:?}. Choose different keys.", bind.id);
                }
                accepted.push((&bind.id, key));
            }
        } else {
            let mut accepted: Vec<(&str, String)> = Vec::new();
            for bind in self.binds.iter().filter(|bind| bind.enabled && !bind.linux.trim().is_empty()) {
                let key = linux_hotkey(&bind.linux);
                for (id, previous) in &accepted {
                    anyhow::ensure!(previous != &key,
                        "Bind {:?} conflicts with {id:?}. Choose different keys.", bind.id);
                }
                accepted.push((&bind.id, key));
            }
        }
        Ok(())
    }

    pub fn from_toml(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    pub fn to_toml(&self) -> Result<String> {
        self.validate()?;
        let mut text = toml::to_string_pretty(self)?;
        if !text.ends_with('\n') { text.push('\n'); }
        Ok(text)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = self.to_toml()
            .with_context(|| format!("Write configuration for {}.", path.display()))?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("Write configuration to {}.", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("Replace configuration at {}.", path.display()))?;
        Ok(())
    }
}

fn next_id<'a>(prefix: &str, counter: &mut u64, ids: impl Iterator<Item = &'a str>) -> String {
    let used: BTreeSet<_> = ids.collect();
    loop {
        let id = format!("{prefix}-{counter}");
        *counter = counter.checked_add(1).expect("profile identity counter");
        if !used.contains(id.as_str()) { return id; }
    }
}

fn configured_windows_overlap((a, am): (u16, Option<u8>), (b, bm): (u16, Option<u8>)) -> bool {
    let family = |vk| match vk { 0xA0 | 0xA1 => 0x10, 0xA2 | 0xA3 => 0x11, 0xA4 | 0xA5 => 0x12, _ => vk };
    let key = a == b || (family(a) == family(b) && (a <= 0x12 || b <= 0x12));
    key && (am == bm || am.is_none() || bm.is_none())
}
