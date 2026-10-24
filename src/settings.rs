//! The SettingsForm model and the configuration update rules.
//!
//! The settings process edits a shared Config through this form.
//! The core keeps Dictionary roles, language lists, and staged changes here.

use crate::config::{
    Config, FieldMapping, FieldOverride, Profile, ProfileData, ProfileSettings, ResolvedConfig,
    RoleList, PROFILE_FIELDS,
};
use crate::library::{roles_of, Library, Pending, Role, Roles};
use crate::present::DictInfo;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// This module re-exports these names for config.rs.
pub use crate::config::{
    CAPTURE_H_RANGE, CAPTURE_W_RANGE, MAX_HEIGHT_RANGE, MAX_WIDTH_RANGE, PASSES_RANGE,
    SUMMARY_RANGE,
};

/// The fields that the settings window edits.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsForm {
    /// The effective view for the selected profile. The saved catalog stays in `catalog`.
    pub cfg: ResolvedConfig,
    /// The saved profile catalog that the settings form edits.
    pub catalog: Config,
    /// The stable ID of the profile shown by `cfg`.
    pub profile_id: String,
    /// The catalog as the form first loaded it. Apply uses this to merge changes.
    baseline_catalog: Config,
    /// The current profile and shared settings when the form last loaded them.
    baseline_cfg: ResolvedConfig,
    baseline_profile: ProfileSettings,
    baseline_terms: Vec<DictRow>,
    baseline_pitch: Vec<DictRow>,
    screenshot_resets: std::collections::BTreeSet<String>,
    staged_add_profiles: BTreeMap<PathBuf, String>,
    staged_add_before: BTreeMap<String, StagedDictionaryState>,
    dicts: Vec<DictInfo>,
    baseline_frequency: Vec<DictRow>,
    /// The terms Dictionary list. It contains every Dictionary that the config
    /// names or that the Library holds with that role, in priority order. Each
    /// row has its own checkbox.
    ///
    /// Each role has one list, not one list and an exclusion box. List order
    /// and enabled state are separate. A reorder therefore does not exclude a
    /// Dictionary (ARCHITECTURE.md#dictionary-and-lookup).
    pub terms: Vec<DictRow>,
    /// The frequency Dictionary list. Position is the order that
    /// [`RankingStrategy::Priority`] reads. A checkbox here does not affect the
    /// same Dictionary in another list.
    pub frequency: Vec<DictRow>,
    /// The pitch Dictionary list. This checkbox is the only pitch enable switch.
    pub pitch: Vec<DictRow>,
    /// The language whose terms list this form edits. `cfg.ocr.language` can
    /// change in the window; the rows still belong to this language.
    pub dict_list_language: String,
    pub freq_changed: bool,
    pub staged_adds: Vec<StagedAdd>,
    pub staged_removes: Vec<String>,
    pub library_empty: bool,
    /// Files that no archive reader can read.
    ///
    /// The window lists these files but does not order them.
    pub unreadable: Vec<String>,
    /// `None` means that this window has no answer about the field map, so Apply
    /// leaves the saved map unchanged. Windows fills its rows from a live
    /// AnkiConnect `modelFieldNames` call. That call returns an empty vector when
    /// Anki is unreachable
    /// (`crates/chibipop-windows/src/app.rs:797-806`). A window that has not
    /// learned field names must not wipe a good field map. `Some(vec![])` means
    /// that a user mapped no fields. That is an answer, and the form saves it.
    pub field_map: Option<Vec<FieldMapping>>,
    /// Windows preference answer.
    pub background_on_close: Option<bool>,
    /// The loaded library helps profile switches retain role corrections.
    library: Option<Library>,
    /// Saved targets can change while this form is open.
    /// Only an explicit reset can remove a target from the latest Config.
    pub screenshot_reset_targets: bool,
}

/// One Dictionary row in one role list.
///
/// The name identifies the Dictionary. The flag enables that role.
/// Enabled state belongs to each role, not to the Dictionary. A mixed archive
/// can provide definitions and frequency data, so one checkbox affects only
/// its own role list (ARCHITECTURE.md#dictionary-and-lookup).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictRow {
    pub name: String,
    pub enabled: bool,
}

/// An import staged until Apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedAdd {
    pub source: PathBuf,
    /// The title that the Dictionary provides.
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ListPosition {
    enabled: bool,
    index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct StagedProfileDictionaryState {
    terms: Option<ListPosition>,
    pitch: Option<ListPosition>,
    languages: BTreeMap<String, usize>,
    terms_override: bool,
    pitch_override: bool,
    languages_override: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct StagedDictionaryState {
    profiles: BTreeMap<String, StagedProfileDictionaryState>,
    frequency: Option<ListPosition>,
}
fn role_position(list: &RoleList, name: &str) -> Option<ListPosition> {
    list.enabled.iter().position(|entry| entry == name).map(|index| ListPosition {
        enabled: true,
        index,
    }).or_else(|| list.disabled.iter().position(|entry| entry == name).map(|index| ListPosition {
        enabled: false,
        index,
    }))
}

fn row_position(rows: &[DictRow], name: &str) -> Option<ListPosition> {
    let index = rows.iter().position(|row| row.name == name)?;
    let enabled = rows[index].enabled;
    let role_index = rows[..index].iter().filter(|row| row.enabled == enabled).count();
    Some(ListPosition { enabled, index: role_index })
}

fn staged_dictionary_state(form: &SettingsForm, name: &str) -> StagedDictionaryState {
    let mut state = StagedDictionaryState {
        frequency: row_position(&form.frequency, name),
        ..Default::default()
    };
    for profile in &form.catalog.profiles {
        let Ok(settings) = form.catalog.resolve(&profile.id) else { continue };
        let (mut terms_override, mut pitch_override, mut languages_override) = match &profile.data {
            ProfileData::Full { .. } => (true, true, true),
            ProfileData::Derived { overrides, .. } => (
                overrides.contains_key("dictionaries.terms"),
                overrides.contains_key("dictionaries.pitch"),
                overrides.contains_key("dictionaries.per_language"),
            ),
        };
        for prior in form.staged_add_before.values()
            .filter_map(|state| state.profiles.get(&profile.id))
        {
            terms_override &= prior.terms_override;
            pitch_override &= prior.pitch_override;
            languages_override &= prior.languages_override;
        }
        let mut profile_state = StagedProfileDictionaryState {
            terms: role_position(&settings.dictionaries.terms, name),
            pitch: role_position(&settings.dictionaries.pitch, name),
            languages: settings.dictionaries.per_language.iter()
                .filter_map(|(language, names)| {
                    names.iter().position(|entry| entry == name)
                        .map(|index| (language.clone(), index))
                })
                .collect(),
            terms_override,
            pitch_override,
            languages_override,
        };
        if profile.id == form.profile_id {
            profile_state.pitch = row_position(&form.pitch, name);
            if is_scoped(form) {
                let language = &form.cfg.ocr.language;
                profile_state.languages.remove(language);
                if let Some(prior) = row_position(&form.terms, name).filter(|prior| prior.enabled) {
                    profile_state.languages.insert(language.clone(), prior.index);
                }
            } else {
                profile_state.terms = row_position(&form.terms, name);
            }
        }
        state.profiles.insert(profile.id.clone(), profile_state);
    }
    state
}

fn restore_role_name(list: &mut RoleList, name: &str, prior: Option<ListPosition>) -> bool {
    let Some(prior) = prior else {
        let before = list.enabled.len() + list.disabled.len();
        list.enabled.retain(|entry| entry != name);
        list.disabled.retain(|entry| entry != name);
        return before != list.enabled.len() + list.disabled.len();
    };
    let current = role_position(list, name);
    let Some(current) = current else { return false };
    if current.enabled == prior.enabled {
        return false;
    }
    list.enabled.retain(|entry| entry != name);
    list.disabled.retain(|entry| entry != name);
    let target = if prior.enabled { &mut list.enabled } else { &mut list.disabled };
    target.insert(prior.index.min(target.len()), name.to_string());
    true
}

fn restore_language_name(names: &mut Vec<String>, name: &str, prior: Option<usize>) -> bool {
    let Some(index) = prior else {
        let before = names.len();
        names.retain(|entry| entry != name);
        return names.len() != before;
    };
    if names.iter().any(|entry| entry == name) {
        return false;
    }
    names.insert(index.min(names.len()), name.to_string());
    true
}

fn restore_staged_dictionary(
    cfg: &mut Config,
    name: &str,
    before: &StagedDictionaryState,
    remaining: &[StagedAdd],
) {
    let mut frequency = RoleList {
        enabled: cfg.dictionaries.frequency.clone(),
        disabled: cfg.dictionaries.frequency_disabled.clone(),
    };
    if restore_role_name(&mut frequency, name, before.frequency) {
        cfg.dictionaries.frequency = frequency.enabled;
        cfg.dictionaries.frequency_disabled = frequency.disabled;
    }

    let mut ids: Vec<(bool, String)> = cfg.profiles.iter().map(|profile| {
        (matches!(&profile.data, ProfileData::Derived { .. }), profile.id.clone())
    }).collect();
    ids.sort_by_key(|(derived, _)| *derived);
    for (_, id) in ids {
        let prior = before.profiles.get(&id).cloned().unwrap_or_default();
        let mut settings = match cfg.resolve(&id) {
            Ok(settings) => settings,
            Err(_) => continue,
        };
        let mut changed = restore_role_name(&mut settings.dictionaries.terms, name, prior.terms);
        changed |= restore_role_name(&mut settings.dictionaries.pitch, name, prior.pitch);
        for (language, names) in &mut settings.dictionaries.per_language {
            changed |= restore_language_name(names, name, prior.languages.get(language).copied());
        }
        if changed {
            cfg.update_profile(&id, &settings)
                .expect("a valid catalog must update its profile");
        }
        let Some(profile) = cfg.profiles.iter().find(|profile| profile.id == id) else { continue };
        if !matches!(&profile.data, ProfileData::Derived { .. }) {
            continue;
        }
        if !prior.terms_override && !remaining.iter().any(|add| {
            role_position(&settings.dictionaries.terms, &add.name).is_some()
        })
        {
            cfg.reset_override(&id, "dictionaries.terms")
                .expect("the terms override must be valid");
        }
        if !prior.pitch_override && !remaining.iter().any(|add| {
            role_position(&settings.dictionaries.pitch, &add.name).is_some()
        })
        {
            cfg.reset_override(&id, "dictionaries.pitch")
                .expect("the pitch override must be valid");
        }
        if !prior.languages_override && !remaining.iter().any(|add| {
            settings.dictionaries.per_language.values()
                .any(|names| names.contains(&add.name))
        })
        {
            cfg.reset_override(&id, "dictionaries.per_language")
                .expect("the language override must be valid");
        }
    }
}

impl SettingsForm {
    /// The list of this role.
    pub fn list(&self, role: Role) -> &[DictRow] {
        match role {
            Role::Terms => &self.terms,
            Role::Frequency => &self.frequency,
            Role::Pitch => &self.pitch,
        }
    }

    /// The list of this role, to reorder or to check.
    pub fn list_mut(&mut self, role: Role) -> &mut Vec<DictRow> {
        match role {
            Role::Terms => &mut self.terms,
            Role::Frequency => &mut self.frequency,
            Role::Pitch => &mut self.pitch,
        }
    }

    /// Stages an archive for import.
    ///
    /// The archive goes to the bottom of every list for its roles, enabled. It
    /// does not move a prior row. An import therefore preserves the user's
    /// list order.
    ///
    /// The function returns `None` when the archive is unreadable or already
    /// staged. An unreadable archive has no role list.
    pub fn stage_add(&mut self, source: &Path) -> Option<Roles> {
        if self.staged_adds.iter().any(|add| add.source == source) {
            return None;
        }
        let roles = roles_of(source);
        if roles.is_empty() {
            return None;
        }
        let name = archive_title(source)?;
        self.track_staged_dictionary_edits();
        if !self.staged_add_before.contains_key(&name) {
            let before = staged_dictionary_state(self, &name);
            self.staged_add_before.insert(name.clone(), before);
        }
        // Titles can repeat because split editions use one title.
        for role in roles.iter() {
            let rows = self.list_mut(role);
            if let Some(row) = rows.iter_mut().find(|row| row.name == name) {
                row.enabled = true;
            } else {
                rows.push(DictRow { name: name.clone(), enabled: true });
            }
        }
        if roles.has(Role::Frequency) {
            self.freq_changed = true;
        }
        let source = source.to_path_buf();
        self.staged_add_profiles.insert(source.clone(), self.profile_id.clone());
        self.staged_adds.push(StagedAdd { source, name });
        Some(roles)
    }

    /// Stages a row for removal.
    ///
    /// Cancels a staged import or stages a saved Dictionary for removal.
    pub fn stage_remove(&mut self, name: &str) {
        let was_freq = self.frequency.iter().any(|row| row.name == name);
        if self.is_staged_add(name) {
            self.save_current().expect("the selected profile must save before cancellation");
            self.staged_adds.retain(|add| add.name != name);
            self.staged_add_profiles.retain(|source, _| {
                self.staged_adds.iter().any(|add| add.source == *source)
            });
            if let Some(before) = self.staged_add_before.remove(name) {
                restore_staged_dictionary(&mut self.catalog, name, &before, &self.staged_adds);
            }
            let id = self.profile_id.clone();
            self.load_profile(&id, None).expect("the selected profile must reload after cancellation");
        } else {
            for role in Role::EVERY {
                self.list_mut(role).retain(|row| row.name != name);
            }
            if !self.staged_removes.iter().any(|entry| entry == name) {
                self.staged_removes.push(name.to_string());
            }
        }
        if was_freq {
            self.freq_changed = true;
        }

    }

    /// Returns true when the form has staged changes.
    pub fn has_staged(&self) -> bool {
        !self.staged_adds.is_empty() || !self.staged_removes.is_empty()
    }

    /// Clears staged changes after Apply.
    pub fn clear_staged(&mut self) {
        self.staged_adds.clear();
        self.staged_removes.clear();
        self.staged_add_before.clear();
        self.freq_changed = false;
        self.staged_add_profiles.clear();
    }

    /// Copies the per-language lists that Apply wrote.
    pub fn reseed_per_language(&mut self, written: &BTreeMap<String, Vec<String>>) {
        self.cfg.dictionaries.per_language = written.clone();
    }

    /// Returns true when this row names a staged import.
    pub fn is_staged_add(&self, name: &str) -> bool {
        self.staged_adds.iter().any(|a| a.name == name)
    }
    /// Saves edits to the selected profile and shared settings.
    pub fn save_current(&mut self) -> Result<()> {
        let mut catalog = self.catalog.clone();
        let mut settings = catalog.resolve(&self.profile_id)?;
        let edited = profile_settings_from_form(self, &self.baseline_profile)?;
        merge_profile_edits(
            &self.baseline_profile,
            &edited,
            &mut settings,
            &self.staged_removes,
        )?;
        catalog.update_profile(&self.profile_id, &settings)?;
        merge_shared_form(
            &mut catalog,
            &self.baseline_cfg,
            &self.baseline_frequency,
            self,
        );
        if self.screenshot_reset_targets {
            let profile = catalog.profiles.iter_mut().find(|profile| profile.id == self.profile_id)
                .expect("the selected profile must exist");
            reset_screenshot_targets(profile);
            if !self.screenshot_resets.contains(&self.profile_id) {
                self.screenshot_resets.insert(self.profile_id.clone());
            }
            self.screenshot_reset_targets = false;
        }
        self.track_staged_dictionary_edits();
        self.catalog = catalog;
        self.refresh_baseline()?;
        Ok(())
    }

    pub fn accept_applied(&mut self, applied: AppliedSettings, dicts: &[DictInfo]) -> Result<()> {
        anyhow::ensure!(
            applied.config.profiles.iter().any(|profile| profile.id == applied.profile_id),
            "Profile {:?} no longer exists.", applied.profile_id,
        );
        self.staged_add_profiles.retain(|source, _| {
            self.staged_adds.iter().any(|add| add.source == *source)
        });
        for owner in self.staged_add_profiles.values_mut() {
            if let Some(id) = applied.remapped.get(owner) {
                owner.clone_from(id);
            }
        }
        self.baseline_catalog = applied.config.clone();
        self.catalog = applied.config;
        self.screenshot_resets.clear();
        if self.dicts.as_slice() != dicts {
            self.dicts.clear();
            self.dicts.extend_from_slice(dicts);
        }
        self.load_profile(&applied.profile_id, Some(dicts))
    }

    /// Saves current edits and loads a profile by stable ID.
    pub fn select_profile(&mut self, id: &str, dicts: &[DictInfo]) -> Result<()> {
        self.catalog.resolve(id)?;
        self.save_current()?;
        self.load_profile(id, Some(dicts))
    }

    /// Creates and selects a full default profile or a derived profile.
    pub fn create_profile(
        &mut self,
        name: String,
        parent: Option<&str>,
        dicts: &[DictInfo],
    ) -> Result<String> {
        ensure_profile_name(&name)?;
        self.save_current()?;
        if let Some(parent) = parent {
            let source = self.catalog.profiles.iter().find(|profile| profile.id == parent);
            anyhow::ensure!(
                source.is_some_and(|profile| matches!(&profile.data, ProfileData::Full { .. })),
                "A derived profile must inherit a full profile."
            );
        }
        let id = self.catalog.next_profile_id();
        let data = match parent {
            Some(parent) => ProfileData::Derived { parent: parent.to_string(), overrides: BTreeMap::new() },
            None => ProfileData::Full { settings: Box::new(ProfileSettings::default()) },
        };
        self.catalog.profiles.push(Profile { id: id.clone(), name, data });
        self.load_profile(&id, Some(dicts))?;
        Ok(id)
    }

    /// Creates and selects a full copy of the selected effective profile.
    pub fn duplicate_profile(&mut self, name: String, dicts: &[DictInfo]) -> Result<String> {
        ensure_profile_name(&name)?;
        self.save_current()?;
        let settings = self.catalog.resolve(&self.profile_id)?;
        let id = self.catalog.next_profile_id();
        self.catalog.profiles.push(Profile {
            id: id.clone(),
            name,
            data: ProfileData::Full { settings: Box::new(settings) },
        });
        self.load_profile(&id, Some(dicts))?;
        Ok(id)
    }

    /// Renames the selected profile without changing its stable ID.
    pub fn rename_profile(&mut self, name: String) -> Result<()> {
        ensure_profile_name(&name)?;
        let profile = self.catalog.profiles.iter_mut()
            .find(|profile| profile.id == self.profile_id)
            .with_context(|| format!("Profile {:?} does not exist.", self.profile_id))?;
        profile.name = name;
        Ok(())
    }

    /// Removes a profile only when no saved reference names it.
    pub fn delete_profile(&mut self, id: &str, dicts: &[DictInfo]) -> Result<()> {
        self.catalog.resolve(id)?;
        self.save_current()?;
        self.catalog.remove_profile(id)?;
        if self.profile_id == id {
            let default = self.catalog.default_profile.clone();
            self.load_profile(&default, Some(dicts))?;
        }
        Ok(())
    }

    /// Changes only the catalog's default profile.
    pub fn set_default_profile(&mut self, id: &str) -> Result<()> {
        self.catalog.resolve(id)?;
        self.catalog.default_profile = id.to_string();
        Ok(())
    }

    /// Removes one inherited value from the selected derived profile.
    pub fn reset_profile_override(&mut self, path: &str, dicts: &[DictInfo]) -> Result<()> {
        self.save_current()?;
        self.catalog.reset_override(&self.profile_id, path)?;
        for state in self.staged_add_before.values_mut() {
            if let Some(prior) = state.profiles.get_mut(&self.profile_id) {
                match path {
                    "dictionaries.terms" => prior.terms_override = false,
                    "dictionaries.pitch" => prior.pitch_override = false,
                    "dictionaries.per_language" => prior.languages_override = false,
                    _ => {}
                }
            }
        }
        let id = self.profile_id.clone();
        self.load_profile(&id, Some(dicts))
    }

    fn track_staged_dictionary_edits(&mut self) {
        if self.staged_add_before.is_empty() {
            return;
        }
        let same_names = |before: &[String], after: &[String]| {
            before.iter().filter(|name| !self.is_staged_add(name))
                .eq(after.iter().filter(|name| !self.is_staged_add(name)))
        };
        let same_rows = |before: &[DictRow], after: &[DictRow]| {
            [true, false].into_iter().all(|enabled| {
                let unchanged = |row: &&DictRow| row.enabled == enabled && !self.is_staged_add(&row.name);
                before.iter().filter(unchanged).eq(after.iter().filter(unchanged))
            })
        };
        let before = &self.baseline_cfg.dictionaries;
        let after = &self.cfg.dictionaries;
        let terms_changed = !same_rows(&self.baseline_terms, &self.terms);
        let scoped = is_scoped(self);
        let terms_override = (!scoped && terms_changed)
            || !same_names(&before.terms, &after.terms)
            || !same_names(&before.terms_disabled, &after.terms_disabled);
        let pitch_override = !same_rows(&self.baseline_pitch, &self.pitch)
            || !same_names(&before.pitch, &after.pitch)
            || !same_names(&before.pitch_disabled, &after.pitch_disabled);
        let languages_override = (scoped && terms_changed)
            || before.per_language.len() != after.per_language.len()
            || before.per_language.iter().any(|(language, names)| {
                after.per_language.get(language).is_none_or(|current| !same_names(names, current))
            });
        if !terms_override && !pitch_override && !languages_override {
            return;
        }
        for state in self.staged_add_before.values_mut() {
            if let Some(prior) = state.profiles.get_mut(&self.profile_id) {
                prior.terms_override |= terms_override;
                prior.pitch_override |= pitch_override;
                prior.languages_override |= languages_override;
            } else {
                state.profiles.insert(self.profile_id.clone(), StagedProfileDictionaryState {
                    terms_override,
                    pitch_override,
                    languages_override,
                    ..Default::default()
                });
            }
        }
    }

    fn refresh_baseline(&mut self) -> Result<()> {
        self.cfg = self.catalog.resolved(Some(&self.profile_id))?;
        self.baseline_cfg = self.cfg.clone();
        self.baseline_profile = self.catalog.resolve(&self.profile_id)?;
        self.baseline_frequency = self.frequency.clone();
        self.baseline_terms = self.terms.clone();
        self.baseline_pitch = self.pitch.clone();
        Ok(())
    }

    fn load_profile(&mut self, id: &str, dicts: Option<&[DictInfo]>) -> Result<()> {
        let cfg = self.catalog.resolved(Some(id))?;
        let settings = self.catalog.resolve(id)?;
        let (mut terms, frequency, pitch) = rows_for_profile(&cfg, dicts.unwrap_or(&self.dicts));
        if let Some(library) = &self.library {
            apply_library_rows(&mut terms, Role::Terms, library);
            let mut frequency = frequency;
            apply_library_rows(&mut frequency, Role::Frequency, library);
            let mut pitch = pitch;
            apply_library_rows(&mut pitch, Role::Pitch, library);
            self.frequency = frequency;
            self.pitch = pitch;
        } else {
            self.frequency = frequency;
            self.pitch = pitch;
        }
        for add in &self.staged_adds {
            let roles = roles_of(&add.source);
            let enabled = self.staged_add_profiles.get(&add.source)
                .is_none_or(|owner| owner == id);
            for role in roles.iter() {
                let rows = match role {
                    Role::Terms => &mut terms,
                    Role::Frequency => &mut self.frequency,
                    Role::Pitch => &mut self.pitch,
                };
                if !rows.iter().any(|row| row.name == add.name) {
                    let enabled = enabled && (role != Role::Terms
                        || !cfg.dictionaries.per_language.contains_key(&cfg.ocr.language));
                    rows.push(DictRow { name: add.name.clone(), enabled });
                }
            }
        }
        self.cfg = cfg;
        self.profile_id = id.to_string();
        self.terms = terms;
        for file in &self.unreadable {
            if !self.terms.iter().any(|row| row.name == *file) {
                self.terms.push(DictRow { name: file.clone(), enabled: false });
            }
        }
        for rows in [&mut self.terms, &mut self.frequency, &mut self.pitch] {
            rows.retain(|row| !self.staged_removes.contains(&row.name));
        }
        self.dict_list_language = self.cfg.ocr.language.clone();
        self.field_map = Some(self.cfg.anki.field_map.clone());
        self.baseline_cfg = self.cfg.clone();
        self.baseline_profile = settings;
        self.baseline_frequency = self.frequency.clone();
        self.baseline_terms = self.terms.clone();
        self.baseline_pitch = self.pitch.clone();
        self.screenshot_reset_targets = false;
        Ok(())
    }
}

/// Returns the title that the Dictionary provides.
///
/// A file name does not replace this title.
pub fn archive_title(source: &Path) -> Option<String> {
    let index = crate::dict::archive::read_index(source).ok()?;
    let title = index.get("title").and_then(|v| v.as_str()).filter(|t| !t.is_empty());
    match title {
        Some(t) => Some(t.to_string()),
        None => source.file_stem().map(|s| s.to_string_lossy().into_owned()),
    }
}

/// The file name that the list displays for an archive.
pub fn shown_name(source: &Path) -> Option<String> {
    source.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// Returns whether this window's terms list belongs to its OCR language.
///
/// The code rewrites a per-language list only when the visible rows belong to
/// that language and the language already has a list. Without this check,
/// Apply can assign one language's arrangement to another language's tag.
pub fn is_scoped(form: &SettingsForm) -> bool {
    form.dict_list_language == form.cfg.ocr.language
        && form.cfg.dictionaries.per_language.contains_key(&form.cfg.ocr.language)
}

/// Merges installed Dictionary roles into the form's lists.
pub fn with_library(mut form: SettingsForm, lib: &Library) -> SettingsForm {
    apply_library(&mut form, lib);
    form.library = Some(lib.clone());
    form.baseline_terms = form.terms.clone();
    form.baseline_pitch = form.pitch.clone();
    form
}

/// Returns files for staged removals.
pub fn removed_files(form: &SettingsForm, lib: &Library) -> Vec<String> {
    form.staged_removes
        .iter()
        .filter_map(|name| lib.entries.iter().find(|e| &e.name == name || &e.file == name))
        .map(|e| e.file.clone())
        .collect()
}

/// Counts the term archives that Apply would leave.
///
/// The code reads this count. It does not use a form list for the count.
pub fn terms_after_apply(form: &SettingsForm, lib: &Library) -> usize {
    let gone = removed_files(form, lib);
    let kept = lib
        .entries
        .iter()
        .filter(|e| e.roles.has(Role::Terms) && !gone.contains(&e.file))
        .count();
    kept + form
        .staged_adds
        .iter()
        .filter(|a| roles_of(&a.source).has(Role::Terms))
        .count()
}

/// Applies the staged changes to the Library.
///
/// The function checks the term archive count before it moves files.
/// A failed change restores the Library before commit.
pub fn stage_into_library(form: &SettingsForm, dir: &Path) -> Result<Pending> {
    let mut lib = Library::load(dir).with_context(|| format!("reading {}", dir.display()))?;
    if terms_after_apply(form, &lib) == 0 {
        anyhow::bail!("that would leave chibipop with no dictionary");
    }
    let gone = removed_files(form, &lib);
    let mut pending = Pending::new(dir, &lib);
    match mutate(&mut lib, &mut pending, dir, form, &gone) {
        Ok(()) => Ok(pending),
        Err(e) => {
            if let Err(back) = pending.rollback() {
                eprintln!("chibipop: putting the library back failed: {back:#}");
            }
            Err(e)
        }
    }
}

/// Imports staged archives, quarantines removed files, and saves the Library.
fn mutate(
    lib: &mut Library,
    pending: &mut Pending,
    dir: &Path,
    form: &SettingsForm,
    gone: &[String],
) -> Result<()> {
    // A source can already be in `dir`.
    for add in &form.staged_adds {
        let entry = lib
            .import(dir, &add.source)
            .with_context(|| format!("importing {}", add.source.display()))?;
        pending.added(entry.file);
    }
    for file in gone {
        lib.quarantine(dir, file).with_context(|| format!("removing {file}"))?;
        pending.held(file.clone());
    }
    lib.save(dir)
}

/// Builds one role list from config names and Library entries.
///
/// The function starts with the config order. A name that matches no
/// installed Dictionary remains a row. The row stays in the file, so a
/// disconnected drive cannot remove it from the list.
/// `with_library` then corrects the roles.
fn rows_for(cfg: &ResolvedConfig, role: Role) -> Vec<DictRow> {
    cfg.dictionaries
        .listed(role)
        .into_iter()
        .map(|(name, enabled)| DictRow { name, enabled })
        .collect()
}

fn rows_for_profile(cfg: &ResolvedConfig, dicts: &[DictInfo]) -> (Vec<DictRow>, Vec<DictRow>, Vec<DictRow>) {
    let mut terms = rows_for(cfg, Role::Terms);
    let frequency = rows_for(cfg, Role::Frequency);
    let pitch = rows_for(cfg, Role::Pitch);
    for dict in dicts {
        if ![&terms, &frequency, &pitch]
            .iter()
            .any(|rows| rows.iter().any(|row| row.name == dict.name))
        {
            terms.push(DictRow { name: dict.name.clone(), enabled: false });
        }
    }
    if let Some(scope) = cfg.dictionaries.language_scope(&cfg.ocr.language) {
        let mut scoped: Vec<DictRow> =
            scope.into_iter().map(|name| DictRow { name, enabled: true }).collect();
        for row in terms {
            if !scoped.iter().any(|seen| seen.name == row.name) {
                scoped.push(DictRow { name: row.name, enabled: false });
            }
        }
        terms = scoped;
    }
    (terms, frequency, pitch)
}

fn apply_library_rows(rows: &mut Vec<DictRow>, role: Role, lib: &Library) {
    rows.retain(|row| {
        lib.entries.iter().find(|entry| entry.name == row.name)
            .is_none_or(|entry| entry.roles.has(role))
    });
    for entry in lib.entries.iter().filter(|entry| entry.roles.has(role)) {
        if !rows.iter().any(|row| row.name == entry.name) {
            rows.push(DictRow { name: entry.name.clone(), enabled: false });
        }
    }
}

fn apply_library(form: &mut SettingsForm, lib: &Library) {
    apply_library_rows(&mut form.terms, Role::Terms, lib);
    apply_library_rows(&mut form.frequency, Role::Frequency, lib);
    apply_library_rows(&mut form.pitch, Role::Pitch, lib);
    form.unreadable =
        lib.entries.iter().filter(|entry| entry.roles.is_empty()).map(|entry| entry.file.clone()).collect();
    for file in &form.unreadable {
        if !form.terms.iter().any(|row| row.name == *file) {
            form.terms.push(DictRow { name: file.clone(), enabled: false });
        }
    }
    form.library_empty = lib.is_empty();
}

/// Builds a form from the saved catalog's default profile.
pub fn from_config(cfg: &Config, dicts: &[DictInfo]) -> SettingsForm {
    let retained = crate::config::ProfileCatalog::new(cfg, dicts)
        .expect("settings require a valid saved profile catalog");
    let catalog = retained.config.clone();
    let profile_id = catalog.default_profile.clone();
    let resolved = catalog.resolved(Some(&profile_id))
        .expect("the saved default profile must resolve");
    let settings = catalog.resolve(&profile_id)
        .expect("the saved default profile must resolve");
    let (terms, frequency, pitch) = rows_for_profile(&resolved, dicts);
    SettingsForm {
        cfg: resolved.clone(),
        catalog: catalog.clone(),
        profile_id,
        baseline_catalog: catalog,
        staged_add_before: BTreeMap::new(),
        baseline_cfg: resolved.clone(),
        baseline_profile: settings,
        baseline_terms: terms.clone(),
        baseline_pitch: pitch.clone(),
        screenshot_resets: Default::default(),
        baseline_frequency: frequency.clone(),
        terms,
        frequency,
        pitch,
        dict_list_language: resolved.ocr.language.clone(),
        freq_changed: false,
        staged_adds: Vec::new(),
        staged_add_profiles: BTreeMap::new(),
        library_empty: false,
        unreadable: Vec::new(),
        field_map: Some(resolved.anki.field_map.clone()),
        background_on_close: None,
        library: None,
        dicts: dicts.to_vec(),
        staged_removes: Vec::new(),
        screenshot_reset_targets: false,
    }
}


/// Returns enabled names in row order, including an explicit empty list.
pub fn scoped_entry(rows: &[DictRow], unreadable: &[String]) -> Option<Vec<String>> {
    let mut names = Vec::new();
    let mut selected_unreadable = false;
    for row in rows.iter().filter(|row| row.enabled) {
        if unreadable.contains(&row.name) {
            selected_unreadable = true;
        } else {
            names.push(row.name.clone());
        }
    }
    if names.is_empty() && selected_unreadable { None } else { Some(names) }
}

fn role_list(rows: &[DictRow], unreadable: &[String]) -> RoleList {
    let names = |enabled| {
        rows.iter()
            .filter(|row| row.enabled == enabled && !unreadable.contains(&row.name))
            .map(|row| row.name.clone())
            .collect()
    };
    RoleList { enabled: names(true), disabled: names(false) }
}

fn reset_screenshot_targets(profile: &mut Profile) {
    match &mut profile.data {
        ProfileData::Full { settings } => {
            settings.actions.screenshot.fixed_region = None;
            settings.actions.screenshot.fixed_window = None;
        }
        ProfileData::Derived { overrides, .. } => {
            for path in ["actions.screenshot.fixed_region", "actions.screenshot.fixed_window"] {
                if let Some(value) = overrides.get_mut(path) {
                    *value = FieldOverride::Clear;
                } else {
                    overrides.insert(path.to_string(), FieldOverride::Clear);
                }
            }
        }
    }
}

fn profile_settings_from_form(
    form: &SettingsForm,
    baseline: &ProfileSettings,
) -> Result<ProfileSettings> {
    let mut effective = form.cfg.clone();
    effective.clamp_ranges(None);
    let mut settings = ProfileSettings::from_resolved(&effective);
    if form.terms != form.baseline_terms {
        if is_scoped(form) {
            if let Some(names) = scoped_entry(&form.terms, &form.unreadable) {
                settings.dictionaries.per_language.insert(form.cfg.ocr.language.clone(), names);
            }
        } else {
            settings.dictionaries.terms = role_list(&form.terms, &form.unreadable);
        }
    }
    if form.pitch != form.baseline_pitch {
        settings.dictionaries.pitch = role_list(&form.pitch, &form.unreadable);
    }
    if let Some(field_map) = &form.field_map {
        settings.anki.field_map = field_map.clone();
    } else {
        settings.anki.field_map = baseline.anki.field_map.clone();
    }
    for name in &form.staged_removes {
        remove_profile_dictionary(&mut settings, name);
    }
    Ok(settings)
}

fn remove_profile_dictionary(settings: &mut ProfileSettings, name: &str) {
    settings.dictionaries.terms.enabled.retain(|entry| entry != name);
    settings.dictionaries.terms.disabled.retain(|entry| entry != name);
    settings.dictionaries.pitch.enabled.retain(|entry| entry != name);
    settings.dictionaries.pitch.disabled.retain(|entry| entry != name);
    for list in settings.dictionaries.per_language.values_mut() {
        list.retain(|entry| entry != name);
    }
}


fn merge_role_list(
    before: &RoleList,
    edited: &RoleList,
    latest: &RoleList,
    removed: &[String],
) -> RoleList {
    let mut merged = edited.clone();
    for (enabled, source) in [(true, &latest.enabled), (false, &latest.disabled)] {
        for name in source {
            if !before.enabled.iter().chain(&before.disabled).any(|entry| entry == name)
                && !merged.enabled.iter().chain(&merged.disabled).any(|entry| entry == name)
                && !removed.contains(name)
            {
                let target = if enabled { &mut merged.enabled } else { &mut merged.disabled };
                target.push(name.clone());
            }
        }
    }
    merged.enabled.retain(|name| !removed.contains(name));
    merged.disabled.retain(|name| !removed.contains(name));
    merged
}

fn merge_language_lists(
    before: &BTreeMap<String, Vec<String>>,
    edited: &BTreeMap<String, Vec<String>>,
    latest: &BTreeMap<String, Vec<String>>,
    removed: &[String],
) -> BTreeMap<String, Vec<String>> {
    let mut merged = latest.clone();
    for language in before.keys().chain(edited.keys()) {
        if before.get(language) == edited.get(language) {
            continue;
        }
        match edited.get(language) {
            Some(names) => {
                let mut result = names.clone();
                if let Some(current) = latest.get(language) {
                    for name in current {
                        if !before.get(language).is_some_and(|values| values.contains(name))
                            && !result.contains(name)
                            && !removed.contains(name)
                        {
                            result.push(name.clone());
                        }
                    }
                }
                result.retain(|name| !removed.contains(name));
                merged.insert(language.clone(), result);
            }
            None => {
                merged.remove(language);
            }
        }
    }
    for names in merged.values_mut() {
        names.retain(|name| !removed.contains(name));
    }
    merged
}

fn merge_profile_edits(
    before: &ProfileSettings,
    edited: &ProfileSettings,
    latest: &mut ProfileSettings,
    removed: &[String],
) -> Result<()> {
    for path in PROFILE_FIELDS {
        let old = before.field(path)?;
        let new = edited.field(path)?;
        if old == new {
            continue;
        }
        let merged = match *path {
            "dictionaries.terms" => {
                let base = &before.dictionaries.terms;
                let desired = &edited.dictionaries.terms;
                let current = &latest.dictionaries.terms;
                FieldOverride::from_value(merge_role_list(base, desired, current, removed))?
            }
            "dictionaries.pitch" => {
                let base = &before.dictionaries.pitch;
                let desired = &edited.dictionaries.pitch;
                let current = &latest.dictionaries.pitch;
                FieldOverride::from_value(merge_role_list(base, desired, current, removed))?
            }
            "dictionaries.per_language" => FieldOverride::from_value(merge_language_lists(
                &before.dictionaries.per_language,
                &edited.dictionaries.per_language,
                &latest.dictionaries.per_language,
                removed,
            ))?,
            _ => new,
        };
        latest.set_field(path, merged)?;
    }
    Ok(())
}

fn merge_shared_form(
    catalog: &mut Config,
    baseline: &ResolvedConfig,
    baseline_frequency: &[DictRow],
    form: &SettingsForm,
) {
    if form.cfg.trigger.mode != form.baseline_cfg.trigger.mode {
        catalog.live_lookup = form.cfg.trigger.mode == crate::config::TriggerMode::Live;
    }
    if form.cfg.plugins.enabled != form.baseline_cfg.plugins.enabled {
        catalog.plugins.enabled = form.cfg.plugins.enabled.clone();
    }
    if form.cfg.debug.show_scan_region != baseline.debug.show_scan_region {
        catalog.debug.show_scan_region = form.cfg.debug.show_scan_region;
    }
    if form.cfg.debug.show_engine_log != baseline.debug.show_engine_log {
        catalog.debug.show_engine_log = form.cfg.debug.show_engine_log;
    }
    if form.cfg.debug.show_adapter_log != baseline.debug.show_adapter_log {
        catalog.debug.show_adapter_log = form.cfg.debug.show_adapter_log;
    }
    if let Some(value) = form.background_on_close {
        if value != baseline.application.background_on_close {
            catalog.application.background_on_close = value;
        }
    }
    if form.frequency != baseline_frequency {
        let before = role_list(baseline_frequency, &[]);
        let desired = role_list(&form.frequency, &form.unreadable);
        let current = RoleList {
            enabled: catalog.dictionaries.frequency.clone(),
            disabled: catalog.dictionaries.frequency_disabled.clone(),
        };
        let merged = merge_role_list(&before, &desired, &current, &form.staged_removes);
        catalog.dictionaries.frequency = merged.enabled;
        catalog.dictionaries.frequency_disabled = merged.disabled;
    }
    if form.cfg.dictionaries.ranking_strategy != baseline.dictionaries.ranking_strategy {
        catalog.dictionaries.ranking_strategy = form.cfg.dictionaries.ranking_strategy;
    }
}

fn merge_shared_catalog(
    before: &Config,
    edited: &Config,
    latest: &mut Config,
    removed: &[String],
) {
    if before.live_lookup != edited.live_lookup {
        latest.live_lookup = edited.live_lookup;
    }
    if before.application.background_on_close != edited.application.background_on_close {
        latest.application.background_on_close = edited.application.background_on_close;
    }
    if before.plugins.enabled != edited.plugins.enabled {
        latest.plugins.enabled = edited.plugins.enabled.clone();
    }
    if before.debug.show_scan_region != edited.debug.show_scan_region {
        latest.debug.show_scan_region = edited.debug.show_scan_region;
    }
    if before.debug.show_engine_log != edited.debug.show_engine_log {
        latest.debug.show_engine_log = edited.debug.show_engine_log;
    }
    if before.debug.show_adapter_log != edited.debug.show_adapter_log {
        latest.debug.show_adapter_log = edited.debug.show_adapter_log;
    }
    let before_frequency = RoleList {
        enabled: before.dictionaries.frequency.clone(),
        disabled: before.dictionaries.frequency_disabled.clone(),
    };
    let edited_frequency = RoleList {
        enabled: edited.dictionaries.frequency.clone(),
        disabled: edited.dictionaries.frequency_disabled.clone(),
    };
    if before_frequency != edited_frequency {
        let latest_frequency = RoleList {
            enabled: latest.dictionaries.frequency.clone(),
            disabled: latest.dictionaries.frequency_disabled.clone(),
        };
        let merged = merge_role_list(&before_frequency, &edited_frequency, &latest_frequency, removed);
        latest.dictionaries.frequency = merged.enabled;
        latest.dictionaries.frequency_disabled = merged.disabled;
    }
    if before.dictionaries.ranking_strategy != edited.dictionaries.ranking_strategy {
        latest.dictionaries.ranking_strategy = edited.dictionaries.ranking_strategy;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppliedSettings {
    pub config: Config,
    pub profile_id: String,
    remapped: BTreeMap<String, String>,
}

/// Merges this form's profile edits into the latest saved catalog.
pub fn apply_to(form: &SettingsForm, cfg: &Config) -> AppliedSettings {
    let mut draft = form.catalog.clone();
    let mut current = draft.resolve(&form.profile_id)
        .expect("the form's selected profile must exist");
    let edited = profile_settings_from_form(form, &form.baseline_profile)
        .expect("the form's profile settings must serialize");
    merge_profile_edits(
        &form.baseline_profile,
        &edited,
        &mut current,
        &form.staged_removes,
    )
    .expect("profile fields must be valid");
    draft.update_profile(&form.profile_id, &current)
        .expect("the form's selected profile must update");
    merge_shared_form(&mut draft, &form.baseline_cfg, &form.baseline_frequency, form);
    for add in &form.staged_adds {
        let roles = roles_of(&add.source);
        if roles.is_empty() {
            continue;
        }
        let owner = form.staged_add_profiles.get(&add.source)
            .map(String::as_str)
            .unwrap_or(&form.profile_id);
        let owner = draft.profiles.iter().any(|profile| profile.id == owner).then_some(owner);
        let profile_roles = Roles::only(
            &roles.iter().filter(|role| *role != Role::Frequency).collect::<Vec<_>>(),
        );
        if !profile_roles.is_empty() {
            dictionary_added_to_profile(&mut draft, owner, &add.name, profile_roles, true)
                .expect("the staged import profile must be valid");
        }
    }
    for name in &form.staged_removes {
        dictionary_removed_from_profiles(&mut draft, name);
    }
    let mut latest = cfg.clone();
    latest.migrate_dictionary_lists(&form.dicts);
    let remapped = merge_catalog_changes(
        &form.baseline_catalog,
        &draft,
        &mut latest,
        &form.staged_removes,
    );
    let current_reset = form.screenshot_reset_targets.then_some(form.profile_id.as_str())
        .filter(|id| !form.screenshot_resets.contains(*id));
    for id in form.screenshot_resets.iter().map(String::as_str).chain(current_reset) {
        let id = remapped.get(id).map_or(id, String::as_str);
        if let Some(profile) = latest.profiles.iter_mut().find(|profile| profile.id == id) {
            reset_screenshot_targets(profile);
        }
    }
    for name in &form.staged_removes {
        dictionary_removed_from_profiles(&mut latest, name);
    }
    let profile_id = remapped.get(&form.profile_id).unwrap_or(&form.profile_id).clone();
    AppliedSettings { config: latest, profile_id, remapped }
}

fn merge_catalog_changes(
    baseline: &Config,
    draft: &Config,
    latest: &mut Config,
    removed: &[String],
) -> BTreeMap<String, String> {
    latest.next_id = latest.next_id.max(draft.next_id);
    let profile_ids: Vec<String> = draft.profiles.iter()
        .filter(|profile| !baseline.profiles.iter().any(|old| old.id == profile.id))
        .map(|profile| profile.id.clone())
        .collect();
    let mut remapped = BTreeMap::new();
    for id in &profile_ids {
        if latest.profiles.iter().any(|profile| profile.id == *id) {
            remapped.insert(id.clone(), latest.next_profile_id());
        }
    }
    let bind_ids: Vec<String> = draft.binds.iter()
        .filter(|bind| !baseline.binds.iter().any(|old| old.id == bind.id))
        .map(|bind| bind.id.clone())
        .collect();
    let mut remapped_binds = BTreeMap::new();
    for id in bind_ids {
        if latest.binds.iter().any(|bind| bind.id == id) {
            remapped_binds.insert(id, latest.next_bind_id());
        }
    }
    let mut draft = draft.clone();
    rewrite_profile_ids(&mut draft, &remapped);
    for bind in &mut draft.binds {
        if let Some(id) = remapped_binds.get(&bind.id) {
            bind.id = id.clone();
        }
        if let Some(id) = bind.profile.as_mut().and_then(|id| remapped.get(id)) {
            bind.profile = Some(id.clone());
        }
    }
    latest.next_id = latest.next_id.max(draft.next_id);

    merge_shared_catalog(baseline, &draft, latest, removed);
    merge_bind_changes(baseline, &draft, latest);
    for profile in &draft.profiles {
        if !baseline.profiles.iter().any(|old| old.id == profile.id)
            && !latest.profiles.iter().any(|saved| saved.id == profile.id)
        {
            latest.profiles.push(profile.clone());
        }
    }
    if draft.default_profile != baseline.default_profile
        && latest.profiles.iter().any(|profile| profile.id == draft.default_profile)
    {
        latest.default_profile = draft.default_profile.clone();
    }
    let profiles: Vec<&Profile> = baseline.profiles.iter()
        .filter(|profile| matches!(&profile.data, ProfileData::Full { .. }))
        .chain(baseline.profiles.iter()
            .filter(|profile| matches!(&profile.data, ProfileData::Derived { .. })))
        .collect();
    for old in profiles {
        let Some(wanted) = draft.profiles.iter().find(|profile| profile.id == old.id) else {
            continue;
        };
        let Some(target_index) = latest.profiles.iter().position(|profile| profile.id == old.id) else {
            continue;
        };
        if wanted.name != old.name {
            latest.profiles[target_index].name = wanted.name.clone();
        }
        match (&old.data, &wanted.data) {
            (ProfileData::Full { settings: old_settings }, ProfileData::Full { settings: wanted_settings }) => {
                let mut target = latest.resolve(&old.id).expect("saved profile must resolve");
                merge_profile_edits(old_settings, wanted_settings, &mut target, removed)
                    .expect("profile fields must be valid");
                latest.update_profile(&old.id, &target).expect("saved profile must update");
            }
            (ProfileData::Derived { parent: old_parent, overrides: old_overrides },
             ProfileData::Derived { parent: wanted_parent, overrides: wanted_overrides }) => {
                if old_parent != wanted_parent {
                    if let ProfileData::Derived { parent, .. } = &mut latest.profiles[target_index].data {
                        *parent = wanted_parent.clone();
                    }
                }
                let old_settings = baseline.resolve(&old.id).expect("baseline profile must resolve");
                let wanted_settings = draft.resolve(&old.id).expect("draft profile must resolve");
                let latest_settings = latest.resolve(&old.id).expect("saved profile must resolve");
                let mut updates = Vec::new();
                for path in PROFILE_FIELDS {
                    if old_overrides.get(*path) == wanted_overrides.get(*path) {
                        continue;
                    }
                    let value = match wanted_overrides.get(*path) {
                        Some(value)
                            if *path == "dictionaries.per_language"
                                && matches!(value, FieldOverride::Set(_)) =>
                        {
                            if old_overrides.contains_key(*path) {
                                Some(FieldOverride::from_value(merge_language_lists(
                                    &old_settings.dictionaries.per_language,
                                    &wanted_settings.dictionaries.per_language,
                                    &latest_settings.dictionaries.per_language,
                                    removed,
                                ))
                                .expect("language map must serialize"))
                            } else {
                                Some(value.clone())
                            }
                        }
                        Some(value) => Some(value.clone()),
                        None => None,
                    };
                    updates.push((*path, value));
                }
                if let ProfileData::Derived { overrides, .. } = &mut latest.profiles[target_index].data {
                    for (path, value) in updates {
                        if let Some(value) = value {
                            overrides.insert(path.to_string(), value);
                        } else {
                            overrides.remove(path);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let mut deleted: Vec<&str> = baseline.profiles.iter()
        .filter(|old| !draft.profiles.iter().any(|profile| profile.id == old.id))
        .map(|profile| profile.id.as_str())
        .collect();
    loop {
        let before = deleted.len();
        let mut index = 0;
        while index < deleted.len() {
            let id = deleted[index];
            let referenced = latest.default_profile == id
                || latest.binds.iter().any(|bind| bind.profile.as_deref() == Some(id))
                || latest.profiles.iter().any(|profile| {
                    !deleted.contains(&profile.id.as_str())
                        && latest.profile_references(profile).contains(&Some(id))
                });
            if referenced {
                deleted.swap_remove(index);
            } else {
                index += 1;
            }
        }
        if deleted.len() == before { break; }
    }
    latest.profiles.retain(|profile| !deleted.contains(&profile.id.as_str()));
    remapped
}

fn rewrite_profile_ids(config: &mut Config, ids: &BTreeMap<String, String>) {
    for profile in &mut config.profiles {
        if let Some(id) = ids.get(&profile.id) {
            profile.id = id.clone();
        }
        match &mut profile.data {
            ProfileData::Full { settings } => {
                if let Some(id) = settings.nested_profile.as_mut().and_then(|id| ids.get(id)) {
                    settings.nested_profile = Some(id.clone());
                }
            }
            ProfileData::Derived { parent, overrides } => {
                if let Some(id) = ids.get(parent) {
                    *parent = id.clone();
                }
                if let Some(FieldOverride::Set(toml::Value::String(id))) = overrides.get_mut("nested_profile") {
                    if let Some(mapped) = ids.get(id) {
                        *id = mapped.clone();
                    }
                }
            }
        }
    }
    if let Some(id) = ids.get(&config.default_profile) {
        config.default_profile = id.clone();
    }
}

fn merge_bind_changes(baseline: &Config, draft: &Config, latest: &mut Config) {
    for old in &baseline.binds {
        let Some(edited) = draft.binds.iter().find(|bind| bind.id == old.id) else {
            latest.binds.retain(|bind| bind.id != old.id);
            continue;
        };
        let Some(target) = latest.binds.iter_mut().find(|bind| bind.id == old.id) else {
            continue;
        };
        if edited.action != old.action { target.action = edited.action; }
        if edited.windows != old.windows { target.windows = edited.windows.clone(); }
        if edited.linux != old.linux { target.linux = edited.linux.clone(); }
        if edited.mode != old.mode { target.mode = edited.mode; }
        if edited.profile != old.profile { target.profile = edited.profile.clone(); }
        if edited.enabled != old.enabled { target.enabled = edited.enabled; }
    }
    for bind in &draft.binds {
        if !baseline.binds.iter().any(|old| old.id == bind.id)
            && !latest.binds.iter().any(|saved| saved.id == bind.id)
        {
            latest.binds.push(bind.clone());
        }
    }
}

/// Reports capture-size values that Apply changed.
pub fn clamp_notice(form: &SettingsForm, applied: &ResolvedConfig) -> Option<String> {
    let mut parts = Vec::new();
    if form.cfg.ocr.capture_width != applied.ocr.capture_width {
        parts.push(axis_notice("width", form.cfg.ocr.capture_width, applied.ocr.capture_width));
    }
    if form.cfg.ocr.capture_height != applied.ocr.capture_height {
        parts.push(axis_notice("height", form.cfg.ocr.capture_height, applied.ocr.capture_height));
    }
    if parts.is_empty() { None } else { Some(parts.join(" ")) }
}

/// Builds the exact clamp notice sentence.
fn axis_notice(axis: &str, asked: i32, got: i32) -> String {
    let (verb, bound) = if got > asked { ("raised", "minimum") } else { ("lowered", "maximum") };
    format!("Capture {axis} {verb} to the {got}px {bound}.")
}

/// Returns names that match no installed Dictionary in any saved profile.
pub fn stale_order_entries(cfg: &Config, dicts: &[DictInfo]) -> Vec<String> {
    let mut migrated = cfg.clone();
    migrated.migrate_dictionary_lists(dicts);
    let mut stale = Vec::new();
    for profile in &migrated.profiles {
        let Ok(resolved) = migrated.resolved(Some(&profile.id)) else { continue };
        for role in Role::EVERY {
            let (enabled, disabled) = resolved.dictionaries.lists(role);
            for name in enabled.iter().chain(disabled) {
                if !dicts.iter().any(|dict| dict.name == *name) && !stale.contains(name) {
                    stale.push(name.clone());
                }
            }
        }
    }
    stale
}

/// Removes one Dictionary from every saved profile and shared frequency list.
pub fn dictionary_removed_from_profiles(cfg: &mut Config, name: &str) {
    cfg.dictionaries.frequency.retain(|entry| entry != name);
    cfg.dictionaries.frequency_disabled.retain(|entry| entry != name);
    let ids: Vec<String> = cfg.profiles.iter().map(|profile| profile.id.clone()).collect();
    for id in ids {
        let Some(profile) = cfg.profiles.iter().find(|profile| profile.id == id) else {
            continue;
        };
        let full = matches!(&profile.data, ProfileData::Full { .. });
        let (terms_explicit, pitch_explicit, languages_explicit) = match &profile.data {
            ProfileData::Full { .. } => (true, true, true),
            ProfileData::Derived { overrides, .. } => (
                overrides.contains_key("dictionaries.terms"),
                overrides.contains_key("dictionaries.pitch"),
                overrides.contains_key("dictionaries.per_language"),
            ),
        };
        if !full && !terms_explicit && !pitch_explicit && !languages_explicit {
            continue;
        }
        let Ok(mut settings) = cfg.resolve(&id) else { continue };
        let mut changed = false;
        if terms_explicit {
            let before = settings.dictionaries.terms.clone();
            remove_role_name(&mut settings.dictionaries.terms, name);
            changed |= before != settings.dictionaries.terms;
        }
        if pitch_explicit {
            let before = settings.dictionaries.pitch.clone();
            remove_role_name(&mut settings.dictionaries.pitch, name);
            changed |= before != settings.dictionaries.pitch;
        }
        if languages_explicit {
            for list in settings.dictionaries.per_language.values_mut() {
                let before = list.len();
                list.retain(|entry| entry != name);
                changed |= list.len() != before;
            }
        }
        if changed {
            cfg.update_profile(&id, &settings)
                .expect("a valid catalog must update its profile");
        }
    }
}

/// Adds roles to the selected profile and disables them in other explicit lists.
/// Set `preserve_existing` to keep the selected profile's saved row state.
fn dictionary_added_to_profile(
    cfg: &mut Config,
    profile_id: Option<&str>,
    name: &str,
    roles: Roles,
    preserve_existing: bool,
) -> Result<()> {
    if name.trim().is_empty() {
        return Ok(());
    }
    if let Some(id) = profile_id {
        anyhow::ensure!(
            cfg.profiles.iter().any(|profile| profile.id == id),
            "Profile {id:?} does not exist."
        );
    }
    if roles.has(Role::Frequency) {
        if !cfg.dictionaries.frequency.iter().any(|entry| entry == name) {
            cfg.dictionaries.frequency.push(name.to_string());
        }
        cfg.dictionaries.frequency_disabled.retain(|entry| entry != name);
    }
    let ids: Vec<String> = cfg.profiles.iter().map(|profile| profile.id.clone()).collect();
    for id in ids {
        let profile = cfg.profiles.iter().find(|profile| profile.id == id)
            .with_context(|| format!("Profile {id:?} does not exist."))?;
        let is_selected = profile_id == Some(id.as_str());
        let full = matches!(&profile.data, ProfileData::Full { .. });
        let (terms_explicit, pitch_explicit) = match &profile.data {
            ProfileData::Full { .. } => (true, true),
            ProfileData::Derived { overrides, .. } => (
                overrides.contains_key("dictionaries.terms"),
                overrides.contains_key("dictionaries.pitch"),
            ),
        };
        let mut settings = cfg.resolve(&id)?;
        let mut changed = false;
        for (role, explicit) in [(Role::Terms, terms_explicit), (Role::Pitch, pitch_explicit)] {
            if !roles.has(role) || (!is_selected && !full && !explicit) {
                continue;
            }
            let enabled = !preserve_existing || role != Role::Terms
                || settings.dictionaries.per_language.get(&settings.ocr.language)
                    .is_none_or(|names| names.iter().any(|entry| entry == name));
            let list = match role {
                Role::Terms => &mut settings.dictionaries.terms,
                Role::Pitch => &mut settings.dictionaries.pitch,
                Role::Frequency => continue,
            };
            if is_selected {
                let already_listed = list.enabled.iter().chain(&list.disabled).any(|entry| entry == name);
                if !preserve_existing || !already_listed {
                    let (selected, unselected) = if enabled {
                        (&mut list.enabled, &mut list.disabled)
                    } else {
                        (&mut list.disabled, &mut list.enabled)
                    };
                    let before = unselected.len();
                    unselected.retain(|entry| entry != name);
                    if !selected.iter().any(|entry| entry == name) {
                        selected.push(name.to_string());
                        changed = true;
                    }
                    changed |= unselected.len() != before;
                }
            } else if !list.enabled.iter().chain(&list.disabled).any(|entry| entry == name) {
                list.disabled.push(name.to_string());
                changed = true;
            }
        }
        if is_selected && roles.has(Role::Terms) && !preserve_existing {
            if let Some(list) = settings.dictionaries.per_language.get_mut(&settings.ocr.language) {
                if !list.iter().any(|entry| entry == name) {
                    list.push(name.to_string());
                    changed = true;
                }
            }
        }
        if changed {
            cfg.update_profile(&id, &settings)?;
        }
    }
    Ok(())
}

fn remove_role_name(list: &mut RoleList, name: &str) {
    list.enabled.retain(|entry| entry != name);
    list.disabled.retain(|entry| entry != name);
}

fn ensure_profile_name(name: &str) -> Result<()> {
    anyhow::ensure!(!name.trim().is_empty(), "A profile name cannot be empty.");
    Ok(())
}

/// The work that a Dictionary change needs beyond the file save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictionaryWork {
    /// Save the file and send `reload` only.
    None,
    /// Recompute every Frequency rank in place first.
    Reindex,
}

/// Chooses whether a Dictionary change needs a Reindex.
///
/// Exactly three inputs determine `term.freq`: enabled frequency Dictionaries,
/// their order, and the Ranking strategy. A change to one input makes stored
/// ranks stale. Terms and pitch lists do not affect `term.freq`, so they need
/// no Reindex (ARCHITECTURE.md#dictionary-and-lookup).
///
/// This rule stays here instead of in a settings window. A Reindex is not a
/// rebuild because this code does not read an archive.
pub fn dictionary_work(before: &Config, after: &Config) -> DictionaryWork {
    let changed = before.dictionaries.frequency != after.dictionaries.frequency
        || before.dictionaries.frequency_disabled != after.dictionaries.frequency_disabled
        || before.dictionaries.ranking_strategy != after.dictionaries.ranking_strategy;
    if changed { DictionaryWork::Reindex } else { DictionaryWork::None }
}

/// The differences between the Library and the source list.
#[derive(Debug, Default, PartialEq, Eq)]
struct Drift {
    /// Archives in the Library that the source list does not name.
    unbuilt: Vec<String>,
    /// Archives in the source list that the Library no longer has.
    orphaned: Vec<String>,
}

/// Compares the Library with `source_hashes`.
///
/// The function parses the source list and compares archive names. It does
/// not compare record byte values.
fn drift(sources: Option<&str>, lib: &Library) -> Drift {
    let Some(raw) = sources else { return Drift::default() };
    let Ok(listed) = serde_json::from_str::<Vec<serde_json::Value>>(raw) else {
        return Drift::default();
    };
    let recorded: Vec<String> =
        listed.iter().filter_map(|rec| rec["name"].as_str()).map(str::to_string).collect();
    // write_meta skips unreadables.
    let built: Vec<String> = lib
        .entries
        .iter()
        .filter(|e| !e.roles.is_empty())
        .map(|e| e.file.clone())
        .collect();
    Drift { unbuilt: only_in(&built, &recorded), orphaned: only_in(&recorded, &built) }
}

/// Returns names that occur in one list but not the other.
fn only_in(these: &[String], those: &[String]) -> Vec<String> {
    these.iter().filter(|name| !those.contains(name)).cloned().collect()
}

/// Builds a rebuild notice when the Library and database differ.
///
/// The function returns no notice when the Library has no term archive.
/// CRLF separates the notice from the command because the destination box is an EDIT.
pub fn drift_notice(
    sources: Option<&str>,
    lib: &Library,
    dir: &Path,
    db: &Path,
) -> Option<String> {
    // build-dict needs a term archive.
    if !lib.entries.iter().any(|e| e.roles.has(Role::Terms)) {
        return None;
    }
    let found = drift(sources, lib);
    let mut parts = Vec::new();
    if !found.unbuilt.is_empty() {
        parts.push(format!(
            "In your library but not in the database: {}.",
            found.unbuilt.join(", ")
        ));
    }
    if !found.orphaned.is_empty() {
        parts.push(format!(
            "In the database but no longer in your library: {}.",
            found.orphaned.join(", ")
        ));
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!(
        "Your library and your dictionary database no longer match. {} Nothing is broken \
         and lookups still work, but a dictionary the database does not have cannot answer. \
         To make them match, quit chibipop, then run this in a terminal, and start chibipop \
         again when it finishes:\r\nchibipop build-dict --library \"{}\" --out \"{}\"",
        parts.join(" "),
        dir.display(),
        db.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::
        TriggerMode
    ;
    

    fn dicts() -> Vec<DictInfo> {
        vec![
            DictInfo { dict_id: 1, name: "Jitendex.org [2026-07-09]".into() },
            DictInfo { dict_id: 2, name: "大辞林　第四版".into() },
        ]
    }

    fn saved_from_resolved(config: &ResolvedConfig) -> Config {
        let mut saved = Config {
            live_lookup: config.trigger.mode == TriggerMode::Live,
            ..Default::default()
        };
        saved.dictionaries.frequency = config.dictionaries.frequency.clone();
        saved.dictionaries.frequency_disabled = config.dictionaries.frequency_disabled.clone();
        saved.dictionaries.ranking_strategy = config.dictionaries.ranking_strategy;
        saved.application = config.application.clone();
        saved.plugins = config.plugins.clone();
        saved.debug = config.debug.clone();
        saved.update_profile("default", &ProfileSettings::from_resolved(config)).unwrap();
        if config.trigger.mode != TriggerMode::Live {
            if let Some(bind) = saved.binds.iter_mut()
                .find(|bind| bind.action == crate::config::BindAction::Lookup)
            {
                bind.mode = if config.trigger.mode == TriggerMode::HoldShift {
                    TriggerMode::HoldKey
                } else {
                    config.trigger.mode
                };
                bind.enabled = true;
            }
        }
        saved
    }

    fn from_resolved(config: &ResolvedConfig, dicts: &[DictInfo]) -> SettingsForm {
        super::from_config(&saved_from_resolved(config), dicts)
    }

    fn apply_resolved(form: &SettingsForm, config: &ResolvedConfig) -> ResolvedConfig {
        super::apply_to(form, &saved_from_resolved(config)).config
            .resolved(Some(&form.profile_id))
            .unwrap()
    }

    fn insert_full(config: &mut Config, id: &str, name: &str, settings: ProfileSettings) {
        config.profiles.push(Profile {
            id: id.to_string(),
            name: name.to_string(),
            data: ProfileData::Full { settings: Box::new(settings) },
        });
    }

    #[test]
    fn profile_switches_save_each_edit_and_form_changes_win() {
        let mut saved = Config::default();
        let mut other_settings = saved.resolve("default").unwrap();
        other_settings.popup.theme = "sepia".into();
        insert_full(&mut saved, "other", "Other", other_settings);

        let mut form = super::from_config(&saved, &[]);
        form.cfg.popup.theme = "light".into();
        form.select_profile("other", &[]).unwrap();
        assert_eq!("sepia", form.cfg.popup.theme);
        form.cfg.popup.theme = "blue".into();
        form.select_profile("default", &[]).unwrap();
        form.set_default_profile("other").unwrap();

        let mut latest = saved.clone();
        let mut other_settings = latest.resolve("other").unwrap();
        other_settings.popup.theme = "latest".into();
        latest.update_profile("other", &other_settings).unwrap();
        let applied = super::apply_to(&form, &latest).config;
        assert_eq!("other", applied.default_profile);
        assert_eq!("light", applied.resolved(Some("default")).unwrap().popup.theme);
        assert_eq!("blue", applied.resolved(Some("other")).unwrap().popup.theme);
    }

    #[test]
    fn a_mixed_staged_import_keeps_checkbox_state_and_role_order() {
        let mut saved = Config::default();
        let mut settings = saved.resolve("default").unwrap();
        settings.dictionaries.terms.enabled = vec!["Terms A".into(), "Terms B".into()];
        settings.dictionaries.terms.disabled = vec!["Terms off".into()];
        settings.dictionaries.pitch.enabled = vec!["Pitch A".into(), "Pitch B".into()];
        settings.dictionaries.pitch.disabled = vec!["Pitch off".into()];
        saved.update_profile("default", &settings).unwrap();
        let mut form = super::from_config(&saved, &[]);
        form.stage_add(&fixture("both.zip")).unwrap();
        form.pitch.iter_mut().find(|row| row.name == "FixtureBoth").unwrap().enabled = false;

        let applied = super::apply_to(&form, &saved).config;
        let profile = applied.resolve("default").unwrap();

        assert_eq!(
            vec!["Terms A".to_string(), "Terms B".to_string(), "FixtureBoth".to_string()],
            profile.dictionaries.terms.enabled,
        );
        assert_eq!(vec!["Terms off".to_string()], profile.dictionaries.terms.disabled);
        assert_eq!(vec!["Pitch A".to_string(), "Pitch B".to_string()], profile.dictionaries.pitch.enabled);
        assert_eq!(
            vec!["Pitch off".to_string(), "FixtureBoth".to_string()],
            profile.dictionaries.pitch.disabled,
        );
    }







    #[test]
    fn apply_to_writes_the_checked_rows_into_their_language() {
        let mut cfg = ResolvedConfig::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries
            .per_language
            .insert("ja".to_string(), vec!["大辞林　第四版".to_string()]);
        let mut form = from_resolved(&cfg, &dicts());
        form.terms = vec![
            DictRow { name: "大辞林　第四版".to_string(), enabled: true },
            DictRow { name: "Jitendex.org [2026-07-09]".to_string(), enabled: false },
        ];
        let out = apply_resolved(&form, &cfg);
        assert_eq!(vec!["大辞林　第四版".to_string()], out.dictionaries.per_language["ja"]);
    }

    // ---- library changes ----

    struct TempDirGuard(PathBuf);

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yomitan").join(name)
    }

    fn stocked(test_name: &str) -> (PathBuf, TempDirGuard) {
        let dir = std::env::temp_dir()
            .join("chibipop_stage_test")
            .join(format!("t_{}_{test_name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(fixture("terms.zip"), dir.join("terms.zip")).unwrap();
        std::fs::copy(fixture("freq.zip"), dir.join("freq.zip")).unwrap();
        (dir.clone(), TempDirGuard(dir))
    }

    /// Keep assertions about archives and the manifest independent of disposable
    /// caches. A cache file does not represent a staged library change.
    fn files_in(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".zip") || name == "library.json")
            .collect();
        out.sort();
        out
    }

    fn form_for(dir: &Path) -> SettingsForm {
        let form = from_resolved(&ResolvedConfig::default(), &[]);
        match Library::load(dir) {
            Ok(lib) => with_library(form, &lib),
            Err(_) => form,
        }
    }

    /// The staged copy gets a free file name, but the manifest treats it as the
    /// same Dictionary.
    ///
    /// `stocked` already holds `terms.zip`, so the test copies it to
    /// `terms (2).zip`. The files have identical bytes. `Library::load` treats
    /// both names as one Dictionary. The duplicate file remains where the copy
    /// placed it.
    #[test]
    fn an_add_lands_a_copy_in_the_library_and_in_the_manifest() {
        let (dir, _guard) = stocked("add");
        let mut form = form_for(&dir);
        assert!(form.stage_add(&fixture("terms.zip")).is_some());

        stage_into_library(&form, &dir).unwrap().commit().unwrap();

        assert_eq!(
            vec!["freq.zip", "library.json", "terms (2).zip", "terms.zip"],
            files_in(&dir)
        );
        let lib = Library::load(&dir).unwrap();
        assert_eq!(2, lib.entries.len(), "the copy is not a second dictionary");
        assert_eq!(
            vec![dir.join("freq.zip"), dir.join("terms.zip")],
            lib.dict_paths(&dir),
            "the byte-identical copy is not a second dictionary, and the frequency \
             archive is one in its own right",
        );
    }

    /// The function checks all term archives that would remain before it deletes anything.
    #[test]
    fn removing_every_dictionary_is_refused_and_changes_nothing() {
        let (dir, _guard) = stocked("refuse");
        let before = files_in(&dir);
        let mut form = form_for(&dir);
        form.stage_remove("FixtureTerms");
        form.stage_remove("FixtureFreq");

        let refused = stage_into_library(&form, &dir).unwrap_err();

        assert!(format!("{refused:#}").contains("no dictionary"), "{refused:#}");
        assert_eq!(before, files_in(&dir), "nothing was deleted");
    }

    // ---- incremental ----

    // ---- what a change costs ----


    // ---- drift ----

}

