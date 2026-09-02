//! The settings window's model.

use crate::config::{
    Config, FieldMapping, LayoutMode, OcrClipboardConfig, SentenceMode, TriggerMode,
};
use crate::dict::frequency::RankingStrategy;
use crate::library::{roles_of, Library, Pending, Role, Roles};
use crate::present::DictInfo;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Re-exported for config.rs.
pub use crate::config::{
    CAPTURE_H_RANGE, CAPTURE_W_RANGE, MAX_HEIGHT_RANGE, MAX_WIDTH_RANGE, PASSES_RANGE,
    SUMMARY_RANGE,
};

/// What the window edits.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsForm {
    pub mode: TriggerMode,
    pub trigger_key: String,
    pub theme: String,
    pub font: String,
    pub max_width_percent: u8,
    pub max_height_percent: u8,
    pub summary_chars: usize,
    pub highlight_match: bool,
    pub scroll_popup: bool,
    pub side_panel: bool,
    /// Compact or roomy.
    pub layout_mode: LayoutMode,
    pub dictionary_styling: bool,
    pub show_examples: bool,
    pub show_attributions: bool,
    pub show_images: bool,
    pub show_part_of_speech: bool,
    pub exclude_from_capture: bool,
    /// The terms Dictionary list: every Dictionary the config names or the
    /// library holds with that role, in priority order, each row carrying
    /// its own checkbox.
    ///
    /// One list per role and not one list plus an exclusion box: order and
    /// enabling are separate questions, so reordering can never silently
    /// exclude anything (ARCHITECTURE.md#dictionary-and-lookup).
    pub terms: Vec<DictRow>,
    /// The frequency Dictionary list. Position is the order
    /// [`RankingStrategy::Priority`] reads, and a checkbox here never
    /// touches the same Dictionary's row in another list.
    pub frequency: Vec<DictRow>,
    /// The pitch Dictionary list. Enabling a pitch Dictionary is the only
    /// switch pitch gets.
    pub pitch: Vec<DictRow>,
    /// The rule reducing the enabled frequency Dictionaries' Reported
    /// frequencies to one Frequency rank.
    pub ranking_strategy: RankingStrategy,
    pub dict_list_language: String,
    pub per_language: BTreeMap<String, Vec<String>>,
    pub max_ocr_passes: u8,
    pub prefer_vertical: bool,
    pub capture_width: i32,
    pub capture_height: i32,
    pub scan_alphanumeric: bool,
    pub per_character_lookup: bool,
    pub ocr_language: String,
    /// "builtin" or a plugin's name.
    pub engine: String,
    pub show_scan_region: bool,
    pub show_engine_log: bool,
    pub show_adapter_log: bool,
    pub freq_changed: bool,
    pub staged_adds: Vec<StagedAdd>,
    pub staged_removes: Vec<String>,
    pub library_empty: bool,
    /// Files nothing can read.
    ///
    /// Listed, never ordered.
    pub unreadable: Vec<String>,
    pub anki_enabled: bool,
    pub anki_url: String,
    pub anki_deck: String,
    pub anki_model: String,
    pub anki_add_key: String,
    /// `None`: this window has no answer about the field map, so Apply
    /// leaves the saved one alone. Windows fills its rows from a live
    /// AnkiConnect `modelFieldNames` call that answers with an empty
    /// vector whenever Anki is unreachable
    /// (`crates/chibipop-windows/src/app.rs:797-806`), and a window that
    /// was never told the field names must not wipe a good mapping.
    /// `Some(vec![])` is a user who mapped nothing, which is an answer
    /// and saves.
    pub field_map: Option<Vec<FieldMapping>>,
    pub notify_on_add: bool,
    pub sentence_mode: SentenceMode,
    pub static_region_key: String,
    pub show_static_overlay: bool,
    /// `None`: the action is off.
    pub ocr_clipboard_key: Option<String>,
    pub include_screenshot: bool,
    /// Only the top dict's entry.
    pub first_dict_only: bool,
    /// Plugin names allowed to run.
    pub enabled_plugins: Vec<String>,
}

/// One Dictionary's row in one role's list.
///
/// The name is the identity and the flag is that role's checkbox. Enabled
/// is per *role* and not per Dictionary: unchecking a mixed archive's
/// definitions must not silently kill its frequency data, so a checkbox
/// only ever affects the list it sits in
/// (ARCHITECTURE.md#dictionary-and-lookup).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictRow {
    pub name: String,
    pub enabled: bool,
}

/// An import waiting for Apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedAdd {
    pub source: PathBuf,
    /// The dictionary's own title.
    pub name: String,
}

impl SettingsForm {
    /// This role's list.
    pub fn list(&self, role: Role) -> &[DictRow] {
        match role {
            Role::Terms => &self.terms,
            Role::Frequency => &self.frequency,
            Role::Pitch => &self.pitch,
        }
    }

    /// This role's list, to reorder or to check.
    pub fn list_mut(&mut self, role: Role) -> &mut Vec<DictRow> {
        match role {
            Role::Terms => &mut self.terms,
            Role::Frequency => &mut self.frequency,
            Role::Pitch => &mut self.pitch,
        }
    }

    /// Stage an archive for import.
    ///
    /// It lands at the bottom of every list its role set names, enabled,
    /// and moves no row that is already there: installing a Dictionary
    /// never reorders the lists the user curated, and never silently does
    /// nothing.
    ///
    /// `None` when the archive is unreadable - an empty role set has no
    /// list to land in - or already staged.
    pub fn stage_add(&mut self, source: &Path) -> Option<Roles> {
        let roles = roles_of(source);
        if roles.is_empty() {
            return None;
        }
        let name = archive_title(source)?;
        // Titles repeat: split editions.
        if self.staged_adds.iter().any(|a| a.source == source) {
            return None;
        }
        for role in roles.iter() {
            self.list_mut(role).push(DictRow { name: name.clone(), enabled: true });
        }
        if roles.has(Role::Frequency) {
            self.freq_changed = true;
        }
        self.staged_adds.push(StagedAdd { source: source.to_path_buf(), name });
        Some(roles)
    }

    /// Stage a row for removal.
    ///
    /// Out of every list: one archive is one Dictionary, so removing it
    /// removes it from all three roles at once.
    pub fn stage_remove(&mut self, name: &str) {
        let was_freq = self.frequency.iter().any(|row| row.name == name);
        for role in Role::EVERY {
            self.list_mut(role).retain(|row| row.name != name);
        }
        let staged = self.staged_adds.len();
        self.staged_adds.retain(|a| a.name != name);
        // Never reached the library.
        if self.staged_adds.len() == staged && !self.staged_removes.iter().any(|n| n == name) {
            self.staged_removes.push(name.to_string());
        }
        if was_freq {
            self.freq_changed = true;
        }
    }

    /// Anything for the library.
    pub fn has_staged(&self) -> bool {
        !self.staged_adds.is_empty() || !self.staged_removes.is_empty()
    }

    /// Forget what Apply did.
    pub fn clear_staged(&mut self) {
        self.staged_adds.clear();
        self.staged_removes.clear();
        self.freq_changed = false;
    }

    /// Take what Apply wrote.
    pub fn reseed_per_language(&mut self, written: &BTreeMap<String, Vec<String>>) {
        self.per_language = written.clone();
    }

    /// Is this row a pending import?
    pub fn is_staged_add(&self, name: &str) -> bool {
        self.staged_adds.iter().any(|a| a.name == name)
    }
}

/// The dictionary's own title.
///
/// A filename would never match.
pub fn archive_title(source: &Path) -> Option<String> {
    let index = crate::dict::archive::read_index(source).ok()?;
    let title = index.get("title").and_then(|v| v.as_str()).filter(|t| !t.is_empty());
    match title {
        Some(t) => Some(t.to_string()),
        None => source.file_stem().map(|s| s.to_string_lossy().into_owned()),
    }
}

/// How an archive is listed.
pub fn shown_name(source: &Path) -> Option<String> {
    source.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// Does this window's terms list belong to the OCR language it names?
///
/// A per-language list is only rewritten when the rows on screen were drawn
/// for that language and the language already has one - anything else and
/// Apply would key one language's arrangement to another's tag.
pub fn is_scoped(form: &SettingsForm) -> bool {
    form.dict_list_language == form.ocr_language
        && form.per_language.contains_key(&form.ocr_language)
}

/// Show the library's lists.
///
/// The library is the authority on roles, because roles are read from an
/// archive's banks and the library is what holds archives. So a name the
/// library holds *without* a role leaves that role's list, a library
/// archive holding a role the list does not name lands at the bottom of it
/// enabled, and a name the library knows nothing about - an unplugged
/// drive, or a database built outside the app - is left exactly where the
/// config put it.
///
/// Unreadable files are listed in the terms list so they can still be
/// removed, which is the only reason an empty role set is listed at all.
pub fn with_library(mut form: SettingsForm, lib: &Library) -> SettingsForm {
    for role in Role::EVERY {
        let rows = form.list_mut(role);
        rows.retain(|row| {
            lib.entries.iter().find(|e| e.name == row.name).is_none_or(|e| e.roles.has(role))
        });
        for entry in lib.entries.iter().filter(|e| e.roles.has(role)) {
            if !rows.iter().any(|row| row.name == entry.name) {
                rows.push(DictRow { name: entry.name.clone(), enabled: true });
            }
        }
    }
    form.unreadable =
        lib.entries.iter().filter(|e| e.roles.is_empty()).map(|e| e.file.clone()).collect();
    for file in &form.unreadable {
        if !form.terms.iter().any(|row| row.name == *file) {
            form.terms.push(DictRow { name: file.clone(), enabled: false });
        }
    }
    form.library_empty = lib.is_empty();
    form
}

/// Files a removal names.
pub fn removed_files(form: &SettingsForm, lib: &Library) -> Vec<String> {
    form.staged_removes
        .iter()
        .filter_map(|name| lib.entries.iter().find(|e| &e.name == name || &e.file == name))
        .map(|e| e.file.clone())
        .collect()
}

/// Term archives Apply leaves.
///
/// Read, never asked of a list.
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

/// Do what Apply staged.
///
/// Refuses before it moves.
/// Reversible until commit.
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

/// Import, quarantine, save.
fn mutate(
    lib: &mut Library,
    pending: &mut Pending,
    dir: &Path,
    form: &SettingsForm,
    gone: &[String],
) -> Result<()> {
    // A source may live in `dir`.
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

/// One role's rows, as the config holds them plus whatever the database
/// has that no list names.
///
/// The config comes first because it is what the user arranged, and a name
/// it holds that names nothing installed stays as a row: keeping it on
/// screen is what keeps it in the file, so unplugging the drive a library
/// sits on cannot delete a list. `with_library` then corrects the roles.
fn rows_for(cfg: &Config, role: Role, dicts: &[DictInfo]) -> Vec<DictRow> {
    cfg.dictionaries
        .listed(role, dicts)
        .into_iter()
        .map(|(name, enabled)| DictRow { name, enabled })
        .collect()
}

/// The config's lists, plus the language's own terms scope where it has
/// one.
pub fn from_config(cfg: &Config, dicts: &[DictInfo]) -> SettingsForm {
    let mut terms = rows_for(cfg, Role::Terms, dicts);
    let frequency = rows_for(cfg, Role::Frequency, dicts);
    let pitch = rows_for(cfg, Role::Pitch, dicts);
    // A database built outside the app has dictionaries no list has ever
    // named and no library entry to read a role off, and the popup only
    // ever spends the terms list - so that is where they are listed, and
    // `with_library` moves them the moment the library holds the archive.
    for dict in dicts {
        let named = [&terms, &frequency, &pitch]
            .iter()
            .any(|rows| rows.iter().any(|row| row.name == dict.name));
        if !named {
            terms.push(DictRow { name: dict.name.clone(), enabled: true });
        }
    }
    // The language's own list is the terms arrangement this window edits, so
    // where it has one its rows are the checked ones, in its order, and
    // everything else trails behind them unchecked.
    if let Some(scope) = cfg.dictionaries.language_scope(&cfg.ocr.language, dicts) {
        let mut rows: Vec<DictRow> =
            scope.into_iter().map(|name| DictRow { name, enabled: true }).collect();
        for row in terms {
            if !rows.iter().any(|seen| seen.name == row.name) {
                rows.push(DictRow { name: row.name, enabled: false });
            }
        }
        terms = rows;
    }

    SettingsForm {
        mode: cfg.trigger.mode,
        trigger_key: cfg.trigger.trigger_key.clone(),
        theme: cfg.popup.theme.clone(),
        font: cfg.popup.font.clone(),
        max_width_percent: cfg.popup.max_width_percent,
        max_height_percent: cfg.popup.max_height_percent,
        summary_chars: cfg.popup.summary_chars,
        highlight_match: cfg.popup.highlight_match,
        scroll_popup: cfg.popup.scroll_popup,
        side_panel: cfg.popup.side_panel,
        layout_mode: cfg.popup.layout_mode,
        dictionary_styling: cfg.popup.dictionary_styling,
        show_examples: cfg.popup.show_examples,
        show_attributions: cfg.popup.show_attributions,
        show_images: cfg.popup.show_images,
        show_part_of_speech: cfg.popup.show_part_of_speech,
        exclude_from_capture: cfg.popup.exclude_from_capture,
        terms,
        frequency,
        pitch,
        ranking_strategy: cfg.dictionaries.ranking_strategy,
        dict_list_language: cfg.ocr.language.clone(),
        per_language: cfg.dictionaries.per_language.clone(),
        max_ocr_passes: cfg.ocr.max_ocr_passes,
        prefer_vertical: cfg.ocr.prefer_vertical,
        capture_width: cfg.ocr.capture_width,
        capture_height: cfg.ocr.capture_height,
        scan_alphanumeric: cfg.ocr.scan_alphanumeric,
        per_character_lookup: cfg.trigger.per_character_lookup,
        ocr_language: cfg.ocr.language.clone(),
        engine: cfg.ocr.engine.clone(),
        show_scan_region: cfg.debug.show_scan_region,
        show_engine_log: cfg.debug.show_engine_log,
        show_adapter_log: cfg.debug.show_adapter_log,
        freq_changed: false,
        staged_adds: Vec::new(),
        staged_removes: Vec::new(),
        library_empty: false,
        unreadable: Vec::new(),
        anki_enabled: cfg.anki.enabled,
        anki_url: cfg.anki.url.clone(),
        anki_deck: cfg.anki.deck.clone(),
        anki_model: cfg.anki.model.clone(),
        anki_add_key: cfg.anki.add_key.clone(),
        field_map: Some(cfg.anki.field_map.clone()),
        notify_on_add: cfg.anki.notify_on_add,
        sentence_mode: cfg.anki.sentence_mode,
        static_region_key: cfg.anki.static_region_key.clone(),
        show_static_overlay: cfg.anki.show_static_overlay,
        ocr_clipboard_key: cfg
            .actions
            .ocr_clipboard
            .as_ref()
            .and_then(|action| action.hotkey.clone()),
        include_screenshot: cfg.actions.screenshot.include_on_add,
        first_dict_only: cfg.anki.first_dict_only,
        enabled_plugins: cfg.plugins.enabled.clone(),
    }
}

/// The terms one language searches, out of the rows on screen.
///
/// The enabled rows, by exact name, in the order they sit in - an
/// unreadable file is a row so it can be removed and never a Dictionary to
/// search. `None` when that leaves nothing, because a language entry naming
/// no dictionary is indistinguishable from having no entry and the caller
/// should leave the saved one alone.
pub fn scoped_entry(rows: &[DictRow], unreadable: &[String]) -> Option<Vec<String>> {
    let named: Vec<String> = rows
        .iter()
        .filter(|row| row.enabled && !unreadable.contains(&row.name))
        .map(|row| row.name.clone())
        .collect();
    (!named.is_empty()).then_some(named)
}

/// The form, back onto the config it was drawn from.
pub fn apply_to(form: &SettingsForm, cfg: &Config) -> Config {
    let mut out = cfg.clone();
    out.trigger.mode = form.mode;
    out.trigger.trigger_key = form.trigger_key.clone();
    out.popup.theme = form.theme.clone();
    out.popup.font = form.font.clone();
    out.popup.max_width_percent =
        form.max_width_percent.clamp(MAX_WIDTH_RANGE.0, MAX_WIDTH_RANGE.1);
    out.popup.max_height_percent =
        form.max_height_percent.clamp(MAX_HEIGHT_RANGE.0, MAX_HEIGHT_RANGE.1);
    out.popup.summary_chars = form.summary_chars.clamp(SUMMARY_RANGE.0, SUMMARY_RANGE.1);
    out.popup.highlight_match = form.highlight_match;
    out.popup.scroll_popup = form.scroll_popup;
    out.popup.side_panel = form.side_panel;
    out.popup.layout_mode = form.layout_mode;
    out.popup.dictionary_styling = form.dictionary_styling;
    out.popup.show_examples = form.show_examples;
    out.popup.show_attributions = form.show_attributions;
    out.popup.show_images = form.show_images;
    out.popup.show_part_of_speech = form.show_part_of_speech;
    out.popup.exclude_from_capture = form.exclude_from_capture;
    out.ocr.max_ocr_passes = form.max_ocr_passes.clamp(PASSES_RANGE.0, PASSES_RANGE.1);
    out.ocr.prefer_vertical = form.prefer_vertical;
    out.ocr.capture_width = form.capture_width.clamp(CAPTURE_W_RANGE.0, CAPTURE_W_RANGE.1);
    out.ocr.capture_height = form.capture_height.clamp(CAPTURE_H_RANGE.0, CAPTURE_H_RANGE.1);
    out.ocr.scan_alphanumeric = form.scan_alphanumeric;
    out.trigger.per_character_lookup = form.per_character_lookup;
    out.ocr.language = form.ocr_language.clone();
    out.ocr.engine = form.engine.clone();
    out.debug.show_scan_region = form.show_scan_region;
    out.debug.show_engine_log = form.show_engine_log;
    out.debug.show_adapter_log = form.show_adapter_log;
    out.anki.enabled = form.anki_enabled;
    out.anki.url = form.anki_url.clone();
    out.anki.deck = form.anki_deck.clone();
    out.anki.model = form.anki_model.clone();
    out.anki.add_key = form.anki_add_key.clone();
    out.anki.notify_on_add = form.notify_on_add;
    if let Some(field_map) = &form.field_map {
        out.anki.field_map = field_map.clone();
    }
    out.anki.sentence_mode = form.sentence_mode;
    out.anki.static_region_key = form.static_region_key.clone();
    out.anki.show_static_overlay = form.show_static_overlay;
    out.anki.first_dict_only = form.first_dict_only;
    // The form renders the Windows chord only, so the Linux twin rides
    // through untouched - turning one off never deletes the other, and
    // the section survives while either is set.
    let hotkey_linux = cfg.actions.ocr_clipboard.as_ref().and_then(|a| a.hotkey_linux.clone());
    out.actions.ocr_clipboard = match (&form.ocr_clipboard_key, &hotkey_linux) {
        (None, None) => None,
        (hotkey, hotkey_linux) => Some(OcrClipboardConfig {
            hotkey: hotkey.clone(),
            hotkey_linux: hotkey_linux.clone(),
        }),
    };
    out.actions.screenshot.include_on_add = form.include_screenshot;
    out.plugins.enabled = form.enabled_plugins.clone();
    // Each list splits into its pair: the checked rows in the order they
    // sit in, then the unchecked ones. An unreadable file is a row so it
    // can be removed and is never a Dictionary, so it reaches neither
    // array - and `display_order` reaches nothing at all, which is what
    // retires it from the file on the first save after an upgrade.
    for role in Role::EVERY {
        let named = |enabled: bool| -> Vec<String> {
            form.list(role)
                .iter()
                .filter(|row| row.enabled == enabled)
                .map(|row| row.name.clone())
                .filter(|name| !form.unreadable.contains(name))
                .collect()
        };
        out.dictionaries.set_lists(role, named(true), named(false));
    }
    out.dictionaries.ranking_strategy = form.ranking_strategy;
    out.dictionaries.display_order.clear();

    let mut per_language = form.per_language.clone();
    if is_scoped(form) {
        if let Some(named) = scoped_entry(&form.terms, &form.unreadable) {
            per_language.insert(form.ocr_language.clone(), named);
        }
    }
    out.dictionaries.per_language = per_language;
    out
}

/// What `apply_to` had to move.
pub fn clamp_notice(form: &SettingsForm, applied: &Config) -> Option<String> {
    let mut parts = Vec::new();
    if form.capture_width != applied.ocr.capture_width {
        parts.push(axis_notice("width", form.capture_width, applied.ocr.capture_width));
    }
    if form.capture_height != applied.ocr.capture_height {
        parts.push(axis_notice("height", form.capture_height, applied.ocr.capture_height));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// The clamp notice's exact sentence.
fn axis_notice(axis: &str, asked: i32, got: i32) -> String {
    let (verb, bound) = if got > asked {
        ("raised", "minimum")
    } else {
        ("lowered", "maximum")
    };
    format!("Capture {axis} {verb} to the {got}px {bound}.")
}

/// Names no installed Dictionary answers to.
///
/// A list may legitimately hold one - an unplugged drive keeps its place -
/// but from inside the settings window it is also exactly what a renamed
/// archive looks like, so the window says so rather than silently ignoring
/// the entry.
pub fn stale_order_entries(cfg: &Config, dicts: &[DictInfo]) -> Vec<String> {
    let mut stale: Vec<String> = Vec::new();
    for role in Role::EVERY {
        let (on, off) = cfg.dictionaries.lists(role);
        for entry in on.iter().chain(off) {
            if !dicts.iter().any(|d| d.name == *entry) && !stale.contains(entry) {
                stale.push(entry.clone());
            }
        }
    }
    stale
}

/// Drop a removed Dictionary.
///
/// Out of all six arrays and every language list, because one archive is
/// one Dictionary and removing it removes every role it held. An emptied
/// language list is removed with it: an entry naming nothing is the same
/// state as having no entry.
pub fn dictionary_removed(cfg: &mut Config, name: &str) {
    for role in Role::EVERY {
        let (mut on, mut off) = {
            let (on, off) = cfg.dictionaries.lists(role);
            (on.to_vec(), off.to_vec())
        };
        on.retain(|entry| entry != name);
        off.retain(|entry| entry != name);
        cfg.dictionaries.set_lists(role, on, off);
    }
    cfg.dictionaries.per_language.retain(|_, list| {
        list.retain(|entry| entry != name);
        !list.is_empty()
    });
}

/// List an added Dictionary.
///
/// At the bottom of the enabled array of every role it holds, so an import
/// reorders nothing that was already arranged. The active language's list
/// gains it too where that language has one, because a term Dictionary the
/// language scope never names would arrive switched off.
pub fn dictionary_added(cfg: &mut Config, name: &str, roles: Roles) {
    if name.trim().is_empty() {
        return;
    }
    for role in roles.iter() {
        let (mut on, off) = {
            let (on, off) = cfg.dictionaries.lists(role);
            (on.to_vec(), off.to_vec())
        };
        if !on.iter().chain(&off).any(|entry| entry == name) {
            on.push(name.to_string());
        }
        cfg.dictionaries.set_lists(role, on, off);
    }
    if !roles.has(Role::Terms) {
        return;
    }
    let listed = cfg.dictionaries.per_language.get_mut(&cfg.ocr.language);
    if let Some(list) = listed.filter(|l| !l.is_empty()) {
        if !list.iter().any(|entry| entry == name) {
            list.push(name.to_string());
        }
    }
}

/// What applying a change costs beyond writing the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictionaryWork {
    /// Save and `reload`, and nothing else.
    None,
    /// Recompute every Frequency rank in place first.
    Reindex,
}

/// Does this change need a Reindex?
///
/// Exactly three inputs decide `term.freq`: which frequency Dictionaries
/// are enabled, the order they rank in, and the strategy that reduces them.
/// Change one and the ranks in the file were computed from something else;
/// change anything in the terms or pitch lists and they were not, because
/// neither role contributes a number to reduce
/// (ARCHITECTURE.md#dictionary-and-lookup).
///
/// The rule lives here so that neither settings window carries a copy of
/// it, and so that a Reindex is never mistaken for a rebuild: nothing here
/// reads an archive.
pub fn dictionary_work(before: &Config, after: &Config) -> DictionaryWork {
    let changed = before.dictionaries.frequency != after.dictionaries.frequency
        || before.dictionaries.frequency_disabled != after.dictionaries.frequency_disabled
        || before.dictionaries.ranking_strategy != after.dictionaries.ranking_strategy;
    if changed {
        DictionaryWork::Reindex
    } else {
        DictionaryWork::None
    }
}

/// Where the two sides differ.
#[derive(Debug, Default, PartialEq, Eq)]
struct Drift {
    /// In the library, never built.
    unbuilt: Vec<String>,
    /// Built from, now gone.
    orphaned: Vec<String>,
}

/// Library vs source_hashes.
///
/// Parsed: the bytes vary.
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

/// Names in one list only.
fn only_in(these: &[String], those: &[String]) -> Vec<String> {
    these.iter().filter(|name| !those.contains(name)).cloned().collect()
}

/// Offer a rebuild, or not.
///
/// CRLF: the box is an EDIT.
pub fn drift_notice(
    sources: Option<&str>,
    lib: &Library,
    dir: &Path,
    db: &Path,
) -> Option<String> {
    // build-dict needs a term zip.
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
    use crate::config::TriggerMode;

    fn dicts() -> Vec<DictInfo> {
        vec![
            DictInfo { dict_id: 1, name: "Jitendex.org [2026-07-09]".into() },
            DictInfo { dict_id: 2, name: "大辞林　第四版".into() },
        ]
    }

    /// A config already written in the role shape.
    fn cfg_with(terms: &[&str]) -> Config {
        let mut c = Config::default();
        c.dictionaries.terms = terms.iter().map(|s| (*s).to_string()).collect();
        c
    }

    /// A pre-roles config, carrying the substrings the role shape replaced.
    fn pre_roles(order: &[&str]) -> Config {
        let mut c = Config::default();
        c.dictionaries.display_order = order.iter().map(|s| (*s).to_string()).collect();
        c
    }

    fn names(rows: &[DictRow]) -> Vec<String> {
        rows.iter().map(|row| row.name.clone()).collect()
    }

    fn enabled_names(rows: &[DictRow]) -> Vec<String> {
        rows.iter().filter(|row| row.enabled).map(|row| row.name.clone()).collect()
    }

    #[test]
    fn the_form_lists_dictionaries_in_the_configured_order() {
        let form =
            from_config(&cfg_with(&["大辞林　第四版", "Jitendex.org [2026-07-09]"]), &dicts());
        assert_eq!(vec!["大辞林　第四版", "Jitendex.org [2026-07-09]"], names(&form.terms));
        assert!(form.terms.iter().all(|row| row.enabled));
    }

    #[test]
    fn an_unlisted_dictionary_is_listed_last() {
        let form = from_config(&cfg_with(&["大辞林　第四版"]), &dicts());
        assert_eq!(vec!["大辞林　第四版", "Jitendex.org [2026-07-09]"], names(&form.terms));
    }

    /// THE trap the substring model set: reordering used to rewrite a
    /// pattern, so moving one edition moved every dictionary it matched.
    /// Now a row is a name and a move is a move.
    #[test]
    fn reordering_writes_the_exact_names_in_their_new_order() {
        let cfg = cfg_with(&["大辞林　第四版", "Jitendex.org [2026-07-09]"]);
        let mut form = from_config(&cfg, &dicts());
        form.terms.reverse();
        let out = apply_to(&form, &cfg);
        assert_eq!(
            vec!["Jitendex.org [2026-07-09]".to_string(), "大辞林　第四版".to_string()],
            out.dictionaries.terms,
        );
        assert!(out.dictionaries.terms_disabled.is_empty());
    }

    /// The upgrade, end to end: a config still naming `大辞林` opens as the
    /// exact installed names it matched, and the first Apply writes them
    /// into the six arrays and retires the key.
    #[test]
    fn a_pre_roles_config_is_written_back_as_exact_names() {
        let cfg = pre_roles(&["大辞林", "Kenkyusha"]);
        let form = from_config(&cfg, &dicts());
        assert_eq!(
            vec!["大辞林　第四版", "Jitendex.org [2026-07-09]"],
            names(&form.terms),
            "the substring resolved, the one matching nothing was dropped, and the \
             dictionary no substring named landed at the bottom",
        );

        let out = apply_to(&form, &cfg);

        assert_eq!(
            vec!["大辞林　第四版".to_string(), "Jitendex.org [2026-07-09]".to_string()],
            out.dictionaries.terms,
        );
        let text = toml::to_string_pretty(&out).expect("the migrated config serialises");
        assert!(!text.contains("display_order"), "the retired key is gone: {text}");
        assert!(!text.contains("\"大辞林\""), "and so is the substring: {text}");
    }

    /// The Windows Apply pipeline renders its subset and carries the
    /// Linux platform fields untouched.
    #[test]
    fn applying_the_form_preserves_the_other_platforms_fields() {
        let mut cfg = cfg_with(&["大辞林　第四版", "Jitendex.org [2026-07-09]"]);
        cfg.trigger.trigger_key_linux = "SUPER+J".to_string();
        cfg.anki.add_key_linux = "SUPER+K".to_string();
        cfg.anki.static_region_key_linux = "SUPER+R".to_string();
        cfg.actions.screenshot.hotkey_linux = Some("SUPER+S".to_string());
        cfg.actions.ocr_clipboard = Some(OcrClipboardConfig {
            hotkey: Some("f9".into()),
            hotkey_linux: Some("SUPER+C".into()),
        });
        cfg.popup.layer = crate::config::PopupLayer::Top;
        let mut form = from_config(&cfg, &dicts());
        form.theme = "light".to_string();
        let out = apply_to(&form, &cfg);
        assert_eq!("SUPER+J", out.trigger.trigger_key_linux);
        assert_eq!("SUPER+K", out.anki.add_key_linux);
        assert_eq!("SUPER+R", out.anki.static_region_key_linux);
        assert_eq!(
            Some("SUPER+C".to_string()),
            out.actions.ocr_clipboard.as_ref().and_then(|a| a.hotkey_linux.clone())
        );
        assert_eq!(Some("SUPER+S".to_string()), out.actions.screenshot.hotkey_linux);
        assert_eq!(crate::config::PopupLayer::Top, out.popup.layer);
        assert_eq!("light", out.popup.theme);
    }

    /// The name written is the dictionary's own, edition and date and all,
    /// where the old key derivation cut everything from the first bracket
    /// so that two editions collapsed onto one entry.
    #[test]
    fn a_never_listed_dictionary_is_written_under_its_whole_name() {
        let cfg = cfg_with(&[]);
        let form = from_config(&cfg, &dicts());
        let out = apply_to(&form, &cfg);
        assert_eq!(
            vec!["Jitendex.org [2026-07-09]".to_string(), "大辞林　第四版".to_string()],
            out.dictionaries.terms,
        );
    }

    /// An unchecked row is written to the disabled twin, keeps its order
    /// within it, and is not searched.
    #[test]
    fn an_unchecked_row_lands_in_the_disabled_twin() {
        let cfg = cfg_with(&[]);
        let mut form = from_config(&cfg, &dicts());
        form.terms[0].enabled = false;
        let out = apply_to(&form, &cfg);
        assert_eq!(vec!["大辞林　第四版".to_string()], out.dictionaries.terms);
        assert_eq!(
            vec!["Jitendex.org [2026-07-09]".to_string()],
            out.dictionaries.terms_disabled,
        );
        assert_eq!(
            vec!["大辞林　第四版".to_string()],
            out.present_config(&dicts()).terms,
            "and the popup searches only what is checked",
        );
    }

    /// The whole point of exact names: one edition off, the other on.
    #[test]
    fn two_dictionaries_sharing_a_substring_are_enabled_independently() {
        let editions = vec![
            DictInfo { dict_id: 1, name: "大辞林　第三版".into() },
            DictInfo { dict_id: 2, name: "大辞林　第四版".into() },
        ];
        let cfg = Config::default();
        let mut form = from_config(&cfg, &editions);
        form.terms[0].enabled = false;
        let out = apply_to(&form, &cfg);
        assert_eq!(vec!["大辞林　第四版".to_string()], out.dictionaries.terms);
        assert_eq!(vec!["大辞林　第三版".to_string()], out.dictionaries.terms_disabled);
        assert_eq!(vec!["大辞林　第四版".to_string()], out.present_config(&editions).terms);
    }

    /// A rename looks like this.
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

    /// Reported stale in every role, because every array names Dictionaries
    /// the same way.
    #[test]
    fn a_stale_entry_is_reported_whichever_list_holds_it() {
        let mut cfg = Config::default();
        cfg.dictionaries.frequency = vec!["Gone".to_string()];
        cfg.dictionaries.pitch_disabled = vec!["Also gone".to_string()];
        assert_eq!(
            vec!["Gone".to_string(), "Also gone".to_string()],
            stale_order_entries(&cfg, &dicts()),
        );
    }

    /// Catches a bad control.
    #[test]
    fn an_untouched_form_round_trips_to_an_equal_config() {
        let cfg = cfg_with(&["大辞林　第四版", "Jitendex.org [2026-07-09]"]);
        let form = from_config(&cfg, &dicts());
        assert_eq!(cfg, apply_to(&form, &cfg));
    }

    #[test]
    fn every_setting_survives_the_round_trip() {
        let mut cfg = cfg_with(&["大辞林　第四版", "Jitendex.org [2026-07-09]"]);
        cfg.trigger.mode = TriggerMode::HoldKey;
        cfg.popup.theme = "light".into();
        cfg.popup.font = "Noto Sans JP".into();
        cfg.popup.max_width_percent = 35;
        cfg.popup.max_height_percent = 70;
        cfg.popup.summary_chars = 25;
        cfg.popup.highlight_match = false;
        cfg.popup.scroll_popup = false;
        cfg.popup.side_panel = true;
        cfg.popup.exclude_from_capture = true;
        cfg.popup.layout_mode = LayoutMode::Compact;
        cfg.popup.dictionary_styling = false;
        cfg.popup.show_examples = false;
        cfg.popup.show_attributions = false;
        cfg.popup.show_images = false;
        cfg.popup.show_part_of_speech = true;
        cfg.ocr.max_ocr_passes = 3;
        cfg.ocr.prefer_vertical = true;
        cfg.ocr.capture_width = 640;
        cfg.ocr.capture_height = 180;
        cfg.ocr.scan_alphanumeric = false;
        cfg.debug.show_scan_region = true;
        cfg.debug.show_engine_log = true;
        cfg.debug.show_adapter_log = true;
        cfg.anki.enabled = true;
        cfg.anki.url = "http://localhost:9999".into();
        cfg.anki.deck = "Mining".into();
        cfg.anki.model = "Custom".into();
        cfg.anki.add_key = "f2".into();
        cfg.anki.notify_on_add = false;
        let form = from_config(&cfg, &dicts());
        assert_eq!(cfg, apply_to(&form, &cfg));
    }

    /// Each render knob reaches the form and comes back, one at a time.
    ///
    /// One at a time rather than all six at once, because the failure
    /// this guards against is a field wired into `from_config` and
    /// forgotten in `apply_to` - which a single all-flipped case would
    /// catch only if every field were wrong the same way.
    #[test]
    fn every_render_knob_round_trips_through_the_form() {
        type Edit = fn(&mut Config);
        type Read = fn(&SettingsForm) -> bool;
        type Back = fn(&Config) -> bool;
        let cases: [(&str, Edit, Read, Back); 6] = [
            (
                "layout_mode",
                |c| c.popup.layout_mode = LayoutMode::Compact,
                |f| f.layout_mode == LayoutMode::Compact,
                |c| c.popup.layout_mode == LayoutMode::Compact,
            ),
            (
                "dictionary_styling",
                |c| c.popup.dictionary_styling = false,
                |f| !f.dictionary_styling,
                |c| !c.popup.dictionary_styling,
            ),
            (
                "show_examples",
                |c| c.popup.show_examples = false,
                |f| !f.show_examples,
                |c| !c.popup.show_examples,
            ),
            (
                "show_attributions",
                |c| c.popup.show_attributions = false,
                |f| !f.show_attributions,
                |c| !c.popup.show_attributions,
            ),
            (
                "show_images",
                |c| c.popup.show_images = false,
                |f| !f.show_images,
                |c| !c.popup.show_images,
            ),
            (
                "show_part_of_speech",
                |c| c.popup.show_part_of_speech = true,
                |f| f.show_part_of_speech,
                |c| c.popup.show_part_of_speech,
            ),
        ];
        for (field, edit, read, back) in cases {
            let mut cfg = cfg_with(&["大辞林"]);
            edit(&mut cfg);
            let form = from_config(&cfg, &dicts());
            assert!(read(&form), "{field} did not reach the form");
            assert!(back(&apply_to(&form, &cfg)), "{field} did not come back from the form");
        }
    }

    #[test]
    fn anki_add_key_round_trips_through_the_form() {
        let mut cfg = cfg_with(&["大辞林", "Jitendex"]);
        cfg.anki.add_key = "f2".into();
        let form = from_config(&cfg, &dicts());
        assert_eq!("f2", form.anki_add_key);
        assert_eq!("f2", apply_to(&form, &cfg).anki.add_key);
    }

    #[test]
    fn field_map_round_trips_through_the_form() {
        let mut cfg = cfg_with(&["大辞林", "Jitendex"]);
        cfg.anki.field_map = vec![FieldMapping {
            anki_field: "Front".into(),
            source: "expression".into(),
        }];
        let form = from_config(&cfg, &dicts());
        assert_eq!(Some(cfg.anki.field_map.clone()), form.field_map);
        assert_eq!(cfg.anki.field_map, apply_to(&form, &cfg).anki.field_map);
    }

    /// Untouched must not reset it.
    #[test]
    fn an_untouched_field_map_survives_apply() {
        let cfg = cfg_with(&[]);
        let form = from_config(&cfg, &dicts());
        assert_eq!(cfg.anki.field_map, apply_to(&form, &cfg).anki.field_map);
    }

    /// A user who removes every row is saying "map nothing": an answer,
    /// and a storable state.
    #[test]
    fn emptying_the_field_map_saves_an_empty_map() {
        let mut cfg = cfg_with(&[]);
        cfg.anki.field_map =
            vec![FieldMapping { anki_field: "Front".into(), source: "expression".into() }];
        let mut form = from_config(&cfg, &dicts());
        form.field_map = Some(Vec::new());
        assert!(
            apply_to(&form, &cfg).anki.field_map.is_empty(),
            "the user emptied the map; Apply must save that"
        );
    }

    /// A window whose rows never loaded has no answer, which is not the
    /// same value as an empty answer.
    #[test]
    fn a_window_with_nothing_to_say_cannot_wipe_the_field_map() {
        let mut cfg = cfg_with(&[]);
        cfg.anki.field_map =
            vec![FieldMapping { anki_field: "Front".into(), source: "expression".into() }];
        let mut form = from_config(&cfg, &dicts());
        form.field_map = None;
        assert_eq!(
            cfg.anki.field_map,
            apply_to(&form, &cfg).anki.field_map,
            "Anki was unreachable; Apply must leave the saved map alone"
        );
    }

    #[test]
    fn include_screenshot_round_trips() {
        let mut cfg = cfg_with(&[]);
        cfg.actions.screenshot.include_on_add = true;
        let form = from_config(&cfg, &dicts());
        assert!(form.include_screenshot);
        let out = apply_to(&form, &cfg);
        assert!(out.actions.screenshot.include_on_add);
    }

    #[test]
    fn include_on_add_defaults_to_false() {
        let cfg = Config::default();
        assert!(!cfg.actions.screenshot.include_on_add);
        let form = from_config(&cfg, &dicts());
        assert!(!form.include_screenshot);
    }

    #[test]
    fn include_screenshot_false_round_trips() {
        let mut cfg = cfg_with(&[]);
        cfg.actions.screenshot.include_on_add = false;
        let form = from_config(&cfg, &dicts());
        assert!(!form.include_screenshot);
        let out = apply_to(&form, &cfg);
        assert!(!out.actions.screenshot.include_on_add);
    }

    #[test]
    fn include_screenshot_does_not_touch_hotkey() {
        let mut cfg = cfg_with(&[]);
        cfg.actions.screenshot.hotkey = "f10".into();
        cfg.actions.screenshot.include_on_add = true;
        let form = from_config(&cfg, &dicts());
        let out = apply_to(&form, &cfg);
        assert_eq!("f10", out.actions.screenshot.hotkey);
    }

    #[test]
    fn ocr_clipboard_key_round_trips_through_the_form() {
        let mut cfg = cfg_with(&[]);
        cfg.actions.ocr_clipboard = Some(OcrClipboardConfig {
            hotkey: Some("f9".into()),
            hotkey_linux: None,
        });

        let form = from_config(&cfg, &dicts());
        assert_eq!(Some("f9".to_string()), form.ocr_clipboard_key);

        let out = apply_to(&form, &cfg);
        assert_eq!(
            Some("f9".to_string()),
            out.actions.ocr_clipboard.and_then(|a| a.hotkey)
        );
    }

    /// No section, no key: `None` all the way through, no sentinel.
    #[test]
    fn an_absent_ocr_clipboard_action_reads_as_none() {
        let cfg = cfg_with(&[]);
        assert_eq!(None, cfg.actions.ocr_clipboard);

        let form = from_config(&cfg, &dicts());
        assert_eq!(None, form.ocr_clipboard_key);
        assert_eq!(None, apply_to(&form, &cfg).actions.ocr_clipboard);
    }

    #[test]
    fn an_unset_ocr_clipboard_key_disables_the_action() {
        let mut cfg = cfg_with(&[]);
        cfg.actions.ocr_clipboard = Some(OcrClipboardConfig {
            hotkey: Some("f9".into()),
            hotkey_linux: None,
        });
        let mut form = from_config(&cfg, &dicts());
        form.ocr_clipboard_key = None;

        assert_eq!(None, apply_to(&form, &cfg).actions.ocr_clipboard);
    }

    /// Clearing the Windows chord must not evict the Linux one.
    #[test]
    fn an_unset_ocr_clipboard_key_keeps_the_linux_twin() {
        let mut cfg = cfg_with(&[]);
        cfg.actions.ocr_clipboard = Some(OcrClipboardConfig {
            hotkey: Some("f9".into()),
            hotkey_linux: Some("SUPER+C".into()),
        });
        let mut form = from_config(&cfg, &dicts());
        form.ocr_clipboard_key = None;

        assert_eq!(
            Some(OcrClipboardConfig { hotkey: None, hotkey_linux: Some("SUPER+C".into()) }),
            apply_to(&form, &cfg).actions.ocr_clipboard
        );
    }

    #[test]
    fn first_dict_only_defaults_to_false() {
        let cfg = Config::default();
        let form = from_config(&cfg, &dicts());
        assert!(!form.first_dict_only);
    }

    #[test]
    fn first_dict_only_round_trips() {
        let mut cfg = cfg_with(&[]);
        cfg.anki.first_dict_only = true;
        let form = from_config(&cfg, &dicts());
        assert!(form.first_dict_only);
        let out = apply_to(&form, &cfg);
        assert!(out.anki.first_dict_only);
    }

    #[test]
    fn sentence_mode_round_trips() {
        let mut cfg = cfg_with(&[]);
        cfg.anki.sentence_mode = SentenceMode::All;
        let form = from_config(&cfg, &dicts());
        assert_eq!(SentenceMode::All, form.sentence_mode);
        let out = apply_to(&form, &cfg);
        assert_eq!(SentenceMode::All, out.anki.sentence_mode);
    }

    #[test]
    fn sentence_mode_defaults_to_line_in_the_form() {
        let cfg = Config::default();
        let form = from_config(&cfg, &dicts());
        assert_eq!(SentenceMode::Line, form.sentence_mode);
    }

    #[test]
    fn static_region_key_round_trips_through_the_form() {
        let mut cfg = cfg_with(&[]);
        cfg.anki.static_region_key = "alt+r".to_string();
        let form = from_config(&cfg, &dicts());
        assert_eq!("alt+r", form.static_region_key);
        let out = apply_to(&form, &cfg);
        assert_eq!("alt+r", out.anki.static_region_key);
    }

    #[test]
    fn sentence_mode_static_round_trips() {
        let mut cfg = cfg_with(&[]);
        cfg.anki.sentence_mode = SentenceMode::Static;
        let form = from_config(&cfg, &dicts());
        assert_eq!(SentenceMode::Static, form.sentence_mode);
        let out = apply_to(&form, &cfg);
        assert_eq!(SentenceMode::Static, out.anki.sentence_mode);
    }

    #[test]
    fn out_of_range_numbers_are_clamped_not_rejected() {
        let cfg = cfg_with(&[]);
        let mut form = from_config(&cfg, &dicts());
        form.max_width_percent = 250;
        form.max_height_percent = 250;
        form.summary_chars = 1;
        form.max_ocr_passes = 99;
        let out = apply_to(&form, &cfg);
        assert_eq!(MAX_WIDTH_RANGE.1, out.popup.max_width_percent);
        assert_eq!(MAX_HEIGHT_RANGE.1, out.popup.max_height_percent);
        assert_eq!(SUMMARY_RANGE.0, out.popup.summary_chars);
        assert_eq!(PASSES_RANGE.1, out.ocr.max_ocr_passes);
    }

    #[test]
    fn apply_to_clamps_the_capture_size() {
        let cfg = Config::default();
        let mut form = from_config(&cfg, &dicts());
        form.capture_width = 99_999;
        form.capture_height = 1;
        let out = apply_to(&form, &cfg);
        assert_eq!(CAPTURE_W_RANGE.1, out.ocr.capture_width);
        assert_eq!(CAPTURE_H_RANGE.0, out.ocr.capture_height);
    }

    #[test]
    fn a_clamped_height_is_named_in_the_notice() {
        let cfg = Config::default();
        let mut form = from_config(&cfg, &dicts());
        form.capture_height = 1;
        let out = apply_to(&form, &cfg);
        assert_eq!(
            Some("Capture height raised to the 80px minimum.".to_string()),
            clamp_notice(&form, &out)
        );
    }

    #[test]
    fn a_clamped_width_is_named_with_its_ceiling() {
        let cfg = Config::default();
        let mut form = from_config(&cfg, &dicts());
        form.capture_width = 99_999;
        let out = apply_to(&form, &cfg);
        assert_eq!(
            Some("Capture width lowered to the 1600px maximum.".to_string()),
            clamp_notice(&form, &out)
        );
    }

    #[test]
    fn both_clamped_axes_are_both_reported() {
        let cfg = Config::default();
        let mut form = from_config(&cfg, &dicts());
        form.capture_width = 1;
        form.capture_height = 9_999;
        let notice = clamp_notice(&form, &apply_to(&form, &cfg)).expect("both were clamped");
        assert!(notice.contains("Capture width raised to the 100px minimum."), "{notice}");
        assert!(notice.contains("Capture height lowered to the 600px maximum."), "{notice}");
    }

    #[test]
    fn in_range_capture_values_produce_no_notice() {
        let cfg = Config::default();
        let mut form = from_config(&cfg, &dicts());
        form.capture_width = 500;
        form.capture_height = 220;
        assert_eq!(None, clamp_notice(&form, &apply_to(&form, &cfg)));
    }

    #[test]
    fn apply_to_carries_scan_alphanumeric() {
        let cfg = Config::default();
        let mut form = from_config(&cfg, &dicts());
        form.scan_alphanumeric = false;
        assert!(!apply_to(&form, &cfg).ocr.scan_alphanumeric);
    }

    #[test]
    fn apply_to_carries_per_character_lookup() {
        let cfg = Config::default();
        let mut form = from_config(&cfg, &dicts());
        form.per_character_lookup = true;
        assert!(apply_to(&form, &cfg).trigger.per_character_lookup);
    }

    #[test]
    fn apply_to_carries_the_ocr_language() {
        let cfg = Config::default();
        let mut form = from_config(&cfg, &dicts());
        form.ocr_language = "zh-Hans".to_string();
        assert_eq!("zh-Hans", apply_to(&form, &cfg).ocr.language);
    }

    /// The other direction, on open.
    #[test]
    fn from_config_seeds_per_character_lookup() {
        let mut cfg = Config::default();
        cfg.trigger.per_character_lookup = true;
        assert!(from_config(&cfg, &dicts()).per_character_lookup);
        assert!(
            !from_config(&Config::default(), &dicts()).per_character_lookup,
            "must default off"
        );
    }

    /// The other direction, on open.
    #[test]
    fn from_config_seeds_the_ocr_language() {
        let mut cfg = Config::default();
        cfg.ocr.language = "zh-Hans".to_string();
        assert_eq!("zh-Hans", from_config(&cfg, &dicts()).ocr_language);
        assert_eq!("ja", from_config(&Config::default(), &dicts()).ocr_language);
    }

    #[test]
    fn apply_to_carries_the_engine() {
        let cfg = Config::default();
        let mut form = from_config(&cfg, &dicts());
        form.engine = "meikiocr".to_string();
        assert_eq!("meikiocr", apply_to(&form, &cfg).ocr.engine);
    }

    /// The other direction, on open.
    #[test]
    fn from_config_seeds_the_engine() {
        let mut cfg = Config::default();
        cfg.ocr.engine = "meikiocr".to_string();
        assert_eq!("meikiocr", from_config(&cfg, &dicts()).engine);
        assert_eq!("builtin", from_config(&Config::default(), &dicts()).engine);
    }

    #[test]
    fn apply_to_carries_enabled_plugins() {
        let cfg = Config::default();
        let mut form = from_config(&cfg, &dicts());
        form.enabled_plugins = vec!["meikiocr".to_string()];
        assert_eq!(
            vec!["meikiocr".to_string()],
            apply_to(&form, &cfg).plugins.enabled
        );
    }

    /// The other direction, on open.
    #[test]
    fn from_config_seeds_enabled_plugins() {
        let mut cfg = Config::default();
        cfg.plugins.enabled = vec!["meikiocr".to_string()];
        assert_eq!(
            vec!["meikiocr".to_string()],
            from_config(&cfg, &dicts()).enabled_plugins
        );
        assert!(from_config(&Config::default(), &dicts()).enabled_plugins.is_empty());
    }

    #[test]
    fn from_config_puts_the_languages_own_terms_first_and_unchecks_the_rest() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries
            .per_language
            .insert("ja".to_string(), vec!["大辞林　第四版".to_string()]);
        let form = from_config(&cfg, &dicts());
        assert_eq!(vec!["大辞林　第四版".to_string()], enabled_names(&form.terms));
        assert!(form.terms.iter().any(|row| row.name.contains("Jitendex") && !row.enabled));
    }

    #[test]
    fn apply_to_writes_the_checked_rows_into_their_language() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries
            .per_language
            .insert("ja".to_string(), vec!["大辞林　第四版".to_string()]);
        let mut form = from_config(&cfg, &dicts());
        form.terms = vec![
            DictRow { name: "大辞林　第四版".to_string(), enabled: true },
            DictRow { name: "Jitendex.org [2026-07-09]".to_string(), enabled: false },
        ];
        let out = apply_to(&form, &cfg);
        assert_eq!(vec!["大辞林　第四版".to_string()], out.dictionaries.per_language["ja"]);
    }

    /// Others must survive.
    #[test]
    fn apply_to_preserves_other_languages() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        let mut form = from_config(&cfg, &dicts());
        form.per_language.insert("zh-Hans-CN".to_string(), vec!["中日大辞典".to_string()]);
        form.terms = vec![DictRow { name: "大辞林　第四版".to_string(), enabled: true }];
        let out = apply_to(&form, &cfg);
        assert_eq!(
            vec!["中日大辞典".to_string()],
            out.dictionaries.per_language["zh-Hans-CN"],
            "the other language's list must survive",
        );
    }

    /// Re-checking a row keeps the key and rewrites it, under the exact
    /// names the second Apply saw.
    #[test]
    fn a_second_apply_rewrites_the_key_the_first_one_wrote() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries
            .per_language
            .insert("ja".to_string(), vec!["大辞林　第四版".to_string()]);
        let mut form = from_config(&cfg, &dicts());
        form.terms = vec![
            DictRow { name: "大辞林　第四版".to_string(), enabled: true },
            DictRow { name: "Jitendex.org [2026-07-09]".to_string(), enabled: false },
        ];
        let first = apply_to(&form, &cfg);
        assert_eq!(vec!["大辞林　第四版".to_string()], first.dictionaries.per_language["ja"]);

        form.reseed_per_language(&first.dictionaries.per_language);
        form.terms[1].enabled = true;
        let second = apply_to(&form, &first);
        assert_eq!(
            vec!["大辞林　第四版".to_string(), "Jitendex.org [2026-07-09]".to_string()],
            second.dictionaries.per_language["ja"],
            "the second Apply must rewrite the key, never drop it",
        );
    }

    /// Merge must not undo scoping.
    #[test]
    fn with_library_keeps_the_exclusion_scoped() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries
            .per_language
            .insert("ja".to_string(), vec!["大辞林　第四版".to_string()]);
        let form = with_library(from_config(&cfg, &dicts()), &library());
        assert!(!enabled_names(&form.terms).iter().any(|n| n.contains("Jitendex")));
        assert!(form.terms.iter().any(|row| row.name.contains("Jitendex") && !row.enabled));
    }

    /// A stale list must not win.
    #[test]
    fn apply_to_does_not_write_a_stale_dict_list_language() {
        let mut cfg = Config::default();
        cfg.dictionaries
            .per_language
            .insert("zh-Hans-CN".to_string(), vec!["中日大辞典".to_string()]);
        let mut form = from_config(&cfg, &dicts());
        form.ocr_language = "zh-Hans-CN".to_string();
        let out = apply_to(&form, &cfg);
        assert_eq!(
            vec!["中日大辞典".to_string()],
            out.dictionaries.per_language["zh-Hans-CN"],
            "a stale dict list must not overwrite the real one",
        );
    }

    /// One helper, two writers.
    #[test]
    fn a_scoped_entry_naming_nothing_is_never_written() {
        let unreadable = ["bad.zip".to_string()];
        assert_eq!(None, scoped_entry(&[], &[]));
        assert_eq!(
            None,
            scoped_entry(&[DictRow { name: "bad.zip".into(), enabled: true }], &unreadable),
        );
        assert_eq!(
            None,
            scoped_entry(&[DictRow { name: "大辞林　第四版".into(), enabled: false }], &[]),
            "an unchecked row is not a dictionary this language searches",
        );
        assert_eq!(
            Some(vec!["大辞林　第四版".to_string()]),
            scoped_entry(&[DictRow { name: "大辞林　第四版".into(), enabled: true }], &[]),
        );
    }

    /// I2-a: nothing left checked.
    #[test]
    fn apply_to_will_not_erase_a_list_when_nothing_is_searched() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries
            .per_language
            .insert("ja".to_string(), vec!["大辞林　第四版".to_string()]);
        let mut form = from_config(&cfg, &dicts());
        for row in &mut form.terms {
            row.enabled = false;
        }
        let out = apply_to(&form, &cfg);
        assert_eq!(
            vec!["大辞林　第四版".to_string()],
            out.dictionaries.per_language["ja"],
            "an empty split must leave the entry alone",
        );
    }

    /// I2-b: only unreadable rows.
    #[test]
    fn apply_to_will_not_erase_a_list_for_an_unreadable_row() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries
            .per_language
            .insert("ja".to_string(), vec!["大辞林　第四版".to_string()]);
        let mut form = from_config(&cfg, &dicts());
        form.terms = vec![
            DictRow { name: "bad.zip".to_string(), enabled: true },
            DictRow { name: "Jitendex.org [2026-07-09]".to_string(), enabled: false },
        ];
        form.unreadable = vec!["bad.zip".to_string()];
        let out = apply_to(&form, &cfg);
        assert_eq!(
            vec!["大辞林　第四版".to_string()],
            out.dictionaries.per_language["ja"],
            "a split naming only an unreadable file must leave the entry alone",
        );
    }

    /// A language list naming nothing installed is still that language's
    /// list: the guard that used to throw it away and search everything was
    /// insurance against a substring, and an exact name cannot mistype its
    /// way into matching all of them.
    #[test]
    fn from_config_keeps_a_language_list_naming_nothing_installed() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries.per_language.insert("ja".to_string(), vec!["Daijirin".to_string()]);
        let form = from_config(&cfg, &dicts());
        assert_eq!(vec!["Daijirin".to_string()], enabled_names(&form.terms));
        assert!(
            form.terms.iter().filter(|row| !row.enabled).count() == 2,
            "and both installed dictionaries sit unchecked below it: {:?}",
            form.terms,
        );
    }

    #[test]
    fn from_config_still_splits_a_list_that_matches_one() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries
            .per_language
            .insert("ja".to_string(), vec!["大辞林　第四版".to_string()]);
        let form = from_config(&cfg, &dicts());
        assert_eq!(vec!["大辞林　第四版".to_string()], enabled_names(&form.terms));
        assert_eq!(
            vec!["Jitendex.org [2026-07-09]".to_string()],
            form.terms.iter().filter(|r| !r.enabled).map(|r| r.name.clone()).collect::<Vec<_>>(),
        );
    }

    fn staged_form() -> SettingsForm {
        from_config(&cfg_with(&["大辞林　第四版", "Jitendex.org [2026-07-09]"]), &dicts())
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
        let cfg = Config::default();
        let installed = vec![DictInfo { dict_id: 1, name: "Jitendex.org [2026]".into() }];
        let mut form = from_config(&cfg, &installed);
        form.stage_add(&fixture("terms.zip")).expect("a real archive stages");

        // User drags it to the top.
        let name = form.staged_adds[0].name.clone();
        form.terms.retain(|row| row.name != name);
        form.terms.insert(0, DictRow { name: name.clone(), enabled: true });

        let out = apply_to(&form, &cfg);

        assert_eq!(
            Some(&name),
            out.dictionaries.terms.first(),
            "the position the user chose must survive Apply",
        );
    }

    /// An import lands at the bottom of every list its roles name, enabled,
    /// and moves nothing that was already arranged.
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

    /// A pitch-only archive is in the Pitch list and in neither other one -
    /// the filename heuristic used to file it under terms.
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

        // Picked under Dictionaries.
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

    /// One archive is one Dictionary, so removing it removes every role it
    /// held rather than the one list the user happened to click in.
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

    /// Only the same file duplicates.
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
        let mut form = from_config(&cfg, &dicts());
        form.stage_remove("大辞林　第四版");
        assert_eq!(
            vec!["Jitendex.org [2026-07-09]".to_string()],
            apply_to(&form, &cfg).dictionaries.terms,
        );
    }

    /// A title orders, not a file.
    #[test]
    fn a_staged_add_is_ordered_by_its_title_not_its_filename() {
        let cfg = cfg_with(&["大辞林　第四版", "Jitendex.org [2026-07-09]"]);
        let mut form = from_config(&cfg, &dicts());
        assert_eq!(
            Some(Roles::only(&[Role::Terms])),
            form.stage_add(&fixture("terms.zip")),
        );
        let out = apply_to(&form, &cfg);
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

    /// An archive with two roles is one row in each of its two lists, and no
    /// row at all in the third.
    #[test]
    fn a_mixed_archive_appears_in_both_its_lists_once_each() {
        let lib = Library {
            entries: vec![crate::library::Entry {
                file: "mixed.zip".into(),
                name: "Mixed".into(),
                roles: Roles::only(&[Role::Terms, Role::Frequency]),
            }],
        };
        let form = with_library(from_config(&Config::default(), &[]), &lib);
        assert_eq!(vec!["Mixed".to_string()], names(&form.terms));
        assert_eq!(vec!["Mixed".to_string()], names(&form.frequency));
        assert!(form.pitch.is_empty());
    }

    /// The library is the authority on roles, so a name the config filed
    /// under a role the archive does not hold leaves that list. A name the
    /// library knows nothing about stays exactly where the config put it -
    /// that is what keeps an unplugged drive's entry alive.
    #[test]
    fn the_library_corrects_a_role_and_leaves_a_name_it_does_not_know() {
        let mut cfg = Config::default();
        cfg.dictionaries.pitch = vec!["jiten_freq_global".to_string()];
        cfg.dictionaries.terms = vec!["On the USB stick".to_string()];
        let form = with_library(from_config(&cfg, &[]), &library());
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

    /// A name, not a file.
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

    /// The existing-user case.
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

    /// Frequency alone is nothing to build definitions from.
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

    /// A path naming nothing.
    #[test]
    fn an_add_that_no_longer_exists_cannot_be_staged_at_all() {
        let mut form = with_library(staged_form(), &Library::default());
        form.terms.clear();
        assert_eq!(None, form.stage_add(Path::new(r"C:\gone\jmdict.zip")));
        assert_eq!(0, terms_after_apply(&form, &Library::default()));
    }

    /// Kept, not deleted: the row stays on screen, so Apply writes it back
    /// and the drive it names can be plugged in again.
    #[test]
    fn a_stale_entry_survives_an_apply_that_never_saw_its_dictionary() {
        let cfg = cfg_with(&["Kenkyusha", "大辞林　第四版"]);
        assert_eq!(vec!["Kenkyusha".to_string()], stale_order_entries(&cfg, &dicts()));
        let out = apply_to(&from_config(&cfg, &dicts()), &cfg);
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

    fn files_in(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    fn archives_in(dir: &Path) -> Vec<String> {
        files_in(dir).into_iter().filter(|n| n.ends_with(".zip")).collect()
    }

    fn form_for(dir: &Path) -> SettingsForm {
        let form = from_config(&Config::default(), &[]);
        match Library::load(dir) {
            Ok(lib) => with_library(form, &lib),
            Err(_) => form,
        }
    }

    /// The copy lands on disk under a free name, and the manifest then
    /// offers it as one dictionary rather than two.
    ///
    /// `stocked` already holds `terms.zip`, so staging the same fixture
    /// copies it in as `terms (2).zip`: a second file, byte-identical to
    /// the first. `Library::load` collapses the pair, because two names for
    /// one archive are one dictionary and building both would answer every
    /// headword twice. The file itself stays where it was put.
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

    /// Not before the rebuild.
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

    /// Removing un-stages the add.
    ///
    /// One title names one row.
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

    /// Refused before deleting.
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

    /// Frequency is not a dict.
    #[test]
    fn a_frequency_only_library_is_refused_too() {
        let (dir, _guard) = stocked("freq_only");
        let mut form = form_for(&dir);
        // Leaves frequency data only.
        form.stage_remove("FixtureTerms");

        assert!(stage_into_library(&form, &dir).is_err());
        assert!(dir.join("terms.zip").exists());
    }

    /// Ask the file, not the list.
    #[test]
    fn a_frequency_archive_added_under_dictionaries_supplies_no_terms() {
        let (dir, _guard) = stocked("misfiled");
        let before = files_in(&dir);
        let mut form = form_for(&dir);
        form.stage_remove("FixtureTerms");
        form.frequency.clear();
        // Routed by what its banks hold.
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

    /// Unreadable is not usable.
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
        let form = with_library(from_config(&cfg, &dicts()), &Library::load(&dir).unwrap());

        let out = apply_to(&form, &cfg);

        for role in Role::EVERY {
            let (on, off) = out.dictionaries.lists(role);
            assert!(
                !on.iter().chain(off).any(|e| e.contains("broken")),
                "{role:?}: {on:?} {off:?}",
            );
        }
    }

    /// It must be removable.
    #[test]
    fn removing_an_unreadable_row_quarantines_the_file_it_names() {
        let (dir, _guard) = stocked("unreadable_remove");
        std::fs::write(dir.join("broken.zip"), b"not a zip at all").unwrap();
        let mut form = form_for(&dir);
        form.stage_remove("broken.zip");

        stage_into_library(&form, &dir).unwrap().commit().unwrap();

        assert_eq!(vec!["freq.zip", "library.json", "terms.zip"], files_in(&dir));
    }

    /// A failed Apply costs none.
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

    /// No copies pile up on retry.
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

    /// Nothing left to replay.
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
    /// The list is what Apply builds.
    #[test]
    fn a_library_term_with_no_database_row_is_still_listed() {
        let lib = Library {
            entries: vec![crate::library::Entry {
                file: "dropped-in.zip".into(),
                name: "DroppedIn".into(),
                roles: Roles::only(&[Role::Terms]),
            }],
        };
        let form = with_library(from_config(&Config::default(), &[]), &lib);
        assert!(names(&form.terms).contains(&"DroppedIn".to_string()), "{form:?}");
    }

    // ---- incremental ----

    fn strs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    fn with_list(mut cfg: Config, lang: &str, list: &[&str]) -> Config {
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

    /// An entry naming nothing is the same state as no entry.
    #[test]
    fn a_language_list_left_empty_loses_its_key() {
        let mut cfg =
            with_list(cfg_with(&["大辞林　第四版", "Jitendex.org"]), "ja", &["大辞林　第四版"]);
        dictionary_removed(&mut cfg, "大辞林　第四版");
        assert!(
            !cfg.dictionaries.per_language.contains_key("ja"),
            "an emptied entry must be removed, not written as []: {:?}",
            cfg.dictionaries.per_language
        );
    }

    /// Why `[]` is not written.
    #[test]
    fn an_empty_list_left_behind_would_scope_the_language() {
        let mut cfg = with_list(cfg_with(&["Jitendex.org"]), "ja", &["大辞林　第四版"]);
        dictionary_removed(&mut cfg, "大辞林　第四版");
        assert!(!is_scoped(&from_config(&cfg, &[])), "the language must be unscoped again");

        let blanked = with_list(cfg_with(&["Jitendex.org"]), "ja", &[]);
        assert!(
            is_scoped(&from_config(&blanked, &[])),
            "[] keeps the language scoped and pins it at the next Apply",
        );
    }

    /// The substring model's worst failure, gone: a removal touches the
    /// name it was given and no other.
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
    fn a_list_that_was_already_empty_is_removed_too() {
        let mut cfg = with_list(cfg_with(&["Jitendex.org"]), "zh-Hans-CN", &[]);
        dictionary_removed(&mut cfg, "大辞林　第四版");
        assert!(cfg.dictionaries.per_language.is_empty());
    }

    /// The bottom of every list its roles name, under its whole name.
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

    /// `per_language` answers a question about definitions, so an archive
    /// with no terms role has no business in one.
    #[test]
    fn an_added_frequency_dictionary_never_joins_a_language_list() {
        let mut cfg = with_list(cfg_with(&["大辞林　第四版"]), "ja", &["大辞林　第四版"]);
        dictionary_added(&mut cfg, "jiten_freq_global", Roles::only(&[Role::Frequency]));
        assert_eq!(strs(&["大辞林　第四版"]), cfg.dictionaries.per_language["ja"]);
    }

    /// No entry: the global list decides.
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

    /// `[]` is not a list.
    #[test]
    fn an_added_dictionary_never_fills_an_empty_language_list() {
        let mut cfg = with_list(cfg_with(&["大辞林　第四版"]), "ja", &[]);
        dictionary_added(&mut cfg, "Jitendex.org [2026-07-09]", Roles::only(&[Role::Terms]));
        assert!(
            cfg.dictionaries.per_language["ja"].is_empty(),
            "filling [] would scope ja to the new dictionary alone",
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

    /// It is already listed, checked or not.
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
        assert!(cfg.dictionaries.pitch.is_empty(), "an unchecked row stays unchecked");
        assert_eq!(strs(&["Jitendex.org"]), cfg.dictionaries.per_language["ja"]);
    }

    #[test]
    fn a_dictionary_with_no_title_adds_no_entry() {
        let mut cfg = with_list(cfg_with(&["大辞林　第四版"]), "ja", &["大辞林　第四版"]);
        dictionary_added(&mut cfg, "", Roles::only(&[Role::Terms]));
        assert_eq!(strs(&["大辞林　第四版"]), cfg.dictionaries.terms);
        assert_eq!(strs(&["大辞林　第四版"]), cfg.dictionaries.per_language["ja"]);
    }

    /// Two writers, one result.
    #[test]
    fn an_incremental_removal_agrees_with_a_full_apply() {
        let cfg = cfg_with(&["大辞林　第四版", "Jitendex.org [2026-07-09]"]);
        let kept = vec![DictInfo { dict_id: 1, name: "Jitendex.org [2026-07-09]".into() }];
        let mut form = from_config(&cfg, &kept);
        form.stage_remove("大辞林　第四版");
        let full = apply_to(&form, &cfg);
        let mut incremental = cfg.clone();
        dictionary_removed(&mut incremental, "大辞林　第四版");
        assert_eq!(full.dictionaries, incremental.dictionaries);
    }

    // ---- what applying a change costs ----

    /// Only the frequency inputs reduce to `term.freq`, so only they cost a
    /// Reindex - and none of them costs a rebuild.
    #[test]
    fn reordering_the_frequency_list_costs_a_reindex() {
        let before = Config::default();
        let mut after = before.clone();
        after.dictionaries.frequency = strs(&["B", "A"]);
        assert_eq!(DictionaryWork::Reindex, dictionary_work(&before, &after));
    }

    #[test]
    fn toggling_a_frequency_checkbox_costs_a_reindex() {
        let mut before = Config::default();
        before.dictionaries.frequency = strs(&["A"]);
        let mut after = before.clone();
        after.dictionaries.frequency = Vec::new();
        after.dictionaries.frequency_disabled = strs(&["A"]);
        assert_eq!(DictionaryWork::Reindex, dictionary_work(&before, &after));
    }

    #[test]
    fn selecting_a_ranking_strategy_costs_a_reindex() {
        let before = Config::default();
        let mut after = before.clone();
        after.dictionaries.ranking_strategy = RankingStrategy::Median;
        assert_eq!(DictionaryWork::Reindex, dictionary_work(&before, &after));
    }

    #[test]
    fn reordering_or_toggling_terms_and_pitch_costs_nothing_extra() {
        let before = Config::default();
        let mut after = before.clone();
        after.dictionaries.terms = strs(&["B", "A"]);
        after.dictionaries.terms_disabled = strs(&["C"]);
        after.dictionaries.pitch = strs(&["NHK"]);
        after.dictionaries.pitch_disabled = strs(&["三省堂"]);
        after.popup.summary_chars = 55;
        assert_eq!(DictionaryWork::None, dictionary_work(&before, &after));
    }

    /// Two writers, one result.
    #[test]
    fn an_incremental_addition_agrees_with_a_full_apply() {
        let cfg = cfg_with(&["大辞林　第四版"]);
        let full = apply_to(&from_config(&cfg, &dicts()), &cfg);
        let mut incremental = cfg.clone();
        dictionary_added(
            &mut incremental,
            "Jitendex.org [2026-07-09]",
            Roles::only(&[Role::Terms]),
        );
        assert_eq!(full.dictionaries, incremental.dictionaries);
    }

    // ---- drift ----

    const LIB_DIR: &str = r"C:\chibipop\library";
    const DB_FILE: &str = r"C:\chibipop\data\chibipop.sqlite";

    /// build.rs, json.dumps spacing.
    const BUILT_JSON: &str = concat!(
        r#"[{"name": "jitendex.zip", "bytes": 462, "sha256": "b1a8"}, "#,
        r#"{"name": "daijirin.zip", "bytes": 385, "sha256": "d49c"}, "#,
        r#"{"name": "freq.zip", "bytes": 12, "sha256": "0f0f"}]"#
    );

    /// edit.rs, keys sorted.
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

    /// THE trap: different bytes.
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

    /// A hand-dropped archive.
    #[test]
    fn an_archive_the_database_never_built_from_is_drift() {
        let mut lib = library();
        lib.entries.push(entry("dropped-in.zip", &[Role::Terms]));
        let text = notice(EDITED_JSON, &lib).expect("a dropped-in archive is drift");
        assert!(text.contains("dropped-in.zip"), "{text}");
    }

    /// A hand-deleted archive.
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

    /// The builder skips these.
    #[test]
    fn an_unreadable_archive_is_not_drift() {
        let mut lib = library();
        lib.entries.push(entry("broken.zip", &[]));
        assert_eq!(None, notice(EDITED_JSON, &lib));
    }

    /// write_meta hashes freqs.
    #[test]
    fn a_frequency_archive_is_compared_like_any_other() {
        let partial = concat!(
            r#"[{"bytes":462,"name":"jitendex.zip","sha256":"b1a8"},"#,
            r#"{"bytes":385,"name":"daijirin.zip","sha256":"d49c"}]"#
        );
        let text = notice(partial, &library()).expect("freq.zip is in neither list");
        assert!(text.contains("freq.zip"), "{text}");
    }

    /// Absence is not disagreement.
    #[test]
    fn a_database_with_no_source_list_reports_no_drift() {
        let none = drift_notice(None, &library(), Path::new(LIB_DIR), Path::new(DB_FILE));
        assert_eq!(None, none);
    }

    /// Legacy files must not fail.
    #[test]
    fn a_source_list_that_will_not_parse_reports_no_drift() {
        assert_eq!(None, notice("not json at all", &library()));
    }

    /// An unfamiliar record shape.
    #[test]
    fn a_source_record_with_no_name_is_ignored() {
        let odd = concat!(
            r#"[{"bytes":1},{"name":"jitendex.zip"},"#,
            r#"{"name":"daijirin.zip"},{"name":"freq.zip"}]"#
        );
        assert_eq!(None, notice(odd, &library()));
    }

    /// Task 7's precedent.
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

    /// build-dict would refuse.
    #[test]
    fn a_library_with_no_archives_offers_no_rebuild() {
        assert_eq!(None, notice(EDITED_JSON, &Library::default()));
    }

    /// Frequency builds nothing.
    #[test]
    fn a_library_with_no_term_archive_offers_no_rebuild() {
        let lib = Library { entries: vec![entry("freq.zip", &[Role::Frequency])] };
        assert_eq!(None, notice(EDITED_JSON, &lib));
    }

    /// The offer, not the fact.
    #[test]
    fn drift_is_still_seen_when_no_rebuild_is_offered() {
        let found = drift(Some(EDITED_JSON), &Library::default());
        assert_eq!(strs(&["jitendex.zip", "daijirin.zip", "freq.zip"]), found.orphaned);
    }

    /// One side, one sentence.
    #[test]
    fn the_notice_omits_the_side_that_agrees() {
        let mut lib = library();
        lib.entries.retain(|e| e.file != "freq.zip");
        let text = notice(EDITED_JSON, &lib).unwrap();
        assert!(text.contains("no longer in your library: freq.zip"), "{text}");
        assert!(!text.contains("not in the database"), "{text}");
    }
}

