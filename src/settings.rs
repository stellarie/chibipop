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
    /// The row leaves all role lists. One archive is one Dictionary, so removal
    /// removes it from all three roles.
    pub fn stage_remove(&mut self, name: &str) {
        let was_freq = self.frequency.iter().any(|row| row.name == name);
        for role in Role::EVERY {
            self.list_mut(role).retain(|row| row.name != name);
        }
        let staged_sources: Vec<PathBuf> = self.staged_adds.iter()
            .filter(|add| add.name == name)
            .map(|add| add.source.clone())
            .collect();
        let staged = self.staged_adds.len();
        self.staged_adds.retain(|add| add.name != name);
        for source in staged_sources {
            self.staged_add_profiles.remove(&source);
        }
        // The staged import never reached the Library.
        if self.staged_adds.len() == staged && !self.staged_removes.iter().any(|entry| entry == name) {
            self.staged_removes.push(name.to_string());
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
        self.load_profile(&applied.profile_id, dicts)
    }

    /// Saves current edits and loads a profile by stable ID.
    pub fn select_profile(&mut self, id: &str, dicts: &[DictInfo]) -> Result<()> {
        self.catalog.resolve(id)?;
        self.save_current()?;
        self.load_profile(id, dicts)
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
        self.load_profile(&id, dicts)?;
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
        self.load_profile(&id, dicts)?;
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
            self.load_profile(&default, dicts)?;
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
        let id = self.profile_id.clone();
        self.load_profile(&id, dicts)
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

    fn load_profile(&mut self, id: &str, dicts: &[DictInfo]) -> Result<()> {
        let cfg = self.catalog.resolved(Some(id))?;
        let settings = self.catalog.resolve(id)?;
        let (mut terms, frequency, pitch) = rows_for_profile(&cfg, dicts);
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
                if let Some(row) = rows.iter_mut().find(|row| row.name == add.name) {
                    if enabled {
                        row.enabled = true;
                    }
                } else {
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
        dictionary_added_to_profile(&mut draft, owner, &add.name, roles)
            .expect("the staged import profile must be valid");
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
                            if matches!(path, &"dictionaries.terms" | &"dictionaries.pitch")
                                && matches!(value, FieldOverride::Set(_)) =>
                        {
                            let (base, edited, current) = if *path == "dictionaries.terms" {
                                (
                                    &old_settings.dictionaries.terms,
                                    &wanted_settings.dictionaries.terms,
                                    &latest_settings.dictionaries.terms,
                                )
                            } else {
                                (
                                    &old_settings.dictionaries.pitch,
                                    &wanted_settings.dictionaries.pitch,
                                    &latest_settings.dictionaries.pitch,
                                )
                            };
                            Some(FieldOverride::from_value(merge_role_list(base, edited, current, removed))
                                .expect("role list must serialize"))
                        }
                        Some(value)
                            if *path == "dictionaries.per_language"
                                && matches!(value, FieldOverride::Set(_)) =>
                        {
                            Some(FieldOverride::from_value(merge_language_lists(
                                &old_settings.dictionaries.per_language,
                                &wanted_settings.dictionaries.per_language,
                                &latest_settings.dictionaries.per_language,
                                removed,
                            ))
                            .expect("language map must serialize"))
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

/// Adds a Dictionary to the selected profile and keeps other explicit lists disabled.
fn dictionary_added_to_profile(
    cfg: &mut Config,
    profile_id: Option<&str>,
    name: &str,
    roles: Roles,
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
            let list = match role {
                Role::Terms => &mut settings.dictionaries.terms,
                Role::Pitch => &mut settings.dictionaries.pitch,
                Role::Frequency => continue,
            };
            if is_selected {
                let before = list.disabled.len();
                list.disabled.retain(|entry| entry != name);
                if !list.enabled.iter().any(|entry| entry == name) {
                    list.enabled.push(name.to_string());
                    changed = true;
                }
                changed |= list.disabled.len() != before;
            } else if !list.enabled.iter().chain(&list.disabled).any(|entry| entry == name) {
                list.disabled.push(name.to_string());
                changed = true;
            }
        }
        if is_selected && roles.has(Role::Terms) {
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
    use crate::dict::frequency::RankingStrategy;

    fn dicts() -> Vec<DictInfo> {
        vec![
            DictInfo { dict_id: 1, name: "Jitendex.org [2026-07-09]".into() },
            DictInfo { dict_id: 2, name: "大辞林　第四版".into() },
        ]
    }

    /// A config already written in the role shape.
    fn cfg_with(terms: &[&str]) -> ResolvedConfig {
        let mut c = ResolvedConfig::default();
        c.dictionaries.terms = terms.iter().map(|s| (*s).to_string()).collect();
        c
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

    fn stale_order_entries_resolved(config: &ResolvedConfig, dicts: &[DictInfo]) -> Vec<String> {
        super::stale_order_entries(&saved_from_resolved(config), dicts)
    }

    fn dictionary_added(config: &mut ResolvedConfig, name: &str, roles: Roles) {
        let mut saved = saved_from_resolved(config);
        dictionary_added_to_profile(&mut saved, Some("default"), name, roles).unwrap();
        *config = saved.resolved(Some("default")).unwrap();
    }

    fn dictionary_removed(config: &mut ResolvedConfig, name: &str) {
        let mut saved = saved_from_resolved(config);
        dictionary_removed_from_profiles(&mut saved, name);
        *config = saved.resolved(Some("default")).unwrap();
    }

    fn dictionary_work_resolved(before: &ResolvedConfig, after: &ResolvedConfig) -> DictionaryWork {
        super::dictionary_work(&saved_from_resolved(before), &saved_from_resolved(after))
    }

    fn from_config(config: &ResolvedConfig, dicts: &[DictInfo]) -> SettingsForm {
        from_resolved(config, dicts)
    }

    fn stale_order_entries(config: &ResolvedConfig, dicts: &[DictInfo]) -> Vec<String> {
        stale_order_entries_resolved(config, dicts)
    }

    fn dictionary_work(before: &ResolvedConfig, after: &ResolvedConfig) -> DictionaryWork {
        dictionary_work_resolved(before, after)
    }

    /// Deserializes legacy TOML so tests exercise the production migration path.
    fn pre_roles(order: &[&str]) -> Config {
        let mut dictionaries = toml::Table::new();
        dictionaries.insert(
            "display_order".into(),
            toml::Value::Array(
                order.iter().map(|name| toml::Value::String((*name).to_string())).collect(),
            ),
        );
        let mut legacy = toml::Table::new();
        legacy.insert("trigger".into(), toml::Value::Table(toml::Table::new()));
        legacy.insert("popup".into(), toml::Value::Table(toml::Table::new()));
        legacy.insert("dictionaries".into(), toml::Value::Table(dictionaries));
        let text = toml::to_string(&toml::Value::Table(legacy)).unwrap();
        toml::from_str(&text).unwrap()
    }

    fn names(rows: &[DictRow]) -> Vec<String> {
        rows.iter().map(|row| row.name.clone()).collect()
    }

    fn enabled_names(rows: &[DictRow]) -> Vec<String> {
        rows.iter().filter(|row| row.enabled).map(|row| row.name.clone()).collect()
    }

    fn insert_full(config: &mut Config, id: &str, name: &str, settings: ProfileSettings) {
        config.profiles.push(Profile {
            id: id.to_string(),
            name: name.to_string(),
            data: ProfileData::Full { settings: Box::new(settings) },
        });
    }

    fn insert_derived(
        config: &mut Config,
        id: &str,
        name: &str,
        parent: &str,
        overrides: BTreeMap<String, FieldOverride>,
    ) {
        config.profiles.push(Profile {
            id: id.to_string(),
            name: name.to_string(),
            data: ProfileData::Derived { parent: parent.to_string(), overrides },
        });
    }

    #[test]
    fn profile_merge_repeated_apply_reuses_saved_identity() {
        let saved = Config::default();
        let mut form = super::from_config(&saved, &[]);
        let id = form.create_profile("Local".into(), None, &[]).unwrap();
        let original = form.cfg.popup.theme.clone();
        form.cfg.popup.theme = if original == "dark" { "light" } else { "dark" }.into();
        form.save_current().unwrap();
        let first = super::apply_to(&form, &saved);
        let saved = first.config.clone();
        form.accept_applied(first, &[]).unwrap();
        form.cfg.popup.theme = original.clone();
        form.save_current().unwrap();

        let second = super::apply_to(&form, &saved).config;

        assert_eq!(saved.profiles.len(), second.profiles.len());
        assert_eq!(original, second.resolve(&id).unwrap().popup.theme);
    }

    #[test]
    fn profile_merge_retains_created_selection_after_id_collision() {
        let saved = Config::default();
        let mut local = super::from_config(&saved, &[]);
        let id = local.create_profile("Local".into(), None, &[]).unwrap();
        local.cfg.popup.theme = "light".into();
        local.save_current().unwrap();
        let mut external = super::from_config(&saved, &[]);
        assert_eq!(id, external.create_profile("External".into(), None, &[]).unwrap());
        external.cfg.popup.theme = "dark".into();
        external.save_current().unwrap();
        let applied = super::apply_to(&local, &external.catalog);
        local.accept_applied(applied, &[]).unwrap();

        assert_eq!("light", local.cfg.popup.theme);
        assert_ne!(id, local.profile_id);
        assert_eq!("dark", local.catalog.resolve(&id).unwrap().popup.theme);
    }

    #[test]
    fn ordinary_edit_keeps_an_equal_parent_override_and_reset_reveals_parent() {
        let mut saved = Config::default();
        let mut parent = saved.resolve("default").unwrap();
        parent.popup.theme = "dark".into();
        saved.update_profile("default", &parent).unwrap();
        insert_derived(&mut saved, "derived", "Derived", "default", BTreeMap::new());

        let mut form = super::from_config(&saved, &[]);
        form.select_profile("derived", &[]).unwrap();
        assert_eq!("dark", form.cfg.popup.theme);
        form.cfg.popup.theme = "light".into();

        let mut latest = saved.clone();
        let mut parent = latest.resolve("default").unwrap();
        parent.popup.theme = "light".into();
        latest.update_profile("default", &parent).unwrap();
        let applied = super::apply_to(&form, &latest).config;
        let profile = applied.profiles.iter().find(|profile| profile.id == "derived").unwrap();
        let ProfileData::Derived { overrides, .. } = &profile.data else { panic!("derived profile expected") };
        let expected = FieldOverride::Set(toml::Value::String("light".into()));
        assert_eq!(Some(&expected), overrides.get("popup.theme"));

        let mut reset = super::from_config(&applied, &[]);
        reset.select_profile("derived", &[]).unwrap();
        reset.reset_profile_override("popup.theme", &[]).unwrap();
        assert_eq!("light", reset.cfg.popup.theme);
        let ProfileData::Derived { overrides, .. } = &reset.catalog.profiles
            .iter().find(|profile| profile.id == "derived").unwrap().data else {
                panic!("derived profile expected")
            };
        assert!(!overrides.contains_key("popup.theme"));
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
    fn apply_preserves_an_untouched_latest_profile_edit() {
        let mut saved = Config::default();
        let mut other = saved.resolve("default").unwrap();
        other.popup.theme = "original".into();
        insert_full(&mut saved, "other", "Other", other);
        let mut form = super::from_config(&saved, &[]);
        form.cfg.popup.theme = "form".into();

        let mut latest = saved.clone();
        let mut other = latest.resolve("other").unwrap();
        other.popup.theme = "latest".into();
        latest.update_profile("other", &other).unwrap();
        let applied = super::apply_to(&form, &latest).config;
        assert_eq!("form", applied.resolved(Some("default")).unwrap().popup.theme);
        assert_eq!("latest", applied.resolved(Some("other")).unwrap().popup.theme);
    }

    #[test]
    fn profile_creation_duplication_and_rename_preserve_ids_and_values() {
        let saved = Config::default();
        let mut form = super::from_config(&saved, &[]);
        form.cfg.popup.theme = "edited".into();
        let copy_id = form.duplicate_profile("Copy".into(), &[]).unwrap();
        form.rename_profile("Renamed".into()).unwrap();
        form.set_default_profile(&copy_id).unwrap();
        let derived_id = form.create_profile("Derived".into(), Some("default"), &[]).unwrap();
        let full_id = form.create_profile("Fresh".into(), None, &[]).unwrap();
        assert_eq!(full_id, form.profile_id);

        let applied = super::apply_to(&form, &saved).config;
        assert_eq!(copy_id, applied.default_profile);
        assert_eq!("edited", applied.resolved(Some(&copy_id)).unwrap().popup.theme);
        let copy = applied.profiles.iter().find(|profile| profile.id == copy_id).unwrap();
        assert_eq!("Renamed", copy.name);
        assert!(matches!(&copy.data, ProfileData::Full { .. }));
        let derived = applied.profiles.iter().find(|profile| profile.id == derived_id).unwrap();
        assert!(matches!(&derived.data, ProfileData::Derived { parent, .. } if parent == "default"));
        let full = applied.profiles.iter().find(|profile| profile.id == full_id).unwrap();
        assert!(matches!(&full.data, ProfileData::Full { .. }));
    }

    #[test]
    fn profile_deletion_waits_for_default_bind_nested_and_parent_references() {
        let mut saved = Config::default();
        insert_full(&mut saved, "target", "Target", ProfileSettings::default());
        let other = ProfileSettings {
            nested_profile: Some("target".into()),
            ..Default::default()
        };
        insert_full(&mut saved, "other", "Other", other);
        saved.default_profile = "target".into();
        insert_derived(&mut saved, "child", "Child", "target", BTreeMap::new());
        let mut bind = crate::config::Bind::new("search".into(), crate::config::BindAction::Search);
        bind.profile = Some("target".into());
        saved.binds.push(bind);

        let mut form = super::from_config(&saved, &[]);
        assert!(form.delete_profile("target", &[]).is_err());
        assert!(form.catalog.profiles.iter().any(|profile| profile.id == "target"));

        form.set_default_profile("other").unwrap();
        assert!(form.delete_profile("target", &[]).is_err());
        form.catalog.binds.iter_mut().find(|bind| bind.id == "search").unwrap().profile = None;
        assert!(form.delete_profile("target", &[]).is_err());
        let mut other = form.catalog.resolve("other").unwrap();
        other.nested_profile = None;
        form.catalog.update_profile("other", &other).unwrap();
        assert!(form.delete_profile("target", &[]).is_err());

        form.delete_profile("child", &[]).unwrap();
        form.delete_profile("target", &[]).unwrap();
        assert_eq!("other", form.profile_id);
        assert!(!form.catalog.profiles.iter().any(|profile| profile.id == "target"));
    }

    #[test]
    fn empty_derived_role_and_language_lists_do_not_inherit_or_fall_back() {
        let mut saved = Config::default();
        let mut parent = saved.resolve("default").unwrap();
        parent.dictionaries.terms.enabled = vec!["Known".into()];
        parent.dictionaries.per_language.insert("ja".into(), vec!["Known".into()]);
        saved.update_profile("default", &parent).unwrap();
        let mut overrides = BTreeMap::new();
        overrides.insert(
            "dictionaries.terms".into(),
            FieldOverride::Set(toml::Value::try_from(RoleList::default()).unwrap()),
        );
        overrides.insert(
            "dictionaries.per_language".into(),
            FieldOverride::Set(toml::Value::try_from(BTreeMap::from([
                ("ja".to_string(), Vec::<String>::new()),
            ])).unwrap()),
        );
        insert_derived(&mut saved, "empty", "Empty", "default", overrides);

        let dicts = [DictInfo { dict_id: 1, name: "Known".into() }];
        let mut form = super::from_config(&saved, &dicts);
        form.select_profile("empty", &dicts).unwrap();
        assert!(enabled_names(&form.terms).is_empty());
        assert!(is_scoped(&form));
        assert!(form.cfg.dictionaries.per_language["ja"].is_empty());

        let applied = super::apply_to(&form, &saved).config;
        let empty = applied.resolve("empty").unwrap();
        assert!(empty.dictionaries.terms.enabled.is_empty());
        assert!(empty.dictionaries.per_language["ja"].is_empty());
    }

    #[test]
    fn a_staged_import_enables_its_owner_and_disables_other_explicit_profiles() {
        let mut saved = Config::default();
        let mut explicit = ProfileSettings::default();
        explicit.dictionaries.terms.enabled.push("Existing".into());
        insert_full(&mut saved, "explicit", "Explicit", explicit);
        insert_derived(&mut saved, "inherited", "Inherited", "default", BTreeMap::new());
        let mut overrides = BTreeMap::new();
        overrides.insert(
            "dictionaries.terms".into(),
            FieldOverride::Set(toml::Value::try_from(&RoleList {
                enabled: vec!["Seed".into()],
                disabled: Vec::new(),
            }).unwrap()),
        );
        insert_derived(&mut saved, "explicit-derived", "Explicit derived", "default", overrides);

        let mut form = super::from_config(&saved, &[]);
        form.stage_add(&fixture("terms.zip")).unwrap();
        assert_eq!(1, form.terms.iter().filter(|row| row.name == "FixtureTerms").count());
        assert!(form.terms.iter().find(|row| row.name == "FixtureTerms").unwrap().enabled);
        form.select_profile("explicit", &[]).unwrap();
        assert!(!form.terms.iter().find(|row| row.name == "FixtureTerms").unwrap().enabled);
        form.select_profile("default", &[]).unwrap();

        let applied = super::apply_to(&form, &saved).config;
        let owner = applied.resolve("default").unwrap();
        assert!(owner.dictionaries.terms.enabled.contains(&"FixtureTerms".into()));
        let other = applied.resolve("explicit").unwrap();
        assert!(other.dictionaries.terms.disabled.contains(&"FixtureTerms".into()));
        let inherited = applied.resolve("inherited").unwrap();
        assert!(inherited.dictionaries.terms.enabled.contains(&"FixtureTerms".into()));
        let child = applied.resolve("explicit-derived").unwrap();
        assert!(child.dictionaries.terms.disabled.contains(&"FixtureTerms".into()));
    }

    #[test]
    fn profile_merge_does_not_reassign_a_deleted_import_owner() {
        let mut saved = Config::default();
        insert_full(&mut saved, "importer", "Importer", ProfileSettings::default());
        let mut form = super::from_config(&saved, &[]);
        form.select_profile("importer", &[]).unwrap();
        form.stage_add(&fixture("terms.zip")).unwrap();
        form.delete_profile("importer", &[]).unwrap();

        let applied = super::apply_to(&form, &saved).config;

        assert!(applied.resolve("importer").is_err());
        let remaining = applied.resolve("default").unwrap();
        assert!(!remaining.dictionaries.terms.enabled.contains(&"FixtureTerms".into()));
        assert!(remaining.dictionaries.terms.disabled.contains(&"FixtureTerms".into()));
    }

    #[test]
    fn the_form_lists_dictionaries_in_the_configured_order() {
        let form =
            from_resolved(&cfg_with(&["大辞林　第四版", "Jitendex.org [2026-07-09]"]), &dicts());
        assert_eq!(vec!["大辞林　第四版", "Jitendex.org [2026-07-09]"], names(&form.terms));
        assert!(form.terms.iter().all(|row| row.enabled));
    }

    #[test]
    fn an_unlisted_dictionary_is_listed_last() {
        let form = from_resolved(&cfg_with(&["大辞林　第四版"]), &dicts());
        assert_eq!(vec!["大辞林　第四版", "Jitendex.org [2026-07-09]"], names(&form.terms));
    }

    /// Exact names must control order changes. The older substring model changed a
    /// pattern, so one edition change changed every Dictionary that matched it.
    /// A row now stores one name, and a move changes only that row.
    #[test]
    fn reordering_writes_the_exact_names_in_their_new_order() {
        let cfg = cfg_with(&["大辞林　第四版", "Jitendex.org [2026-07-09]"]);
        let mut form = from_resolved(&cfg, &dicts());
        form.terms.reverse();
        let out = apply_resolved(&form, &cfg);
        assert_eq!(
            vec!["Jitendex.org [2026-07-09]".to_string(), "大辞林　第四版".to_string()],
            out.dictionaries.terms,
        );
        assert!(out.dictionaries.terms_disabled.is_empty());
    }

    /// The migration converts a config with `大辞林` into exact installed names.
    /// The first Apply writes those names into six arrays and retires the old key.
    #[test]
    fn a_pre_roles_config_is_written_back_as_exact_names() {
        let cfg = pre_roles(&["大辞林", "Kenkyusha"]);
        let form = super::from_config(&cfg, &dicts());
        assert_eq!(
            vec!["大辞林　第四版", "Jitendex.org [2026-07-09]"],
            names(&form.terms),
            "the substring resolved, the one matching nothing was dropped, and the \
             dictionary no substring named landed at the bottom",
        );

        let out = super::apply_to(&form, &cfg).config;
        let resolved = out.resolved(Some(&form.profile_id)).unwrap();
        assert_eq!(
            vec!["大辞林　第四版".to_string(), "Jitendex.org [2026-07-09]".to_string()],
            resolved.dictionaries.terms,
        );
        let text = toml::to_string_pretty(&out).expect("the migrated config serialises");
        assert!(!text.contains("display_order"), "the retired key is gone: {text}");
        assert!(!text.contains("\"大辞林\""), "and so is the substring: {text}");
    }

    /// Apply keeps latest bind chords and target state while it saves profile edits.
    #[test]
    fn applying_the_form_preserves_latest_bind_chords_and_targets() {
        let mut latest = Config::default();
        let mut form = super::from_config(&latest, &[]);
        form.cfg.popup.theme = "light".to_string();

        let lookup = latest.binds.iter_mut()
            .find(|bind| bind.action == crate::config::BindAction::Lookup).unwrap();
        lookup.windows = "CTRL+J".to_string();
        lookup.linux = "SUPER+J".to_string();
        let mut settings = latest.resolve("default").unwrap();
        settings.actions.screenshot.fixed_region = Some([1, 2, 300, 400]);
        settings.anki.static_region = Some([10, 20, 30, 40]);
        latest.update_profile("default", &settings).unwrap();

        let out = super::apply_to(&form, &latest).config;
        let lookup = out.binds.iter()
            .find(|bind| bind.action == crate::config::BindAction::Lookup).unwrap();
        assert_eq!("CTRL+J", lookup.windows);
        assert_eq!("SUPER+J", lookup.linux);
        let applied = out.resolved(Some("default")).unwrap();
        assert_eq!(Some([1, 2, 300, 400]), applied.actions.screenshot.fixed_region);
        assert_eq!(Some([10, 20, 30, 40]), applied.anki.static_region);
        assert_eq!("light", applied.popup.theme);
    }

    #[test]
    fn profile_merge_deletes_dependents_before_parent() {
        let mut saved = Config::default();
        let parent = saved.resolve("default").unwrap();
        insert_full(&mut saved, "parent", "Parent", parent);
        insert_derived(&mut saved, "child", "Child", "parent", BTreeMap::new());
        let mut form = super::from_config(&saved, &[]);
        form.delete_profile("child", &[]).unwrap();
        form.delete_profile("parent", &[]).unwrap();

        let applied = super::apply_to(&form, &saved).config;

        assert_eq!(vec!["default"], applied.profiles.iter().map(|profile| profile.id.as_str()).collect::<Vec<_>>());
        applied.validate().unwrap();
    }

    #[test]
    fn profile_merge_resets_only_edited_profile() {
        let mut saved = Config::default();
        let mut first = saved.resolve("default").unwrap();
        first.actions.screenshot.fixed_region = Some([10, 20, 300, 200]);
        saved.update_profile("default", &first).unwrap();
        let mut second = first;
        second.actions.screenshot.fixed_region = Some([50, 60, 700, 400]);
        insert_full(&mut saved, "other", "Other", second);
        let mut form = super::from_config(&saved, &[]);
        form.screenshot_reset_targets = true;
        form.select_profile("other", &[]).unwrap();

        let applied = super::apply_to(&form, &saved).config;

        assert_eq!(None, applied.resolve("default").unwrap().actions.screenshot.fixed_region);
        assert_eq!(Some([50, 60, 700, 400]), applied.resolve("other").unwrap().actions.screenshot.fixed_region);
    }

    #[test]
    fn profile_merge_applies_explicit_reset_to_latest_targets() {
        let mut latest = Config::default();
        let mut form = super::from_config(&latest, &[]);
        form.screenshot_reset_targets = true;
        let mut profile = latest.resolve("default").unwrap();
        profile.actions.screenshot.fixed_region = Some([10, 20, 300, 200]);
        profile.actions.screenshot.fixed_window = Some(crate::config::ScreenshotWindow {
            app_id: "reader".into(), title: "日本語".into(),
        });
        profile.actions.screenshot.capture_mode = crate::config::ScreenshotMode::FixedWindow;
        latest.update_profile("default", &profile).unwrap();

        let applied = super::apply_to(&form, &latest).config.resolve("default").unwrap();

        assert_eq!(None, applied.actions.screenshot.fixed_region);
        assert_eq!(None, applied.actions.screenshot.fixed_window);
        assert_eq!(crate::config::ScreenshotMode::FixedWindow, applied.actions.screenshot.capture_mode);
    }

    #[test]
    fn profile_merge_keeps_untouched_language_scoped_inheritance() {
        let mut saved = Config::default();
        let mut parent = saved.resolve("default").unwrap();
        parent.popup.theme = "dark".into();
        parent.ocr.language = "ja".into();
        parent.dictionaries.terms.enabled = vec!["A".into(), "B".into()];
        parent.dictionaries.per_language.insert("ja".into(), vec!["A".into()]);
        insert_full(&mut saved, "parent", "Parent", parent.clone());
        insert_derived(&mut saved, "child", "Child", "parent", BTreeMap::new());
        let dicts = [
            DictInfo { dict_id: 1, name: "A".into() },
            DictInfo { dict_id: 2, name: "B".into() },
        ];
        let mut form = super::from_config(&saved, &dicts);
        form.select_profile("child", &dicts).unwrap();
        form.cfg.popup.theme = "light".into();

        let mut applied = super::apply_to(&form, &saved).config;

        assert_eq!(vec!["A", "B"], applied.resolve("child").unwrap().dictionaries.terms.enabled);
        parent.dictionaries.terms.enabled.push("C".into());
        applied.update_profile("parent", &parent).unwrap();
        let child = applied.resolve("child").unwrap();
        assert_eq!(vec!["A", "B", "C"], child.dictionaries.terms.enabled);
        assert_eq!("light", child.popup.theme);
    }

    #[test]
    fn profile_merge_removes_dictionary_from_latest_catalog() {
        let mut latest = Config::default();
        let mut parent = latest.resolve("default").unwrap();
        parent.dictionaries.terms.enabled = vec!["Gone".into()];
        latest.update_profile("default", &parent).unwrap();
        insert_derived(&mut latest, "child", "Child", "default", BTreeMap::new());
        let mut form = super::from_config(&latest, &[]);
        form.stage_remove("Gone");

        let mut child = latest.resolve("child").unwrap();
        child.dictionaries.terms.enabled.push("Extra".into());
        latest.update_profile("child", &child).unwrap();
        parent.dictionaries.pitch.enabled = vec!["Gone".into()];
        parent.dictionaries.per_language.insert("ja".into(), vec!["Gone".into()]);
        insert_full(&mut latest, "later", "Later", parent);
        latest.dictionaries.frequency = vec!["Gone".into()];

        let applied = super::apply_to(&form, &latest).config;

        assert!(applied.resolve("default").unwrap().dictionaries.terms.enabled.is_empty());
        assert_eq!(vec!["Extra"], applied.resolve("child").unwrap().dictionaries.terms.enabled);
        let later = applied.resolve("later").unwrap();
        assert!(later.dictionaries.terms.enabled.is_empty());
        assert!(later.dictionaries.pitch.enabled.is_empty());
        assert!(later.dictionaries.per_language["ja"].is_empty());
        assert!(applied.dictionaries.frequency.is_empty());
    }

    /// Installed dictionaries stay disabled when the profile has no terms list.
    #[test]
    fn an_empty_role_list_does_not_enable_installed_dictionaries() {
        let cfg = cfg_with(&[]);
        let form = from_resolved(&cfg, &dicts());
        let out = apply_resolved(&form, &cfg);
        assert!(out.dictionaries.terms.is_empty());
    }

    /// An unchecked enabled row moves to the disabled list.
    #[test]
    fn an_unchecked_row_lands_in_the_disabled_twin() {
        let cfg = cfg_with(&["大辞林　第四版", "Jitendex.org [2026-07-09]"]);
        let mut form = from_resolved(&cfg, &dicts());
        form.terms[0].enabled = false;
        let out = apply_resolved(&form, &cfg);
        assert_eq!(vec!["Jitendex.org [2026-07-09]".to_string()], out.dictionaries.terms);
        assert_eq!(vec!["大辞林　第四版".to_string()], out.dictionaries.terms_disabled);
        assert_eq!(vec!["Jitendex.org [2026-07-09]".to_string()], out.present_config().terms);
    }

    /// Exact names let one edition stay disabled while another stays enabled.
    #[test]
    fn two_dictionaries_sharing_a_substring_are_enabled_independently() {
        let editions = vec![
            DictInfo { dict_id: 1, name: "大辞林　第三版".into() },
            DictInfo { dict_id: 2, name: "大辞林　第四版".into() },
        ];
        let cfg = cfg_with(&["大辞林　第三版", "大辞林　第四版"]);
        let mut form = from_resolved(&cfg, &editions);
        form.terms[0].enabled = false;
        let out = apply_resolved(&form, &cfg);
        assert_eq!(vec!["大辞林　第四版".to_string()], out.dictionaries.terms);
        assert_eq!(vec!["大辞林　第三版".to_string()], out.dictionaries.terms_disabled);
        assert_eq!(vec!["大辞林　第四版".to_string()], out.present_config().terms);
    }

    /// A Config name with no Dictionary match must appear in the stale-name report.
    #[test]
    fn an_entry_matching_no_dictionary_is_reported() {
        let stale = stale_order_entries(&cfg_with(&["大辞林　第四版", "Kenkyusha"]), &dicts());
        assert_eq!(vec!["Kenkyusha".to_string()], stale);
    }

    #[test]
    fn entries_that_all_match_report_nothing() {
        let cfg = cfg_with(&["大辞林　第四版", "Jitendex.org [2026-07-09]"]);
        assert!(stale_order_entries(&cfg, &dicts()).is_empty());
    }

    /// The stale-name report checks every role because all arrays store Dictionary
    /// names with the same exact rule.
    #[test]
    fn a_stale_entry_is_reported_whichever_list_holds_it() {
        let mut cfg = ResolvedConfig::default();
        cfg.dictionaries.frequency = vec!["Gone".to_string()];
        cfg.dictionaries.pitch_disabled = vec!["Also gone".to_string()];
        assert_eq!(
            vec!["Gone".to_string(), "Also gone".to_string()],
            stale_order_entries(&cfg, &dicts()),
        );
    }


    #[test]
    fn selected_text_bind_edits_preserve_latest_linux_chord() {
        let mut latest = Config::default();
        latest.binds.push(crate::config::Bind::new(
            "selected-text".into(), crate::config::BindAction::SelectedText,
        ));
        let mut form = super::from_config(&latest, &[]);
        let edited = form.catalog.binds.iter_mut()
            .find(|bind| bind.action == crate::config::BindAction::SelectedText).unwrap();
        edited.windows = "ALT+L".into();
        latest.binds.iter_mut()
            .find(|bind| bind.action == crate::config::BindAction::SelectedText).unwrap()
            .linux = "SUPER+K".into();

        let out = super::apply_to(&form, &latest).config;
        let saved = out.binds.iter()
            .find(|bind| bind.action == crate::config::BindAction::SelectedText).unwrap();
        assert_eq!("ALT+L", saved.windows);
        assert_eq!("SUPER+K", saved.linux);
    }


    /// An empty field map is an explicit user answer that Apply must save.
    #[test]
    fn emptying_the_field_map_saves_an_empty_map() {
        let mut cfg = cfg_with(&[]);
        cfg.anki.field_map =
            vec![FieldMapping { anki_field: "Front".into(), source: "expression".into() }];
        let mut form = from_resolved(&cfg, &dicts());
        form.field_map = Some(Vec::new());
        assert!(
            apply_resolved(&form, &cfg).anki.field_map.is_empty(),
            "the user emptied the map; Apply must save that"
        );
    }

    /// A form without loaded field names has no answer. That differs from an empty
    /// field map, which is an explicit answer.
    #[test]
    fn a_window_with_nothing_to_say_cannot_wipe_the_field_map() {
        let mut cfg = cfg_with(&[]);
        cfg.anki.field_map =
            vec![FieldMapping { anki_field: "Front".into(), source: "expression".into() }];
        let mut form = from_resolved(&cfg, &dicts());
        form.field_map = None;
        assert_eq!(
            cfg.anki.field_map,
            apply_resolved(&form, &cfg).anki.field_map,
            "Anki was unreachable; Apply must leave the saved map alone"
        );
    }

    #[test]
    fn an_open_settings_form_preserves_a_newly_saved_screenshot_target() {
        let mut cfg = cfg_with(&[]);
        let mut form = from_resolved(&cfg, &dicts());
        cfg.actions.screenshot.fixed_region = Some([-300, 40, 200, 100]);
        cfg.actions.screenshot.fixed_window = Some(crate::config::ScreenshotWindow {
            app_id: "reader".into(),
            title: "日本語".into(),
        });
        form.cfg.actions.screenshot.capture_mode = crate::config::ScreenshotMode::FixedWindow;
        let out = apply_resolved(&form, &cfg);
        assert_eq!(cfg.actions.screenshot.fixed_region, out.actions.screenshot.fixed_region);
        assert_eq!(cfg.actions.screenshot.fixed_window, out.actions.screenshot.fixed_window);
    }

    #[test]
    fn resetting_screenshot_targets_replaces_only_the_saved_targets() {
        let mut cfg = cfg_with(&[]);
        cfg.actions.screenshot.fixed_region = Some([0, 40, 200, 100]);
        cfg.actions.screenshot.fixed_window = Some(crate::config::ScreenshotWindow {
            app_id: "reader".into(),
            title: "日本語".into(),
        });
        cfg.actions.screenshot.capture_mode = crate::config::ScreenshotMode::FixedWindow;
        let mut form = from_resolved(&cfg, &dicts());
        form.screenshot_reset_targets = true;
        let out = apply_resolved(&form, &cfg);
        assert_eq!(None, out.actions.screenshot.fixed_region);
        assert_eq!(None, out.actions.screenshot.fixed_window);
        assert_eq!(cfg.actions.screenshot.capture_mode, out.actions.screenshot.capture_mode);
    }


    #[test]
    fn out_of_range_numbers_are_clamped_not_rejected() {
        let cfg = cfg_with(&[]);
        let mut form = from_resolved(&cfg, &dicts());
        form.cfg.popup.max_width_percent = 250;
        form.cfg.popup.max_height_percent = 250;
        form.cfg.popup.summary_chars = 1;
        form.cfg.ocr.max_ocr_passes = 99;
        let out = apply_resolved(&form, &cfg);
        assert_eq!(MAX_WIDTH_RANGE.1, out.popup.max_width_percent);
        assert_eq!(MAX_HEIGHT_RANGE.1, out.popup.max_height_percent);
        assert_eq!(SUMMARY_RANGE.0, out.popup.summary_chars);
        assert_eq!(PASSES_RANGE.1, out.ocr.max_ocr_passes);
    }

    #[test]
    fn apply_to_clamps_the_capture_size() {
        let cfg = ResolvedConfig::default();
        let mut form = from_resolved(&cfg, &dicts());
        form.cfg.ocr.capture_width = 99_999;
        form.cfg.ocr.capture_height = 1;
        let out = apply_resolved(&form, &cfg);
        assert_eq!(CAPTURE_W_RANGE.1, out.ocr.capture_width);
        assert_eq!(CAPTURE_H_RANGE.0, out.ocr.capture_height);
    }


    #[test]
    fn in_range_capture_values_produce_no_notice() {
        let cfg = ResolvedConfig::default();
        let mut form = from_resolved(&cfg, &dicts());
        form.cfg.ocr.capture_width = 500;
        form.cfg.ocr.capture_height = 220;
        assert_eq!(None, clamp_notice(&form, &apply_resolved(&form, &cfg)));
    }


    #[test]
    fn an_unrendered_application_preference_is_preserved() {
        let mut saved = Config::default();
        saved.application.background_on_close = false;
        let mut form = super::from_config(&saved, &dicts());
        form.cfg.popup.theme = "light".to_string();
        saved.application.background_on_close = true;

        let applied = super::apply_to(&form, &saved).config;

        assert!(applied.application.background_on_close);
        assert_eq!("light", applied.resolve(&applied.default_profile).unwrap().popup.theme);
    }

    #[test]
    fn a_windows_application_preference_answer_is_applied() {
        let cfg = ResolvedConfig::default();
        let mut form = from_resolved(&cfg, &dicts());
        assert_eq!(None, form.background_on_close);
        form.background_on_close = Some(true);

        let applied = apply_resolved(&form, &cfg);

        assert!(applied.application.background_on_close);
    }


    #[test]
    fn from_config_puts_the_languages_own_terms_first_and_unchecks_the_rest() {
        let mut cfg = ResolvedConfig::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries
            .per_language
            .insert("ja".to_string(), vec!["大辞林　第四版".to_string()]);
        let form = from_resolved(&cfg, &dicts());
        assert_eq!(vec!["大辞林　第四版".to_string()], enabled_names(&form.terms));
        assert!(form.terms.iter().any(|row| row.name.contains("Jitendex") && !row.enabled));
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

    /// Other language lists must remain unchanged.
    #[test]
    fn apply_to_preserves_other_languages() {
        let mut cfg = ResolvedConfig::default();
        cfg.ocr.language = "ja".to_string();
        let mut form = from_resolved(&cfg, &dicts());
        form.cfg.dictionaries.per_language.insert("zh-Hans-CN".to_string(), vec!["中日大辞典".to_string()]);
        form.terms = vec![DictRow { name: "大辞林　第四版".to_string(), enabled: true }];
        let out = apply_resolved(&form, &cfg);
        assert_eq!(
            vec!["中日大辞典".to_string()],
            out.dictionaries.per_language["zh-Hans-CN"],
            "the other language's list must survive",
        );
    }

    /// A second Apply must preserve the language key and write the exact names
    /// that the second form contains.
    #[test]
    fn a_second_apply_rewrites_the_key_the_first_one_wrote() {
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
        let first = apply_resolved(&form, &cfg);
        assert_eq!(vec!["大辞林　第四版".to_string()], first.dictionaries.per_language["ja"]);

        form.reseed_per_language(&first.dictionaries.per_language);
        form.terms[1].enabled = true;
        let second = apply_resolved(&form, &first);
        assert_eq!(
            vec!["大辞林　第四版".to_string(), "Jitendex.org [2026-07-09]".to_string()],
            second.dictionaries.per_language["ja"],
            "the second Apply must rewrite the key, never drop it",
        );
    }

    /// The Library merge must preserve a language's scope.
    #[test]
    fn with_library_keeps_the_exclusion_scoped() {
        let mut cfg = ResolvedConfig::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries
            .per_language
            .insert("ja".to_string(), vec!["大辞林　第四版".to_string()]);
        let form = with_library(from_resolved(&cfg, &dicts()), &library());
        assert!(!enabled_names(&form.terms).iter().any(|n| n.contains("Jitendex")));
        assert!(form.terms.iter().any(|row| row.name.contains("Jitendex") && !row.enabled));
    }

    /// A stale `dict_list_language` value must not replace the real language list.
    #[test]
    fn apply_to_does_not_write_a_stale_dict_list_language() {
        let mut cfg = ResolvedConfig::default();
        cfg.dictionaries
            .per_language
            .insert("zh-Hans-CN".to_string(), vec!["中日大辞典".to_string()]);
        let mut form = from_resolved(&cfg, &dicts());
        form.cfg.ocr.language = "zh-Hans-CN".to_string();
        let out = apply_resolved(&form, &cfg);
        assert_eq!(
            vec!["中日大辞典".to_string()],
            out.dictionaries.per_language["zh-Hans-CN"],
            "a stale dict list must not overwrite the real one",
        );
    }

    #[test]
    fn clearing_language_rows_saves_an_explicit_empty_scope() {
        let mut cfg = ResolvedConfig::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries.terms = vec!["大辞林　第四版".to_string()];
        cfg.dictionaries
            .per_language
            .insert("ja".to_string(), vec!["大辞林　第四版".to_string()]);
        let mut form = from_resolved(&cfg, &dicts());
        for row in &mut form.terms {
            row.enabled = false;
        }
        let out = apply_resolved(&form, &cfg);
        assert!(out.dictionaries.per_language["ja"].is_empty());
        assert_eq!(vec!["大辞林　第四版".to_string()], out.dictionaries.terms);
    }

    /// I2-b: Only unreadable rows remain, so Apply must preserve the current entry.
    #[test]
    fn apply_to_will_not_erase_a_list_for_an_unreadable_row() {
        let mut cfg = ResolvedConfig::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries
            .per_language
            .insert("ja".to_string(), vec!["大辞林　第四版".to_string()]);
        let mut form = from_resolved(&cfg, &dicts());
        form.terms = vec![
            DictRow { name: "bad.zip".to_string(), enabled: true },
            DictRow { name: "Jitendex.org [2026-07-09]".to_string(), enabled: false },
        ];
        form.unreadable = vec!["bad.zip".to_string()];
        let out = apply_resolved(&form, &cfg);
        assert_eq!(
            vec!["大辞林　第四版".to_string()],
            out.dictionaries.per_language["ja"],
            "a split naming only an unreadable file must leave the entry alone",
        );
    }

    /// A language list that names no installed Dictionary remains that language's
    /// list. The old guard discarded it and searched every Dictionary to avoid
    /// substring matches. Exact names prevent that false match.
    #[test]
    fn from_config_keeps_a_language_list_naming_nothing_installed() {
        let mut cfg = ResolvedConfig::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries.per_language.insert("ja".to_string(), vec!["Daijirin".to_string()]);
        let form = from_resolved(&cfg, &dicts());
        assert_eq!(vec!["Daijirin".to_string()], enabled_names(&form.terms));
        assert!(
            form.terms.iter().filter(|row| !row.enabled).count() == 2,
            "and both installed dictionaries sit unchecked below it: {:?}",
            form.terms,
        );
    }

    #[test]
    fn from_config_still_splits_a_list_that_matches_one() {
        let mut cfg = ResolvedConfig::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries
            .per_language
            .insert("ja".to_string(), vec!["大辞林　第四版".to_string()]);
        let form = from_resolved(&cfg, &dicts());
        assert_eq!(vec!["大辞林　第四版".to_string()], enabled_names(&form.terms));
        assert_eq!(
            vec!["Jitendex.org [2026-07-09]".to_string()],
            form.terms.iter().filter(|r| !r.enabled).map(|r| r.name.clone()).collect::<Vec<_>>(),
        );
    }

    fn staged_form() -> SettingsForm {
        from_resolved(&cfg_with(&["大辞林　第四版", "Jitendex.org [2026-07-09]"]), &dicts())
    }

    #[test]
    fn a_fresh_form_stages_nothing() {
        let form = staged_form();
        assert!(!form.has_staged());
        assert!(!form.freq_changed);
        assert!(form.frequency.is_empty());
        assert!(form.pitch.is_empty());
        assert!(!form.library_empty);
    }

    #[test]
    fn staging_an_add_then_removing_it_is_a_no_op() {
        let mut form = staged_form();
        let before = names(&form.terms);
        assert_eq!(
            Some(Roles::only(&[Role::Terms])),
            form.stage_add(&fixture("terms.zip")),
        );
        assert_eq!(1, form.staged_adds.len());
        assert_eq!("FixtureTerms", form.staged_adds[0].name, "the title, not the filename");

        form.stage_remove("FixtureTerms");

        assert!(form.staged_adds.is_empty());
        assert!(form.staged_removes.is_empty(), "the library was never asked for it");
        assert_eq!(before, names(&form.terms));
        assert!(!form.has_staged());
    }

    #[test]
    fn a_staged_add_keeps_the_position_the_user_gave_it() {
        let cfg = ResolvedConfig::default();
        let installed = vec![DictInfo { dict_id: 1, name: "Jitendex.org [2026]".into() }];
        let mut form = from_resolved(&cfg, &installed);
        form.stage_add(&fixture("terms.zip")).expect("a real archive stages");

        // Move the staged row to the top.
        let name = form.staged_adds[0].name.clone();
        form.terms.retain(|row| row.name != name);
        form.terms.insert(0, DictRow { name: name.clone(), enabled: true });

        let out = apply_resolved(&form, &cfg);

        assert_eq!(
            Some(&name),
            out.dictionaries.terms.first(),
            "the position the user chose must survive Apply",
        );
    }

    #[test]
    fn staging_import_enables_an_existing_disabled_row_once() {
        let mut cfg = ResolvedConfig::default();
        cfg.dictionaries.terms_disabled = vec!["FixtureTerms".into()];
        let mut form = from_resolved(&cfg, &[]);
        form.stage_add(&fixture("terms.zip")).unwrap();
        assert_eq!(1, form.terms.iter().filter(|row| row.name == "FixtureTerms").count());
        assert!(form.terms.iter().find(|row| row.name == "FixtureTerms").unwrap().enabled);

        let out = apply_resolved(&form, &cfg);
        assert_eq!(vec!["FixtureTerms".to_string()], out.dictionaries.terms);
        assert!(out.dictionaries.terms_disabled.is_empty());
    }

    /// An import goes to the bottom of each role list, enabled. It does not move
    /// a prior row.
    #[test]
    fn an_import_lands_at_the_bottom_of_each_of_its_role_lists() {
        let mut form = staged_form();
        let before = names(&form.terms);

        assert_eq!(
            Some(Roles::only(&[Role::Terms, Role::Pitch])),
            form.stage_add(&fixture("both.zip")),
        );

        let mut expected = before.clone();
        expected.push("FixtureBoth".to_string());
        assert_eq!(expected, names(&form.terms), "at the bottom, and nothing else moved");
        assert_eq!(vec!["FixtureBoth".to_string()], names(&form.pitch));
        assert!(form.frequency.is_empty(), "it supplies no frequency data");
        assert!(
            form.terms.last().unwrap().enabled && form.pitch[0].enabled,
            "and it arrives switched on in both",
        );
    }

    /// A pitch-only archive belongs only in the Pitch list. The old filename
    /// heuristic placed it in the terms list.
    #[test]
    fn a_pitch_only_import_reaches_the_pitch_list_alone() {
        let mut form = staged_form();
        let terms_before = names(&form.terms);

        assert_eq!(Some(Roles::only(&[Role::Pitch])), form.stage_add(&fixture("pitch.zip")));

        assert_eq!(vec!["FixturePitch".to_string()], names(&form.pitch));
        assert_eq!(terms_before, names(&form.terms));
        assert!(form.frequency.is_empty());
    }

    #[test]
    fn a_frequency_archive_lands_in_the_frequency_list_whichever_button_was_used() {
        let mut form = staged_form();
        let terms_before = names(&form.terms);

        // The test starts with the archive under Dictionaries.
        assert_eq!(
            Some(Roles::only(&[Role::Frequency])),
            form.stage_add(&fixture("freq.zip")),
        );

        assert_eq!(vec!["FixtureFreq".to_string()], names(&form.frequency));
        assert_eq!(terms_before, names(&form.terms), "it supplies no definitions");
    }

    #[test]
    fn staging_a_freq_add_sets_freq_changed() {
        let mut form = staged_form();
        assert_eq!(
            Some(Roles::only(&[Role::Frequency])),
            form.stage_add(&fixture("freq.zip")),
        );
        assert!(form.freq_changed);
    }

    #[test]
    fn staging_a_term_add_does_not_set_freq_changed() {
        let mut form = staged_form();
        assert_eq!(
            Some(Roles::only(&[Role::Terms])),
            form.stage_add(&fixture("terms.zip")),
        );
        assert!(!form.freq_changed);
    }

    #[test]
    fn an_unreadable_file_cannot_be_staged() {
        let mut form = staged_form();
        let junk = std::env::temp_dir().join(format!("notazip_{}.zip", std::process::id()));
        std::fs::write(&junk, b"not a zip").unwrap();

        assert_eq!(None, form.stage_add(&junk));

        assert!(form.staged_adds.is_empty());
        assert!(!form.has_staged());
        let _ = std::fs::remove_file(&junk);
    }

    #[test]
    fn a_removal_preserves_the_order_of_the_rest() {
        let mut form = staged_form();
        form.terms = ["a", "b", "c"]
            .into_iter()
            .map(|name| DictRow { name: name.to_string(), enabled: true })
            .collect();

        form.stage_remove("b");

        assert_eq!(vec!["a".to_string(), "c".to_string()], names(&form.terms));
        assert_eq!(vec!["b".to_string()], form.staged_removes);
        assert!(form.has_staged());
    }

    /// One archive is one Dictionary. Removal therefore removes every role that
    /// it held, not only the role list that received the click.
    #[test]
    fn a_removal_drops_the_dictionary_from_every_list() {
        let mut form = staged_form();
        form.stage_add(&fixture("both.zip")).expect("a mixed archive stages");
        form.frequency.push(DictRow { name: "FixtureBoth".to_string(), enabled: true });

        form.stage_remove("FixtureBoth");

        assert!(!names(&form.terms).contains(&"FixtureBoth".to_string()));
        assert!(form.pitch.is_empty());
        assert!(form.frequency.is_empty());
    }

    #[test]
    fn two_parts_sharing_a_title_can_both_be_staged() {
        let mut form = staged_form();
        let dir = std::env::temp_dir().join(format!("chibi_parts_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let a = dir.join("part1.zip");
        let b = dir.join("part2.zip");
        std::fs::copy(fixture("terms.zip"), &a).unwrap();
        std::fs::copy(fixture("terms.zip"), &b).unwrap();

        assert_eq!(Some(Roles::only(&[Role::Terms])), form.stage_add(&a));
        assert_eq!(
            Some(Roles::only(&[Role::Terms])),
            form.stage_add(&b),
            "a split edition shares its title",
        );

        assert_eq!(2, form.staged_adds.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_same_file_cannot_be_staged_twice() {
        let mut form = staged_form();
        assert_eq!(
            Some(Roles::only(&[Role::Terms])),
            form.stage_add(&fixture("terms.zip")),
        );
        assert_eq!(None, form.stage_add(&fixture("terms.zip")));
        assert_eq!(1, form.staged_adds.len());
    }

    #[test]
    fn an_add_of_an_already_listed_name_is_rejected_not_duplicated() {
        let mut form = staged_form();
        assert_eq!(
            Some(Roles::only(&[Role::Terms])),
            form.stage_add(&fixture("terms.zip")),
        );

        assert_eq!(None, form.stage_add(&fixture("terms.zip")));

        assert_eq!(1, form.staged_adds.len());
        assert_eq!(1, names(&form.terms).iter().filter(|n| *n == "FixtureTerms").count());
    }

    /// Only the same source file blocks a duplicate stage.
    #[test]
    fn a_title_an_installed_dictionary_uses_does_not_block_the_add() {
        let mut form = staged_form();
        form.terms.push(DictRow { name: "FixtureTerms".into(), enabled: true });
        assert_eq!(
            Some(Roles::only(&[Role::Terms])),
            form.stage_add(&fixture("terms.zip")),
        );
        assert_eq!(1, form.staged_adds.len());
    }

    #[test]
    fn removing_the_same_row_twice_records_it_once() {
        let mut form = staged_form();
        let name = form.terms[0].name.clone();
        form.stage_remove(&name);
        form.stage_remove(&name);
        assert_eq!(vec![name], form.staged_removes);
    }

    #[test]
    fn a_frequency_row_is_removable_by_the_same_call() {
        let mut form = staged_form();
        form.frequency = vec![DictRow { name: "jiten_freq_global.zip".into(), enabled: true }];
        form.stage_remove("jiten_freq_global.zip");
        assert!(form.frequency.is_empty());
        assert_eq!(vec!["jiten_freq_global.zip".to_string()], form.staged_removes);
    }

    #[test]
    fn removing_a_freq_row_sets_freq_changed() {
        let mut form = staged_form();
        form.frequency = vec![DictRow { name: "FixtureFreq".into(), enabled: true }];
        form.stage_remove("FixtureFreq");
        assert!(form.freq_changed);
    }

    #[test]
    fn removing_a_term_row_does_not_set_freq_changed() {
        let mut form = staged_form();
        let name = form.terms[0].name.clone();
        form.stage_remove(&name);
        assert!(!form.freq_changed);
    }

    #[test]
    fn a_removed_dictionary_loses_its_place_in_every_array() {
        let cfg = cfg_with(&["大辞林　第四版", "Jitendex.org [2026-07-09]"]);
        let mut form = from_resolved(&cfg, &dicts());
        form.stage_remove("大辞林　第四版");
        assert_eq!(
            vec!["Jitendex.org [2026-07-09]".to_string()],
            apply_resolved(&form, &cfg).dictionaries.terms,
        );
    }

    /// The Dictionary title controls order. The source file name does not.
    #[test]
    fn a_staged_add_is_ordered_by_its_title_not_its_filename() {
        let cfg = cfg_with(&["大辞林　第四版", "Jitendex.org [2026-07-09]"]);
        let mut form = from_resolved(&cfg, &dicts());
        assert_eq!(
            Some(Roles::only(&[Role::Terms])),
            form.stage_add(&fixture("terms.zip")),
        );
        let out = apply_resolved(&form, &cfg);
        assert!(
            !out.dictionaries.terms.iter().any(|e| e.contains(".zip")),
            "a filename must never reach a Dictionary list",
        );
        assert_eq!(
            vec![
                "大辞林　第四版".to_string(),
                "Jitendex.org [2026-07-09]".to_string(),
                "FixtureTerms".to_string()
            ],
            out.dictionaries.terms,
        );
    }

    fn library() -> Library {
        Library {
            entries: vec![
                crate::library::Entry {
                    file: "jitendex.zip".into(),
                    name: "Jitendex.org [2026-07-09]".into(),
                    roles: Roles::only(&[Role::Terms]),
                },
                crate::library::Entry {
                    file: "daijirin.zip".into(),
                    name: "大辞林　第四版".into(),
                    roles: Roles::only(&[Role::Terms]),
                },
                crate::library::Entry {
                    file: "freq.zip".into(),
                    name: "jiten_freq_global".into(),
                    roles: Roles::only(&[Role::Frequency]),
                },
            ],
        }
    }

    #[test]
    fn the_frequency_list_comes_from_the_library_not_the_database() {
        let form = with_library(staged_form(), &library());
        assert_eq!(vec!["jiten_freq_global".to_string()], names(&form.frequency));
        assert!(!form.library_empty);
        assert_eq!(vec!["大辞林　第四版", "Jitendex.org [2026-07-09]"], names(&form.terms));
    }

    /// A mixed archive appears once in each role list that it provides.
    /// It does not appear in the list for a role that it does not provide.
    #[test]
    fn a_mixed_archive_appears_in_both_its_lists_once_each() {
        let lib = Library {
            entries: vec![crate::library::Entry {
                file: "mixed.zip".into(),
                name: "Mixed".into(),
                roles: Roles::only(&[Role::Terms, Role::Frequency]),
            }],
        };
        let form = with_library(from_resolved(&ResolvedConfig::default(), &[]), &lib);
        assert_eq!(vec!["Mixed".to_string()], names(&form.terms));
        assert_eq!(vec!["Mixed".to_string()], names(&form.frequency));
        assert!(form.pitch.is_empty());
    }

    /// The Library supplies the role set. A known name in a role that its
    /// archive does not provide leaves that list. An unknown name keeps its
    /// config position, so a disconnected drive keeps its name.
    #[test]
    fn the_library_corrects_a_role_and_leaves_a_name_it_does_not_know() {
        let mut cfg = ResolvedConfig::default();
        cfg.dictionaries.pitch = vec!["jiten_freq_global".to_string()];
        cfg.dictionaries.terms = vec!["On the USB stick".to_string()];
        let form = with_library(from_resolved(&cfg, &[]), &library());
        assert!(form.pitch.is_empty(), "the archive supplies no pitch: {:?}", form.pitch);
        assert_eq!(
            vec![
                "On the USB stick".to_string(),
                "Jitendex.org [2026-07-09]".to_string(),
                "大辞林　第四版".to_string()
            ],
            names(&form.terms),
        );
    }

    #[test]
    fn an_empty_library_says_so() {
        assert!(with_library(staged_form(), &Library::default()).library_empty);
    }

    /// A staged removal resolves a Dictionary name to its source file.
    #[test]
    fn a_removed_row_resolves_to_the_file_that_produced_it() {
        let mut form = with_library(staged_form(), &library());
        form.stage_remove("大辞林　第四版");
        form.stage_remove("jiten_freq_global");
        assert_eq!(
            vec!["daijirin.zip".to_string(), "freq.zip".to_string()],
            removed_files(&form, &library())
        );
    }

    /// A name absent from the Library has no source file.
    #[test]
    fn a_row_the_library_never_held_names_no_file() {
        let mut form = staged_form();
        form.stage_remove("大辞林　第四版");
        assert!(removed_files(&form, &Library::default()).is_empty());
    }

    #[test]
    fn removing_every_dictionary_would_leave_nothing_to_build_from() {
        let mut form = with_library(staged_form(), &library());
        assert_eq!(2, terms_after_apply(&form, &library()));
        form.stage_remove("大辞林　第四版");
        assert_eq!(1, terms_after_apply(&form, &library()));
        form.stage_remove("Jitendex.org [2026-07-09]");
        assert_eq!(0, terms_after_apply(&form, &library()));
    }

    /// Frequency data alone cannot provide term archives.
    #[test]
    fn a_frequency_only_library_leaves_no_term_archives() {
        let mut form = with_library(staged_form(), &Library::default());
        form.terms.clear();
        assert_eq!(
            Some(Roles::only(&[Role::Frequency])),
            form.stage_add(&fixture("freq.zip")),
        );
        assert_eq!(0, terms_after_apply(&form, &Library::default()));
        assert_eq!(
            Some(Roles::only(&[Role::Terms])),
            form.stage_add(&fixture("terms.zip")),
        );
        assert_eq!(1, terms_after_apply(&form, &Library::default()));
    }

    /// An absent source path cannot stage an import.
    #[test]
    fn an_add_that_no_longer_exists_cannot_be_staged_at_all() {
        let mut form = with_library(staged_form(), &Library::default());
        form.terms.clear();
        assert_eq!(None, form.stage_add(Path::new(r"C:\gone\jmdict.zip")));
        assert_eq!(0, terms_after_apply(&form, &Library::default()));
    }

    /// The stale row stays visible, so Apply writes it back. The drive can then
    /// return without loss of its list position.
    #[test]
    fn a_stale_entry_survives_an_apply_that_never_saw_its_dictionary() {
        let cfg = cfg_with(&["Kenkyusha", "大辞林　第四版"]);
        assert_eq!(vec!["Kenkyusha".to_string()], stale_order_entries(&cfg, &dicts()));
        let out = apply_resolved(&from_resolved(&cfg, &dicts()), &cfg);
        assert!(out.dictionaries.terms.contains(&"Kenkyusha".to_string()));
        assert_eq!(vec!["Kenkyusha".to_string()], stale_order_entries(&out, &dicts()));
        assert!(
            !dicts().iter().any(|d| d.name == "Kenkyusha"),
            "and nothing installed answers to it, so it orders and enables nothing",
        );
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

    fn archives_in(dir: &Path) -> Vec<String> {
        files_in(dir).into_iter().filter(|n| n.ends_with(".zip")).collect()
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

    /// The archive moves to `.removed` before commit. It is deleted only after
    /// commit.
    #[test]
    fn a_remove_deletes_the_archive_it_names_only_once_committed() {
        let (dir, _guard) = stocked("remove");
        let mut form = form_for(&dir);
        assert!(form.stage_add(&fixture("terms.zip")).is_some());
        form.stage_remove("FixtureFreq");

        let pending = stage_into_library(&form, &dir).unwrap();
        assert!(dir.join(".removed").join("freq.zip").is_file(), "held, not deleted");
        pending.commit().unwrap();

        assert_eq!(vec!["library.json", "terms (2).zip", "terms.zip"], files_in(&dir));
        assert!(Library::load(&dir).unwrap().freq_paths(&dir).is_empty());
    }

    /// Removal cancels a staged add with the same Dictionary title.
    /// One title identifies one row.
    #[test]
    fn removing_a_row_that_is_a_staged_add_cancels_the_add() {
        let (dir, _guard) = stocked("overlap");
        let before = files_in(&dir);
        let mut form = form_for(&dir);
        assert!(form.stage_add(&dir.join("terms.zip")).is_some());

        form.stage_remove("FixtureTerms");

        assert!(!form.has_staged(), "add and remove cancel out");
        assert_eq!(before, files_in(&dir), "nothing was copied or deleted");
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

    /// Frequency data alone is not a Dictionary for term lookup.
    #[test]
    fn a_frequency_only_library_is_refused_too() {
        let (dir, _guard) = stocked("freq_only");
        let mut form = form_for(&dir);
        // The form now has only frequency data.
        form.stage_remove("FixtureTerms");

        assert!(stage_into_library(&form, &dir).is_err());
        assert!(dir.join("terms.zip").exists());
    }

    /// The archive banks determine the role, not the list where the user added it.
    #[test]
    fn a_frequency_archive_added_under_dictionaries_supplies_no_terms() {
        let (dir, _guard) = stocked("misfiled");
        let before = files_in(&dir);
        let mut form = form_for(&dir);
        form.stage_remove("FixtureTerms");
        form.frequency.clear();
        // The archive banks supply this role.
        assert_eq!(
            Some(Roles::only(&[Role::Frequency])),
            form.stage_add(&fixture("freq.zip")),
        );
        assert!(!names(&form.terms).iter().any(|n| n.contains("Freq")));
        assert!(names(&form.frequency).contains(&"FixtureFreq".to_string()));

        assert_eq!(0, terms_after_apply(&form, &Library::load(&dir).unwrap()));
        let refused = stage_into_library(&form, &dir).unwrap_err();

        assert!(format!("{refused:#}").contains("no dictionary"), "{refused:#}");
        assert_eq!(before, files_in(&dir));
    }

    /// An unreadable archive cannot provide a usable Dictionary.
    #[test]
    fn a_corrupt_archive_does_not_satisfy_the_guard() {
        let (dir, _guard) = stocked("corrupt_guard");
        std::fs::remove_file(dir.join("freq.zip")).unwrap();
        std::fs::write(dir.join("broken.zip"), b"not a zip at all").unwrap();
        let before = files_in(&dir);
        let mut form = form_for(&dir);
        assert!(
            names(&form.terms).contains(&"broken.zip".to_string()),
            "it must stay visible so it can be removed: {:?}",
            form.terms
        );
        form.stage_remove("FixtureTerms");

        let refused = stage_into_library(&form, &dir).unwrap_err();

        assert!(format!("{refused:#}").contains("no dictionary"), "{refused:#}");
        assert_eq!(before, files_in(&dir), "terms.zip survives");
    }

    #[test]
    fn an_unreadable_row_reaches_no_dictionary_list() {
        let (dir, _guard) = stocked("unreadable_order");
        std::fs::write(dir.join("broken.zip"), b"not a zip at all").unwrap();
        let cfg = cfg_with(&[]);
        let form = with_library(from_resolved(&cfg, &dicts()), &Library::load(&dir).unwrap());

        let out = apply_resolved(&form, &cfg);

        for role in Role::EVERY {
            let (on, off) = out.dictionaries.lists(role);
            assert!(
                !on.iter().chain(off).any(|e| e.contains("broken")),
                "{role:?}: {on:?} {off:?}",
            );
        }
    }

    /// The user must be able to remove an unreadable file.
    #[test]
    fn removing_an_unreadable_row_quarantines_the_file_it_names() {
        let (dir, _guard) = stocked("unreadable_remove");
        std::fs::write(dir.join("broken.zip"), b"not a zip at all").unwrap();
        let mut form = form_for(&dir);
        form.stage_remove("broken.zip");

        stage_into_library(&form, &dir).unwrap().commit().unwrap();

        assert_eq!(vec!["freq.zip", "library.json", "terms.zip"], files_in(&dir));
    }

    /// A failed Apply must leave the Library unchanged.
    #[test]
    fn a_rolled_back_apply_leaves_the_library_exactly_as_it_was() {
        let (dir, _guard) = stocked("rollback");
        let before = archives_in(&dir);
        let manifest = Library::load(&dir).unwrap();
        let mut form = form_for(&dir);
        assert!(form.stage_add(&fixture("terms.zip")).is_some());
        form.stage_remove("FixtureFreq");

        let pending = stage_into_library(&form, &dir).unwrap();
        pending.rollback().unwrap();

        assert_eq!(before, archives_in(&dir));
        assert!(!dir.join(".removed").exists());
        assert_eq!(manifest.entries, Library::load(&dir).unwrap().entries);
    }

    /// A retry must not import a second copy.
    #[test]
    fn retrying_a_failed_apply_never_imports_a_second_copy() {
        let (dir, _guard) = stocked("retry");
        let mut form = form_for(&dir);
        assert!(form.stage_add(&fixture("terms.zip")).is_some());

        for _ in 0..3 {
            stage_into_library(&form, &dir).unwrap().rollback().unwrap();
        }
        stage_into_library(&form, &dir).unwrap().commit().unwrap();

        assert_eq!(
            vec!["freq.zip", "library.json", "terms (2).zip", "terms.zip"],
            files_in(&dir)
        );
    }

    /// A form with no staged changes gives Apply no work.
    #[test]
    fn clearing_the_staged_list_leaves_nothing_for_apply_to_do() {
        let (dir, _guard) = stocked("clear");
        let mut form = form_for(&dir);
        assert!(form.stage_add(&fixture("terms.zip")).is_some());
        form.stage_remove("FixtureFreq");
        assert!(form.has_staged());

        form.clear_staged();

        assert!(!form.has_staged());
        assert!(form.staged_adds.is_empty());
        assert!(form.staged_removes.is_empty());
    }

    #[test]
    fn clear_staged_resets_freq_changed() {
        let mut form = staged_form();
        form.freq_changed = true;
        form.clear_staged();
        assert!(!form.freq_changed);
    }

    #[test]
    fn the_frequency_list_a_window_opens_with_comes_from_the_library() {
        let (dir, _guard) = stocked("form");
        let form = form_for(&dir);
        assert_eq!(vec!["FixtureFreq".to_string()], names(&form.frequency));
        assert!(!form.library_empty);
        assert!(!form.has_staged());
    }

    #[test]
    fn a_library_that_is_not_there_yet_reads_as_empty() {
        let dir = std::env::temp_dir().join("chibipop_stage_test").join("never_created");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(form_for(&dir).library_empty);
    }
    /// Apply lists a Library term even when the database has no row for it.
    #[test]
    fn a_library_term_with_no_database_row_is_still_listed() {
        let lib = Library {
            entries: vec![crate::library::Entry {
                file: "dropped-in.zip".into(),
                name: "DroppedIn".into(),
                roles: Roles::only(&[Role::Terms]),
            }],
        };
        let form = with_library(from_resolved(&ResolvedConfig::default(), &[]), &lib);
        assert!(names(&form.terms).contains(&"DroppedIn".to_string()), "{form:?}");
    }

    // ---- incremental ----

    fn strs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    fn with_list(mut cfg: ResolvedConfig, lang: &str, list: &[&str]) -> ResolvedConfig {
        cfg.dictionaries.per_language.insert(lang.to_string(), strs(list));
        cfg
    }

    #[test]
    fn a_removed_dictionary_loses_its_entry_in_every_array() {
        let mut cfg = cfg_with(&["大辞林　第四版", "Jitendex.org"]);
        cfg.dictionaries.frequency_disabled = strs(&["大辞林　第四版"]);
        cfg.dictionaries.pitch = strs(&["大辞林　第四版", "NHK"]);
        dictionary_removed(&mut cfg, "大辞林　第四版");
        assert_eq!(strs(&["Jitendex.org"]), cfg.dictionaries.terms);
        assert!(cfg.dictionaries.frequency_disabled.is_empty());
        assert_eq!(strs(&["NHK"]), cfg.dictionaries.pitch);
    }

    #[test]
    fn a_removal_drops_the_name_from_every_language_list() {
        let mut cfg = with_list(
            with_list(
                cfg_with(&["大辞林　第四版", "Jitendex.org"]),
                "ja",
                &["大辞林　第四版", "Jitendex.org"],
            ),
            "zh-Hans-CN",
            &["大辞林　第四版", "中日大辞典"],
        );
        dictionary_removed(&mut cfg, "大辞林　第四版");
        assert_eq!(strs(&["Jitendex.org"]), cfg.dictionaries.per_language["ja"]);
        assert_eq!(strs(&["中日大辞典"]), cfg.dictionaries.per_language["zh-Hans-CN"]);
    }

    #[test]
    fn dictionary_removal_updates_all_explicit_profiles_and_keeps_empty_scopes() {
        let mut saved = Config::default();
        let mut parent = saved.resolve("default").unwrap();
        parent.dictionaries.terms.enabled = vec!["Gone".into()];
        parent.dictionaries.pitch.disabled = vec!["Gone".into()];
        parent.dictionaries.per_language.insert("ja".into(), vec!["Gone".into()]);
        saved.update_profile("default", &parent).unwrap();
        saved.dictionaries.frequency_disabled = vec!["Gone".into()];

        let mut other = ProfileSettings::default();
        other.dictionaries.terms.disabled = vec!["Gone".into()];
        other.dictionaries.per_language.insert("zh".into(), vec!["Gone".into()]);
        insert_full(&mut saved, "other", "Other", other);
        insert_derived(&mut saved, "inherited", "Inherited", "default", BTreeMap::new());
        let mut overrides = BTreeMap::new();
        overrides.insert(
            "dictionaries.terms".into(),
            FieldOverride::Set(toml::Value::try_from(&RoleList {
                enabled: Vec::new(),
                disabled: vec!["Gone".into()],
            }).unwrap()),
        );
        insert_derived(&mut saved, "explicit", "Explicit", "default", overrides);

        dictionary_removed_from_profiles(&mut saved, "Gone");
        assert!(saved.dictionaries.frequency_disabled.is_empty());
        for id in ["default", "other", "inherited", "explicit"] {
            let settings = saved.resolve(id).unwrap();
            assert!(!settings.dictionaries.terms.enabled.contains(&"Gone".into()));
            assert!(!settings.dictionaries.terms.disabled.contains(&"Gone".into()));
        }
        assert!(saved.resolve("default").unwrap().dictionaries.per_language["ja"].is_empty());
        assert!(saved.resolve("other").unwrap().dictionaries.per_language["zh"].is_empty());
    }

    /// Removing the last name keeps an explicit empty language scope.
    #[test]
    fn a_language_list_left_empty_keeps_its_key() {
        let mut cfg =
            with_list(cfg_with(&["大辞林　第四版", "Jitendex.org"]), "ja", &["大辞林　第四版"]);
        dictionary_removed(&mut cfg, "大辞林　第四版");
        assert_eq!(strs(&[]), cfg.dictionaries.per_language["ja"]);
    }

    /// An explicit empty language list enables no dictionaries and stays scoped.
    #[test]
    fn an_empty_language_list_never_falls_back_to_global_terms() {
        let blanked = with_list(cfg_with(&["Jitendex.org"]), "ja", &[]);
        let form = from_config(&blanked, &[]);
        assert!(is_scoped(&form));
        assert!(enabled_names(&form.terms).is_empty());
        assert_eq!(strs(&["Jitendex.org"]), names(&form.terms));
        let out = apply_resolved(&form, &blanked);
        assert!(is_scoped(&from_config(&out, &[])));
        assert!(out.dictionaries.per_language["ja"].is_empty());
    }

    /// Exact names limit removal to the named edition. The other edition remains.
    #[test]
    fn a_removal_leaves_the_other_edition_of_a_shared_name_alone() {
        let mut cfg = cfg_with(&["大辞林　第三版", "大辞林　第四版"]);
        dictionary_removed(&mut cfg, "大辞林　第四版");
        assert_eq!(strs(&["大辞林　第三版"]), cfg.dictionaries.terms);
    }

    #[test]
    fn a_removal_naming_no_entry_changes_nothing() {
        let before =
            with_list(cfg_with(&["大辞林　第四版", "Jitendex.org"]), "ja", &["大辞林　第四版"]);
        let mut cfg = before.clone();
        dictionary_removed(&mut cfg, "jiten_freq_global");
        assert_eq!(before, cfg);
    }

    #[test]
    fn removing_another_name_keeps_an_existing_empty_language_scope() {
        let mut cfg = with_list(cfg_with(&["Jitendex.org"]), "zh-Hans-CN", &[]);
        dictionary_removed(&mut cfg, "大辞林　第四版");
        assert!(cfg.dictionaries.per_language["zh-Hans-CN"].is_empty());
    }

    /// An added Dictionary goes to the bottom of every role list that its roles provide.
    #[test]
    fn an_added_dictionary_is_appended_to_each_of_its_role_lists() {
        let mut cfg = cfg_with(&["大辞林　第四版"]);
        dictionary_added(
            &mut cfg,
            "Jitendex.org [2026-07-09]",
            Roles::only(&[Role::Terms, Role::Frequency]),
        );
        assert_eq!(
            strs(&["大辞林　第四版", "Jitendex.org [2026-07-09]"]),
            cfg.dictionaries.terms,
        );
        assert_eq!(strs(&["Jitendex.org [2026-07-09]"]), cfg.dictionaries.frequency);
        assert!(cfg.dictionaries.pitch.is_empty(), "and no list it has no role for");
    }

    #[test]
    fn an_added_dictionary_joins_a_language_that_has_a_list() {
        let mut cfg = with_list(cfg_with(&["大辞林　第四版"]), "ja", &["大辞林　第四版"]);
        dictionary_added(&mut cfg, "Jitendex.org [2026-07-09]", Roles::only(&[Role::Terms]));
        assert_eq!(
            strs(&["大辞林　第四版", "Jitendex.org [2026-07-09]"]),
            cfg.dictionaries.per_language["ja"],
        );
    }

    /// `per_language` lists Dictionaries for term lookup. An archive without the
    /// Terms role does not belong in this list.
    #[test]
    fn an_added_frequency_dictionary_never_joins_a_language_list() {
        let mut cfg = with_list(cfg_with(&["大辞林　第四版"]), "ja", &["大辞林　第四版"]);
        dictionary_added(&mut cfg, "jiten_freq_global", Roles::only(&[Role::Frequency]));
        assert_eq!(strs(&["大辞林　第四版"]), cfg.dictionaries.per_language["ja"]);
    }

    /// Without a language entry, dictionary_added changes only the global lists.
    #[test]
    fn an_added_dictionary_never_creates_a_language_list() {
        let mut cfg = cfg_with(&["大辞林　第四版"]);
        dictionary_added(&mut cfg, "Jitendex.org [2026-07-09]", Roles::only(&[Role::Terms]));
        assert!(
            cfg.dictionaries.per_language.is_empty(),
            "creating an entry would pin the language: {:?}",
            cfg.dictionaries.per_language
        );
    }

    /// An import adds its terms Dictionary to an explicit empty language list.
    #[test]
    fn an_added_dictionary_fills_an_empty_language_list() {
        let mut cfg = with_list(cfg_with(&["大辞林　第四版"]), "ja", &[]);
        dictionary_added(&mut cfg, "Jitendex.org [2026-07-09]", Roles::only(&[Role::Terms]));
        assert_eq!(
            strs(&["Jitendex.org [2026-07-09]"]),
            cfg.dictionaries.per_language["ja"],
        );
    }

    #[test]
    fn an_added_dictionary_leaves_the_other_languages_alone() {
        let mut cfg = with_list(
            with_list(cfg_with(&["大辞林　第四版"]), "ja", &["大辞林　第四版"]),
            "zh-Hans-CN",
            &["中日大辞典"],
        );
        dictionary_added(&mut cfg, "Jitendex.org [2026-07-09]", Roles::only(&[Role::Terms]));
        assert_eq!(strs(&["中日大辞典"]), cfg.dictionaries.per_language["zh-Hans-CN"]);
    }

    /// A prior name stays in place, whether its row is enabled or disabled.
    #[test]
    fn a_name_an_array_already_holds_is_not_added_twice() {
        let mut cfg = with_list(cfg_with(&["Jitendex.org"]), "ja", &["Jitendex.org"]);
        cfg.dictionaries.pitch_disabled = strs(&["Jitendex.org"]);
        dictionary_added(
            &mut cfg,
            "Jitendex.org",
            Roles::only(&[Role::Terms, Role::Pitch]),
        );
        assert_eq!(strs(&["Jitendex.org"]), cfg.dictionaries.terms);
        assert_eq!(strs(&["Jitendex.org"]), cfg.dictionaries.pitch);
        assert!(cfg.dictionaries.pitch_disabled.is_empty());
        assert_eq!(strs(&["Jitendex.org"]), cfg.dictionaries.per_language["ja"]);
    }

    #[test]
    fn a_dictionary_with_no_title_adds_no_entry() {
        let mut cfg = with_list(cfg_with(&["大辞林　第四版"]), "ja", &["大辞林　第四版"]);
        dictionary_added(&mut cfg, "", Roles::only(&[Role::Terms]));
        assert_eq!(strs(&["大辞林　第四版"]), cfg.dictionaries.terms);
        assert_eq!(strs(&["大辞林　第四版"]), cfg.dictionaries.per_language["ja"]);
    }

    /// The incremental removal and full Apply must produce the same result.
    #[test]
    fn an_incremental_removal_agrees_with_a_full_apply() {
        let cfg = cfg_with(&["大辞林　第四版", "Jitendex.org [2026-07-09]"]);
        let kept = vec![DictInfo { dict_id: 1, name: "Jitendex.org [2026-07-09]".into() }];
        let mut form = from_resolved(&cfg, &kept);
        form.stage_remove("大辞林　第四版");
        let full = apply_resolved(&form, &cfg);
        let mut incremental = cfg.clone();
        dictionary_removed(&mut incremental, "大辞林　第四版");
        assert_eq!(full.dictionaries, incremental.dictionaries);
    }

    // ---- what a change costs ----

    /// Only frequency inputs affect `term.freq`, so only they need a Reindex.
    /// None of them needs a rebuild.
    #[test]
    fn reordering_the_frequency_list_costs_a_reindex() {
        let before = ResolvedConfig::default();
        let mut after = before.clone();
        after.dictionaries.frequency = strs(&["B", "A"]);
        assert_eq!(DictionaryWork::Reindex, dictionary_work(&before, &after));
    }

    #[test]
    fn toggling_a_frequency_checkbox_costs_a_reindex() {
        let mut before = ResolvedConfig::default();
        before.dictionaries.frequency = strs(&["A"]);
        let mut after = before.clone();
        after.dictionaries.frequency = Vec::new();
        after.dictionaries.frequency_disabled = strs(&["A"]);
        assert_eq!(DictionaryWork::Reindex, dictionary_work(&before, &after));
    }

    #[test]
    fn selecting_a_ranking_strategy_costs_a_reindex() {
        let before = ResolvedConfig::default();
        let mut after = before.clone();
        after.dictionaries.ranking_strategy = RankingStrategy::Median;
        assert_eq!(DictionaryWork::Reindex, dictionary_work(&before, &after));
    }

    #[test]
    fn reordering_or_toggling_terms_and_pitch_costs_nothing_extra() {
        let before = ResolvedConfig::default();
        let mut after = before.clone();
        after.dictionaries.terms = strs(&["B", "A"]);
        after.dictionaries.terms_disabled = strs(&["C"]);
        after.dictionaries.pitch = strs(&["NHK"]);
        after.dictionaries.pitch_disabled = strs(&["三省堂"]);
        after.popup.summary_chars = 55;
        assert_eq!(DictionaryWork::None, dictionary_work(&before, &after));
    }


    // ---- drift ----

    const LIB_DIR: &str = r"C:\chibipop\library";
    const DB_FILE: &str = r"C:\chibipop\data\chibipop.sqlite";

    /// `build.rs` writes JSON with `json.dumps` spaces.
    const BUILT_JSON: &str = concat!(
        r#"[{"name": "jitendex.zip", "bytes": 462, "sha256": "b1a8"}, "#,
        r#"{"name": "daijirin.zip", "bytes": 385, "sha256": "d49c"}, "#,
        r#"{"name": "freq.zip", "bytes": 12, "sha256": "0f0f"}]"#
    );

    /// `edit.rs` writes the same records with sorted keys and compact JSON.
    const EDITED_JSON: &str = concat!(
        r#"[{"bytes":462,"name":"jitendex.zip","sha256":"b1a8"},"#,
        r#"{"bytes":385,"name":"daijirin.zip","sha256":"d49c"},"#,
        r#"{"bytes":12,"name":"freq.zip","sha256":"0f0f"}]"#
    );

    fn notice(sources: &str, lib: &Library) -> Option<String> {
        drift_notice(Some(sources), lib, Path::new(LIB_DIR), Path::new(DB_FILE))
    }

    fn entry(file: &str, roles: &[Role]) -> crate::library::Entry {
        crate::library::Entry {
            file: file.into(),
            name: file.into(),
            roles: Roles::only(roles),
        }
    }

    #[test]
    fn a_library_that_matches_the_source_list_reports_no_drift() {
        assert_eq!(None, notice(EDITED_JSON, &library()));
    }

    /// Different spaces in JSON do not indicate archive drift.
    #[test]
    fn the_builders_own_json_reports_no_drift() {
        assert_ne!(BUILT_JSON, EDITED_JSON, "same bytes proves nothing");
        assert_eq!(None, notice(BUILT_JSON, &library()));
    }

    #[test]
    fn a_source_list_in_another_order_reports_no_drift() {
        let shuffled = concat!(
            r#"[{"bytes":12,"name":"freq.zip","sha256":"0f0f"},"#,
            r#"{"bytes":385,"name":"daijirin.zip","sha256":"d49c"},"#,
            r#"{"bytes":462,"name":"jitendex.zip","sha256":"b1a8"}]"#
        );
        assert_eq!(None, notice(shuffled, &library()));
    }

    /// An archive present in the Library but absent from the source list creates drift.
    #[test]
    fn an_archive_the_database_never_built_from_is_drift() {
        let mut lib = library();
        lib.entries.push(entry("dropped-in.zip", &[Role::Terms]));
        let text = notice(EDITED_JSON, &lib).expect("a dropped-in archive is drift");
        assert!(text.contains("dropped-in.zip"), "{text}");
    }

    /// An archive present in the source list but absent from the Library creates drift.
    #[test]
    fn an_archive_the_library_no_longer_has_is_drift() {
        let mut lib = library();
        lib.entries.retain(|e| e.file != "daijirin.zip");
        let text = notice(EDITED_JSON, &lib).expect("a deleted archive is drift");
        assert!(text.contains("daijirin.zip"), "{text}");
    }

    #[test]
    fn drift_names_each_side_in_its_own_list() {
        let mut lib = library();
        lib.entries.retain(|e| e.file != "daijirin.zip");
        lib.entries.push(entry("dropped-in.zip", &[Role::Terms]));
        let found = drift(Some(EDITED_JSON), &lib);
        assert_eq!(strs(&["dropped-in.zip"]), found.unbuilt);
        assert_eq!(strs(&["daijirin.zip"]), found.orphaned);
    }

    /// The build record excludes unreadable archives.
    #[test]
    fn an_unreadable_archive_is_not_drift() {
        let mut lib = library();
        lib.entries.push(entry("broken.zip", &[]));
        assert_eq!(None, notice(EDITED_JSON, &lib));
    }

    /// `write_meta` records a frequency archive like any other archive.
    #[test]
    fn a_frequency_archive_is_compared_like_any_other() {
        let partial = concat!(
            r#"[{"bytes":462,"name":"jitendex.zip","sha256":"b1a8"},"#,
            r#"{"bytes":385,"name":"daijirin.zip","sha256":"d49c"}]"#
        );
        let text = notice(partial, &library()).expect("freq.zip is in neither list");
        assert!(text.contains("freq.zip"), "{text}");
    }

    /// An absent source list does not report drift.
    #[test]
    fn a_database_with_no_source_list_reports_no_drift() {
        let none = drift_notice(None, &library(), Path::new(LIB_DIR), Path::new(DB_FILE));
        assert_eq!(None, none);
    }

    /// An unreadable source list does not report drift.
    #[test]
    fn a_source_list_that_will_not_parse_reports_no_drift() {
        assert_eq!(None, notice("not json at all", &library()));
    }

    /// A source record without a name does not report drift.
    #[test]
    fn a_source_record_with_no_name_is_ignored() {
        let odd = concat!(
            r#"[{"bytes":1},{"name":"jitendex.zip"},"#,
            r#"{"name":"daijirin.zip"},{"name":"freq.zip"}]"#
        );
        assert_eq!(None, notice(odd, &library()));
    }

    /// The notice includes the rebuild command with both supplied paths.
    #[test]
    fn the_notice_names_the_rebuild_command_with_real_paths() {
        let mut lib = library();
        lib.entries.retain(|e| e.file != "freq.zip");
        let text = notice(EDITED_JSON, &lib).unwrap();
        let command = "\r\nchibipop build-dict \
             --library \"C:\\chibipop\\library\" \
             --out \"C:\\chibipop\\data\\chibipop.sqlite\"";
        assert!(text.ends_with(command), "{text}");
    }

    /// A Library with no archive cannot offer a rebuild.
    #[test]
    fn a_library_with_no_archives_offers_no_rebuild() {
        assert_eq!(None, notice(EDITED_JSON, &Library::default()));
    }

    /// A Library with only frequency archives cannot offer a term rebuild.
    #[test]
    fn a_library_with_no_term_archive_offers_no_rebuild() {
        let lib = Library { entries: vec![entry("freq.zip", &[Role::Frequency])] };
        assert_eq!(None, notice(EDITED_JSON, &lib));
    }

    /// Drift still appears when no term archive can support a rebuild.
    #[test]
    fn drift_is_still_seen_when_no_rebuild_is_offered() {
        let found = drift(Some(EDITED_JSON), &Library::default());
        assert_eq!(strs(&["jitendex.zip", "daijirin.zip", "freq.zip"]), found.orphaned);
    }

    /// The notice lists only the side that differs.
    #[test]
    fn the_notice_omits_the_side_that_agrees() {
        let mut lib = library();
        lib.entries.retain(|e| e.file != "freq.zip");
        let text = notice(EDITED_JSON, &lib).unwrap();
        assert!(text.contains("no longer in your library: freq.zip"), "{text}");
        assert!(!text.contains("not in the database"), "{text}");
    }

}

#[cfg(test)]
mod search_settings_tests {
    use super::*;

    #[test]
    fn search_bind_can_clear_windows_chord_and_preserve_latest_linux_chord() {
        let mut latest = Config::default();
        let mut search = crate::config::Bind::new(
            "search".into(), crate::config::BindAction::Search,
        );
        search.windows = "F8".into();
        search.linux = "SUPER+F8".into();
        latest.binds.push(search);
        let mut form = super::from_config(&latest, &[]);
        form.catalog.binds.iter_mut()
            .find(|bind| bind.action == crate::config::BindAction::Search).unwrap()
            .windows.clear();
        latest.binds.iter_mut()
            .find(|bind| bind.action == crate::config::BindAction::Search).unwrap()
            .linux = "SUPER+F7".into();

        let saved = super::apply_to(&form, &latest).config;
        let search = saved.binds.iter()
            .find(|bind| bind.action == crate::config::BindAction::Search).unwrap();
        assert!(search.windows.is_empty());
        assert_eq!("SUPER+F7", search.linux);
    }
}
