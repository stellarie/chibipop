//! The settings window's model.

use crate::config::{Config, FieldMapping, TriggerMode};
use crate::library::{kind_of, Kind, Library, Pending};
use crate::present::{dict_order_rank, DictInfo};
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
    pub exclude_from_capture: bool,
    /// Live names, not substrings.
    pub dict_names: Vec<String>,
    pub dict_excluded: Vec<String>,
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
    pub freq_names: Vec<String>,
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
    pub field_map: Vec<FieldMapping>,
    pub notify_on_add: bool,
    pub sentence_mode: String,
    pub static_region_key: String,
    pub include_screenshot: bool,
    /// Plugin names allowed to run.
    pub enabled_plugins: Vec<String>,
}

/// An import waiting for Apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedAdd {
    pub source: PathBuf,
    /// The dictionary's own title.
    pub name: String,
}

impl SettingsForm {
    /// Stage an archive for import.
    ///
    /// None when unreadable or taken.
    pub fn stage_add(&mut self, source: &Path) -> Option<Kind> {
        let kind = kind_of(source);
        if kind == Kind::Unreadable {
            return None;
        }
        let name = archive_title(source)?;
        // Titles repeat: split editions.
        if self.staged_adds.iter().any(|a| a.source == source) {
            return None;
        }
        if kind == Kind::Frequency {
            self.freq_names.push(name.clone());
        } else {
            self.dict_names.push(name.clone());
        }
        self.staged_adds.push(StagedAdd { source: source.to_path_buf(), name });
        Some(kind)
    }

    /// Stage a row for removal.
    pub fn stage_remove(&mut self, name: &str) {
        self.dict_names.retain(|n| n != name);
        self.freq_names.retain(|n| n != name);
        let staged = self.staged_adds.len();
        self.staged_adds.retain(|a| a.name != name);
        // Never reached the library.
        if self.staged_adds.len() == staged && !self.staged_removes.iter().any(|n| n == name) {
            self.staged_removes.push(name.to_string());
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

/// Split still trustworthy?
pub(crate) fn is_scoped(form: &SettingsForm) -> bool {
    form.dict_list_language == form.ocr_language
        && (form.per_language.contains_key(&form.ocr_language) || !form.dict_excluded.is_empty())
}

/// Show the library's lists.
///
/// Unreadable files are listed.
pub fn with_library(mut form: SettingsForm, lib: &Library) -> SettingsForm {
    form.freq_names = named(lib, Kind::Frequency);
    let to_excluded = is_scoped(&form);
    // Built first, then the rest.
    for name in named(lib, Kind::Term) {
        if form.dict_names.contains(&name) || form.dict_excluded.contains(&name) {
            continue;
        }
        if to_excluded {
            form.dict_excluded.push(name);
        } else {
            form.dict_names.push(name);
        }
    }
    form.unreadable =
        lib.entries.iter().filter(|e| e.kind == Kind::Unreadable).map(|e| e.file.clone()).collect();
    for file in &form.unreadable {
        if !form.dict_names.contains(file) {
            form.dict_names.push(file.clone());
        }
    }
    form.library_empty = lib.is_empty();
    form
}

/// One kind's names.
fn named(lib: &Library, kind: Kind) -> Vec<String> {
    lib.entries.iter().filter(|e| e.kind == kind).map(|e| e.name.clone()).collect()
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
        .filter(|e| e.kind == Kind::Term && !gone.contains(&e.file))
        .count();
    kept + form.staged_adds.iter().filter(|a| kind_of(&a.source) == Kind::Term).count()
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

fn sorted_by_order<'a>(dicts: &'a [DictInfo], order: &[String]) -> Vec<&'a DictInfo> {
    let mut ordered: Vec<&DictInfo> = dicts.iter().collect();
    ordered.sort_by_key(|d| (dict_order_rank(&d.name, order).unwrap_or(usize::MAX), d.dict_id));
    ordered
}

/// Flatten, in popup order.
pub fn from_config(cfg: &Config, dicts: &[DictInfo]) -> SettingsForm {
    let ordered = sorted_by_order(dicts, &cfg.dictionaries.display_order);
    let installed = || dicts.iter().map(|d| d.name.as_str());
    let active = cfg
        .dictionaries
        .per_language
        .get(&cfg.ocr.language)
        .filter(|l| !l.is_empty() && crate::present::any_listed(installed(), l.as_slice()));
    let (dict_names, dict_excluded) = match active {
        Some(list) => {
            let matching = sorted_by_order(dicts, list);
            (
                matching
                    .into_iter()
                    .filter(|d| dict_order_rank(&d.name, list).is_some())
                    .map(|d| d.name.clone())
                    .collect(),
                ordered
                    .iter()
                    .filter(|d| dict_order_rank(&d.name, list).is_none())
                    .map(|d| d.name.clone())
                    .collect(),
            )
        }
        None => (ordered.iter().map(|d| d.name.clone()).collect(), Vec::new()),
    };

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
        exclude_from_capture: cfg.popup.exclude_from_capture,
        dict_names,
        dict_excluded,
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
        freq_names: Vec::new(),
        staged_adds: Vec::new(),
        staged_removes: Vec::new(),
        library_empty: false,
        unreadable: Vec::new(),
        anki_enabled: cfg.anki.enabled,
        anki_url: cfg.anki.url.clone(),
        anki_deck: cfg.anki.deck.clone(),
        anki_model: cfg.anki.model.clone(),
        anki_add_key: cfg.anki.add_key.clone(),
        field_map: cfg.anki.field_map.clone(),
        notify_on_add: cfg.anki.notify_on_add,
        sentence_mode: cfg.anki.sentence_mode.clone(),
        static_region_key: cfg.anki.static_region_key.clone(),
        include_screenshot: cfg.actions.screenshot.include_on_add,
        enabled_plugins: cfg.plugins.enabled.clone(),
    }
}

fn keyed_names(names: &[String], unreadable: &[String], existing: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|name| !unreadable.iter().any(|u| u == *name))
        .map(|name| order_key(name, existing))
        .collect()
}

/// `None`: leave the entry be.
pub(crate) fn scoped_entry(
    names: &[String],
    unreadable: &[String],
    existing: &[String],
) -> Option<Vec<String>> {
    let keyed = keyed_names(names, unreadable, existing);
    (!keyed.is_empty()).then_some(keyed)
}

/// Substrings, not live names.
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
    if !form.field_map.is_empty() {
        out.anki.field_map = form.field_map.clone();
    }
    out.anki.sentence_mode = form.sentence_mode.clone();
    out.anki.static_region_key = form.static_region_key.clone();
    out.actions.screenshot.include_on_add = form.include_screenshot;
    out.plugins.enabled = form.enabled_plugins.clone();
    let full: Vec<String> =
        form.dict_names.iter().chain(form.dict_excluded.iter()).cloned().collect();
    out.dictionaries.display_order =
        keyed_names(&full, &form.unreadable, &cfg.dictionaries.display_order);

    let mut per_language = form.per_language.clone();
    if is_scoped(form) {
        let existing = cfg
            .dictionaries
            .per_language
            .get(&form.ocr_language)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        if let Some(keyed) = scoped_entry(&form.dict_names, &form.unreadable, existing) {
            per_language.insert(form.ocr_language.clone(), keyed);
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

/// Spec §7's exact sentence.
fn axis_notice(axis: &str, asked: i32, got: i32) -> String {
    let (verb, bound) = if got > asked {
        ("raised", "minimum")
    } else {
        ("lowered", "maximum")
    };
    format!("Capture {axis} {verb} to the {got}px {bound}.")
}

/// Keeps a matching entry.
fn order_key(name: &str, existing: &[String]) -> String {
    let lower = name.to_lowercase();
    // Blank matches everything.
    if let Some(entry) = existing
        .iter()
        .filter(|e| !e.trim().is_empty())
        .find(|e| lower.contains(&e.to_lowercase()))
    {
        return entry.clone();
    }
    let cut = name.find(['[', '(']).unwrap_or(name.len());
    let derived = name[..cut].trim();
    if derived.is_empty() { name.to_string() } else { derived.to_string() }
}

/// Entries matching nothing.
pub fn stale_order_entries(cfg: &Config, dicts: &[DictInfo]) -> Vec<String> {
    cfg.dictionaries
        .display_order
        .iter()
        .filter(|entry| {
            let needle = entry.to_lowercase();
            !dicts.iter().any(|d| d.name.to_lowercase().contains(&needle))
        })
        .cloned()
        .collect()
}

/// Drop a removed dictionary.
///
/// An emptied list is removed.
pub fn dictionary_removed(cfg: &mut Config, name: &str) {
    let claims = |entry: &String| dict_order_rank(name, std::slice::from_ref(entry)).is_some();
    cfg.dictionaries.display_order.retain(|entry| !claims(entry));
    cfg.dictionaries.per_language.retain(|_, list| {
        list.retain(|entry| !claims(entry));
        !list.is_empty()
    });
}

/// List an added dictionary.
///
/// Never makes a language list.
pub fn dictionary_added(cfg: &mut Config, name: &str) {
    if name.trim().is_empty() {
        return;
    }
    append_key(name, &mut cfg.dictionaries.display_order);
    let listed = cfg.dictionaries.per_language.get_mut(&cfg.ocr.language);
    if let Some(list) = listed.filter(|l| !l.is_empty()) {
        append_key(name, list);
    }
}

/// Only if nothing claims it.
fn append_key(name: &str, entries: &mut Vec<String>) {
    if dict_order_rank(name, entries).is_none() {
        let key = order_key(name, entries);
        entries.push(key);
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
        .filter(|e| e.kind != Kind::Unreadable)
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
    if !lib.entries.iter().any(|e| e.kind == Kind::Term) {
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

    fn cfg_with(order: &[&str]) -> Config {
        let mut c = Config::default();
        c.dictionaries.display_order = order.iter().map(|s| s.to_string()).collect();
        c
    }

    #[test]
    fn the_form_lists_dictionaries_in_the_configured_order() {
        let form = from_config(&cfg_with(&["大辞林", "Jitendex"]), &dicts());
        assert_eq!(vec!["大辞林　第四版", "Jitendex.org [2026-07-09]"], form.dict_names);
    }

    #[test]
    fn an_unordered_dictionary_is_listed_last() {
        let form = from_config(&cfg_with(&["大辞林"]), &dicts());
        assert_eq!(vec!["大辞林　第四版", "Jitendex.org [2026-07-09]"], form.dict_names);
    }

    /// THE trap: substrings.
    #[test]
    fn reordering_preserves_the_existing_substrings_verbatim() {
        let cfg = cfg_with(&["大辞林", "Jitendex"]);
        let mut form = from_config(&cfg, &dicts());
        form.dict_names.reverse();
        let out = apply_to(&form, &cfg);
        assert_eq!(
            vec!["Jitendex".to_string(), "大辞林".to_string()],
            out.dictionaries.display_order
        );
    }

    #[test]
    fn a_never_ordered_dictionary_derives_a_key_without_the_date() {
        let cfg = cfg_with(&[]);
        let form = from_config(&cfg, &dicts());
        let out = apply_to(&form, &cfg);
        assert!(
            out.dictionaries.display_order.contains(&"Jitendex.org".to_string()),
            "got {:?}",
            out.dictionaries.display_order
        );
        assert!(out.dictionaries.display_order.contains(&"大辞林　第四版".to_string()));
    }

    /// Empty key matches all.
    #[test]
    fn a_key_that_would_be_empty_falls_back_to_the_whole_name() {
        let cfg = cfg_with(&[]);
        let odd = vec![DictInfo { dict_id: 1, name: "[2026] Something".into() }];
        let form = from_config(&cfg, &odd);
        let out = apply_to(&form, &cfg);
        assert_eq!(vec!["[2026] Something".to_string()], out.dictionaries.display_order);
    }

    /// Blank matches every name.
    #[test]
    fn a_blank_order_entry_never_claims_a_dictionary() {
        let cfg = cfg_with(&["", "大辞林"]);
        let out = apply_to(&from_config(&cfg, &dicts()), &cfg);
        assert!(
            !out.dictionaries.display_order.iter().any(|e| e.trim().is_empty()),
            "a blank entry silently pins every dictionary, got {:?}",
            out.dictionaries.display_order
        );
        assert!(out.dictionaries.display_order.contains(&"Jitendex.org".to_string()));
    }

    /// A rename looks like this.
    #[test]
    fn an_entry_matching_no_dictionary_is_reported() {
        let stale = stale_order_entries(&cfg_with(&["大辞林", "Kenkyusha"]), &dicts());
        assert_eq!(vec!["Kenkyusha".to_string()], stale);
    }

    #[test]
    fn entries_that_all_match_report_nothing() {
        assert!(stale_order_entries(&cfg_with(&["大辞林", "Jitendex"]), &dicts()).is_empty());
    }

    /// Must agree with ranking.
    #[test]
    fn a_reported_entry_is_exactly_one_dict_order_rank_cannot_use() {
        let cfg = cfg_with(&["大辞林", "Kenkyusha", "Jitendex"]);
        for entry in stale_order_entries(&cfg, &dicts()) {
            for d in dicts() {
                assert_ne!(
                    Some(0),
                    dict_order_rank(&d.name, std::slice::from_ref(&entry)),
                    "{entry:?} was reported stale but does rank {:?}",
                    d.name
                );
            }
        }
    }

    /// Catches a bad control.
    #[test]
    fn an_untouched_form_round_trips_to_an_equal_config() {
        let cfg = cfg_with(&["大辞林", "Jitendex"]);
        let form = from_config(&cfg, &dicts());
        assert_eq!(cfg, apply_to(&form, &cfg));
    }

    #[test]
    fn every_setting_survives_the_round_trip() {
        let mut cfg = cfg_with(&["大辞林", "Jitendex"]);
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
        assert_eq!(cfg.anki.field_map, form.field_map);
        assert_eq!(cfg.anki.field_map, apply_to(&form, &cfg).anki.field_map);
    }

    /// Untouched must not reset it.
    #[test]
    fn an_untouched_field_map_survives_apply() {
        let cfg = cfg_with(&[]);
        let form = from_config(&cfg, &dicts());
        assert_eq!(cfg.anki.field_map, apply_to(&form, &cfg).anki.field_map);
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
    fn sentence_mode_round_trips() {
        let mut cfg = cfg_with(&[]);
        cfg.anki.sentence_mode = "all".to_string();
        let form = from_config(&cfg, &dicts());
        assert_eq!("all", form.sentence_mode);
        let out = apply_to(&form, &cfg);
        assert_eq!("all", out.anki.sentence_mode);
    }

    #[test]
    fn sentence_mode_defaults_to_line_in_the_form() {
        let cfg = Config::default();
        let form = from_config(&cfg, &dicts());
        assert_eq!("line", form.sentence_mode);
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
        cfg.anki.sentence_mode = "static".to_string();
        let form = from_config(&cfg, &dicts());
        assert_eq!("static", form.sentence_mode);
        let out = apply_to(&form, &cfg);
        assert_eq!("static", out.anki.sentence_mode);
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
    fn from_config_splits_active_from_excluded() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries.per_language.insert(
            "ja".to_string(), vec!["大辞林".to_string()]);
        let form = from_config(&cfg, &dicts());
        assert!(form.dict_names.iter().all(|n| n.contains("大辞林")));
        assert!(form.dict_excluded.iter().any(|n| n.contains("Jitendex")));
    }

    #[test]
    fn apply_to_writes_the_visible_list_into_its_language() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        let mut form = from_config(&cfg, &dicts());
        form.dict_names = vec!["大辞林　第四版".to_string()];
        form.dict_excluded = vec!["Jitendex.org [2026-07-09]".to_string()];
        let out = apply_to(&form, &cfg);
        assert_eq!(
            vec!["大辞林　第四版".to_string()],
            out.dictionaries.per_language["ja"],
        );
    }

    /// Others must survive.
    #[test]
    fn apply_to_preserves_other_languages() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        let mut form = from_config(&cfg, &dicts());
        form.per_language.insert(
            "zh-Hans-CN".to_string(), vec!["中日大辞典".to_string()]);
        form.dict_names = vec!["大辞林　第四版".to_string()];
        let out = apply_to(&form, &cfg);
        assert_eq!(
            vec!["中日大辞典".to_string()],
            out.dictionaries.per_language["zh-Hans-CN"],
            "the other language's list must survive",
        );
    }

    /// Re-include keeps the key.
    #[test]
    fn a_second_apply_rewrites_the_key_the_first_one_wrote() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        let mut form = from_config(&cfg, &dicts());
        form.dict_names = vec!["大辞林　第四版".to_string()];
        form.dict_excluded = vec!["Jitendex.org [2026-07-09]".to_string()];
        let first = apply_to(&form, &cfg);
        assert_eq!(
            vec!["大辞林　第四版".to_string()],
            first.dictionaries.per_language["ja"],
        );
        form.reseed_per_language(&first.dictionaries.per_language);
        form.dict_names =
            vec!["大辞林　第四版".to_string(), "Jitendex.org [2026-07-09]".to_string()];
        form.dict_excluded = Vec::new();
        let second = apply_to(&form, &first);
        assert_eq!(
            vec!["大辞林　第四版".to_string(), "Jitendex.org".to_string()],
            second.dictionaries.per_language["ja"],
            "the second Apply must rewrite the key, never drop it",
        );
    }

    /// Merge must not undo scoping.
    #[test]
    fn with_library_keeps_the_exclusion_scoped() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries.per_language.insert(
            "ja".to_string(), vec!["大辞林".to_string()]);
        let form = with_library(from_config(&cfg, &dicts()), &library());
        assert!(!form.dict_names.iter().any(|n| n.contains("Jitendex")));
        assert!(form.dict_excluded.iter().any(|n| n.contains("Jitendex")));
    }

    /// A stale list must not win.
    #[test]
    fn apply_to_does_not_write_a_stale_dict_list_language() {
        let mut cfg = Config::default();
        cfg.dictionaries.per_language.insert(
            "zh-Hans-CN".to_string(), vec!["中日大辞典".to_string()]);
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
    fn a_keyed_empty_entry_is_never_written() {
        assert_eq!(None, scoped_entry(&[], &[], &[]));
        assert_eq!(
            None,
            scoped_entry(&["bad.zip".to_string()], &["bad.zip".to_string()], &[]),
        );
        assert_eq!(
            Some(vec!["大辞林".to_string()]),
            scoped_entry(&["大辞林　第四版".to_string()], &[], &["大辞林".to_string()]),
        );
    }

    /// I2-a: nothing left searched.
    #[test]
    fn apply_to_will_not_erase_a_list_when_nothing_is_searched() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries.per_language.insert(
            "ja".to_string(), vec!["大辞林".to_string()]);
        let mut form = from_config(&cfg, &dicts());
        form.dict_names = Vec::new();
        form.dict_excluded = vec!["Jitendex.org [2026-07-09]".to_string()];
        let out = apply_to(&form, &cfg);
        assert_eq!(
            vec!["大辞林".to_string()],
            out.dictionaries.per_language["ja"],
            "an empty split must leave the entry alone",
        );
    }

    /// I2-b: only unreadable rows.
    #[test]
    fn apply_to_will_not_erase_a_list_for_an_unreadable_row() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries.per_language.insert(
            "ja".to_string(), vec!["大辞林".to_string()]);
        let mut form = from_config(&cfg, &dicts());
        form.dict_names = vec!["bad.zip".to_string()];
        form.dict_excluded = vec!["Jitendex.org [2026-07-09]".to_string()];
        form.unreadable = vec!["bad.zip".to_string()];
        let out = apply_to(&form, &cfg);
        assert_eq!(
            vec!["大辞林".to_string()],
            out.dictionaries.per_language["ja"],
            "a keyed-empty split must leave the entry alone",
        );
    }

    /// Matches nothing: search all.
    #[test]
    fn from_config_ignores_a_list_matching_nothing_installed() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries.per_language.insert(
            "ja".to_string(), vec!["Daijirin".to_string()]);
        let form = from_config(&cfg, &dicts());
        assert_eq!(
            vec!["大辞林　第四版".to_string(), "Jitendex.org [2026-07-09]".to_string()],
            form.dict_names,
            "the tab must match resolve_dict_filter's fallback",
        );
        assert!(form.dict_excluded.is_empty());
    }

    #[test]
    fn from_config_still_splits_a_list_that_matches_one() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries.per_language.insert(
            "ja".to_string(), vec!["大辞林".to_string()]);
        let form = from_config(&cfg, &dicts());
        assert_eq!(vec!["大辞林　第四版".to_string()], form.dict_names);
        assert_eq!(vec!["Jitendex.org [2026-07-09]".to_string()], form.dict_excluded);
    }

    fn staged_form() -> SettingsForm {
        from_config(&cfg_with(&["大辞林", "Jitendex"]), &dicts())
    }

    #[test]
    fn a_fresh_form_stages_nothing() {
        let form = staged_form();
        assert!(!form.has_staged());
        assert!(form.freq_names.is_empty());
        assert!(!form.library_empty);
    }

    #[test]
    fn staging_an_add_then_removing_it_is_a_no_op() {
        let mut form = staged_form();
        let before = form.dict_names.clone();
        assert_eq!(Some(Kind::Term), form.stage_add(&fixture("terms.zip")));
        assert_eq!(1, form.staged_adds.len());
        assert_eq!("FixtureTerms", form.staged_adds[0].name, "the title, not the filename");

        form.stage_remove("FixtureTerms");

        assert!(form.staged_adds.is_empty());
        assert!(form.staged_removes.is_empty(), "the library was never asked for it");
        assert_eq!(before, form.dict_names);
        assert!(!form.has_staged());
    }

    #[test]
    fn a_staged_add_keeps_the_position_the_user_gave_it() {
        let mut cfg = Config::default();
        cfg.dictionaries.display_order = vec!["Jitendex".to_string()];
        let installed = vec![DictInfo { dict_id: 1, name: "Jitendex.org [2026]".into() }];
        let mut form = from_config(&cfg, &installed);
        form.stage_add(&fixture("terms.zip")).expect("a real archive stages");

        // User drags it to the top.
        let name = form.staged_adds[0].name.clone();
        form.dict_names.retain(|n| n != &name);
        form.dict_names.insert(0, name.clone());

        let out = apply_to(&form, &cfg);

        assert_eq!(Some(&name), out.dictionaries.display_order.first(),
            "the position the user chose must survive Apply");
    }

    #[test]
    fn a_frequency_list_lands_in_frequency_whichever_button_was_used() {
        let mut form = staged_form();
        let dicts_before = form.dict_names.clone();

        // Picked under Dictionaries.
        assert_eq!(Some(Kind::Frequency), form.stage_add(&fixture("freq.zip")));

        assert_eq!(vec!["FixtureFreq".to_string()], form.freq_names);
        assert_eq!(dicts_before, form.dict_names, "it is not a dictionary");
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
        form.dict_names = vec!["a".into(), "b".into(), "c".into()];

        form.stage_remove("b");

        assert_eq!(vec!["a".to_string(), "c".to_string()], form.dict_names);
        assert_eq!(vec!["b".to_string()], form.staged_removes);
        assert!(form.has_staged());
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

        assert_eq!(Some(Kind::Term), form.stage_add(&a));
        assert_eq!(Some(Kind::Term), form.stage_add(&b), "a split edition shares its title");

        assert_eq!(2, form.staged_adds.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_same_file_cannot_be_staged_twice() {
        let mut form = staged_form();
        assert_eq!(Some(Kind::Term), form.stage_add(&fixture("terms.zip")));
        assert_eq!(None, form.stage_add(&fixture("terms.zip")));
        assert_eq!(1, form.staged_adds.len());
    }

    #[test]
    fn an_add_of_an_already_listed_name_is_rejected_not_duplicated() {
        let mut form = staged_form();
        assert_eq!(Some(Kind::Term), form.stage_add(&fixture("terms.zip")));

        assert_eq!(None, form.stage_add(&fixture("terms.zip")));

        assert_eq!(1, form.staged_adds.len());
        assert_eq!(1, form.dict_names.iter().filter(|n| *n == "FixtureTerms").count());
    }

    /// Only the same file duplicates.
    #[test]
    fn a_title_an_installed_dictionary_uses_does_not_block_the_add() {
        let mut form = staged_form();
        form.dict_names.push("FixtureTerms".into());
        assert_eq!(Some(Kind::Term), form.stage_add(&fixture("terms.zip")));
        assert_eq!(1, form.staged_adds.len());
    }

    #[test]
    fn removing_the_same_row_twice_records_it_once() {
        let mut form = staged_form();
        let name = form.dict_names[0].clone();
        form.stage_remove(&name);
        form.stage_remove(&name);
        assert_eq!(vec![name], form.staged_removes);
    }

    #[test]
    fn a_frequency_row_is_removable_by_the_same_call() {
        let mut form = staged_form();
        form.freq_names = vec!["jiten_freq_global.zip".into()];
        form.stage_remove("jiten_freq_global.zip");
        assert!(form.freq_names.is_empty());
        assert_eq!(vec!["jiten_freq_global.zip".to_string()], form.staged_removes);
    }

    #[test]
    fn a_removed_dictionary_loses_its_display_order_entry() {
        let cfg = cfg_with(&["大辞林", "Jitendex"]);
        let mut form = from_config(&cfg, &dicts());
        form.stage_remove("大辞林　第四版");
        assert_eq!(
            vec!["Jitendex".to_string()],
            apply_to(&form, &cfg).dictionaries.display_order
        );
    }

    /// A title orders, not a file.
    #[test]
    fn a_staged_add_is_ordered_by_its_title_not_its_filename() {
        let cfg = cfg_with(&["大辞林", "Jitendex"]);
        let mut form = from_config(&cfg, &dicts());
        assert_eq!(Some(Kind::Term), form.stage_add(&fixture("terms.zip")));
        let out = apply_to(&form, &cfg);
        assert!(!out.dictionaries.display_order.iter().any(|e| e.contains(".zip")),
            "a filename must never reach display_order");
        assert_eq!(
            vec!["大辞林".to_string(), "Jitendex".to_string(), "FixtureTerms".to_string()],
            out.dictionaries.display_order
        );
    }

    fn library() -> Library {
        Library {
            entries: vec![
                crate::library::Entry {
                    file: "jitendex.zip".into(),
                    name: "Jitendex.org [2026-07-09]".into(),
                    kind: Kind::Term,
                },
                crate::library::Entry {
                    file: "daijirin.zip".into(),
                    name: "大辞林　第四版".into(),
                    kind: Kind::Term,
                },
                crate::library::Entry {
                    file: "freq.zip".into(),
                    name: "jiten_freq_global".into(),
                    kind: Kind::Frequency,
                },
            ],
        }
    }

    #[test]
    fn the_frequency_list_comes_from_the_library_not_the_database() {
        let form = with_library(staged_form(), &library());
        assert_eq!(vec!["jiten_freq_global".to_string()], form.freq_names);
        assert!(!form.library_empty);
        assert_eq!(vec!["大辞林　第四版", "Jitendex.org [2026-07-09]"], form.dict_names);
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

    /// Frequency alone is nothing.
    #[test]
    fn a_frequency_only_library_leaves_no_term_archives() {
        let mut form = with_library(staged_form(), &Library::default());
        form.dict_names.clear();
        assert_eq!(Some(Kind::Frequency), form.stage_add(&fixture("freq.zip")));
        assert_eq!(0, terms_after_apply(&form, &Library::default()));
        assert_eq!(Some(Kind::Term), form.stage_add(&fixture("terms.zip")));
        assert_eq!(1, terms_after_apply(&form, &Library::default()));
    }

    /// A path naming nothing.
    #[test]
    fn an_add_that_no_longer_exists_cannot_be_staged_at_all() {
        let mut form = with_library(staged_form(), &Library::default());
        form.dict_names.clear();
        assert_eq!(None, form.stage_add(Path::new(r"C:\gone\jmdict.zip")));
        assert_eq!(0, terms_after_apply(&form, &Library::default()));
    }

    /// Kept, not deleted.
    #[test]
    fn a_stale_entry_is_replaced_by_applying_not_by_reporting() {
        let cfg = cfg_with(&["Kenkyusha", "大辞林"]);
        assert_eq!(vec!["Kenkyusha".to_string()], stale_order_entries(&cfg, &dicts()));
        let out = apply_to(&from_config(&cfg, &dicts()), &cfg);
        assert!(!out.dictionaries.display_order.contains(&"Kenkyusha".to_string()));
        assert!(stale_order_entries(&out, &dicts()).is_empty());
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
        assert_eq!(3, lib.entries.len());
        assert_eq!(2, lib.term_paths(&dir).len(), "the copy is a term archive");
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
    fn a_frequency_archive_added_under_dictionaries_is_still_not_a_dictionary() {
        let (dir, _guard) = stocked("misfiled");
        let before = files_in(&dir);
        let mut form = form_for(&dir);
        form.stage_remove("FixtureTerms");
        form.freq_names.clear();
        // Routed by what it is.
        assert_eq!(Some(Kind::Frequency), form.stage_add(&fixture("freq.zip")));
        assert!(!form.dict_names.iter().any(|n| n.contains("Freq")));
        assert!(form.freq_names.contains(&"FixtureFreq".to_string()));

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
            form.dict_names.contains(&"broken.zip".to_string()),
            "it must stay visible so it can be removed: {:?}",
            form.dict_names
        );
        form.stage_remove("FixtureTerms");

        let refused = stage_into_library(&form, &dir).unwrap_err();

        assert!(format!("{refused:#}").contains("no dictionary"), "{refused:#}");
        assert_eq!(before, files_in(&dir), "terms.zip survives");
    }

    #[test]
    fn an_unreadable_row_contributes_no_display_order_entry() {
        let (dir, _guard) = stocked("unreadable_order");
        std::fs::write(dir.join("broken.zip"), b"not a zip at all").unwrap();
        let cfg = cfg_with(&[]);
        let form = with_library(from_config(&cfg, &dicts()), &Library::load(&dir).unwrap());

        let out = apply_to(&form, &cfg);

        assert!(!out.dictionaries.display_order.iter().any(|e| e.contains("broken")), "{out:?}");
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
    fn the_frequency_list_a_window_opens_with_comes_from_the_library() {
        let (dir, _guard) = stocked("form");
        let form = form_for(&dir);
        assert_eq!(vec!["FixtureFreq".to_string()], form.freq_names);
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
                kind: Kind::Term,
            }],
        };
        let form = with_library(from_config(&Config::default(), &[]), &lib);
        assert!(form.dict_names.contains(&"DroppedIn".to_string()), "{form:?}");
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
    fn a_removed_dictionary_loses_its_order_entry() {
        let mut cfg = cfg_with(&["大辞林", "Jitendex"]);
        dictionary_removed(&mut cfg, "大辞林　第四版");
        assert_eq!(strs(&["Jitendex"]), cfg.dictionaries.display_order);
    }

    #[test]
    fn a_removal_drops_the_name_from_every_language_list() {
        let mut cfg = with_list(
            with_list(cfg_with(&["大辞林", "Jitendex"]), "ja", &["大辞林", "Jitendex"]),
            "zh-Hans-CN",
            &["大辞林", "中日大辞典"],
        );
        dictionary_removed(&mut cfg, "大辞林　第四版");
        assert_eq!(strs(&["Jitendex"]), cfg.dictionaries.per_language["ja"]);
        assert_eq!(strs(&["中日大辞典"]), cfg.dictionaries.per_language["zh-Hans-CN"]);
    }

    /// `[]` searches everything.
    #[test]
    fn a_language_list_left_empty_loses_its_key() {
        let mut cfg = with_list(cfg_with(&["大辞林", "Jitendex"]), "ja", &["大辞林"]);
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
        let mut cfg = with_list(cfg_with(&["Jitendex"]), "ja", &["大辞林"]);
        dictionary_removed(&mut cfg, "大辞林　第四版");
        assert!(!is_scoped(&from_config(&cfg, &[])), "the language must be unscoped again");

        let blanked = with_list(cfg_with(&["Jitendex"]), "ja", &[]);
        assert!(
            is_scoped(&from_config(&blanked, &[])),
            "[] keeps the language scoped and pins it at the next Apply",
        );
    }

    /// A blank claims nothing.
    #[test]
    fn a_removal_leaves_a_blank_entry_alone() {
        let mut cfg = cfg_with(&["", "大辞林"]);
        dictionary_removed(&mut cfg, "大辞林　第四版");
        assert_eq!(strs(&[""]), cfg.dictionaries.display_order);
    }

    /// A frequency name too.
    #[test]
    fn a_removal_naming_no_entry_changes_nothing() {
        let before = with_list(cfg_with(&["大辞林", "Jitendex"]), "ja", &["大辞林"]);
        let mut cfg = before.clone();
        dictionary_removed(&mut cfg, "jiten_freq_global");
        assert_eq!(before, cfg);
    }

    /// Recorded, not desired.
    #[test]
    fn a_substring_two_dictionaries_share_is_dropped_by_either_removal() {
        let mut cfg = cfg_with(&["大辞"]);
        dictionary_removed(&mut cfg, "大辞林　第四版");
        assert!(cfg.dictionaries.display_order.is_empty(), "大辞泉 loses its place too");
    }

    #[test]
    fn a_list_that_was_already_empty_is_removed_too() {
        let mut cfg = with_list(cfg_with(&["Jitendex"]), "zh-Hans-CN", &[]);
        dictionary_removed(&mut cfg, "大辞林　第四版");
        assert!(cfg.dictionaries.per_language.is_empty());
    }

    #[test]
    fn an_added_dictionary_is_appended_with_a_derived_key() {
        let mut cfg = cfg_with(&["大辞林"]);
        dictionary_added(&mut cfg, "Jitendex.org [2026-07-09]");
        assert_eq!(strs(&["大辞林", "Jitendex.org"]), cfg.dictionaries.display_order);
    }

    #[test]
    fn an_added_dictionary_joins_a_language_that_has_a_list() {
        let mut cfg = with_list(cfg_with(&["大辞林"]), "ja", &["大辞林"]);
        dictionary_added(&mut cfg, "Jitendex.org [2026-07-09]");
        assert_eq!(strs(&["大辞林", "Jitendex.org"]), cfg.dictionaries.per_language["ja"]);
    }

    /// No entry: search all.
    #[test]
    fn an_added_dictionary_never_creates_a_language_list() {
        let mut cfg = cfg_with(&["大辞林"]);
        dictionary_added(&mut cfg, "Jitendex.org [2026-07-09]");
        assert!(
            cfg.dictionaries.per_language.is_empty(),
            "creating an entry would pin the language: {:?}",
            cfg.dictionaries.per_language
        );
    }

    /// `[]` is not a list.
    #[test]
    fn an_added_dictionary_never_fills_an_empty_language_list() {
        let mut cfg = with_list(cfg_with(&["大辞林"]), "ja", &[]);
        dictionary_added(&mut cfg, "Jitendex.org [2026-07-09]");
        assert!(
            cfg.dictionaries.per_language["ja"].is_empty(),
            "filling [] would scope ja to the new dictionary alone",
        );
    }

    #[test]
    fn an_added_dictionary_leaves_the_other_languages_alone() {
        let mut cfg = with_list(
            with_list(cfg_with(&["大辞林"]), "ja", &["大辞林"]),
            "zh-Hans-CN",
            &["中日大辞典"],
        );
        dictionary_added(&mut cfg, "Jitendex.org [2026-07-09]");
        assert_eq!(strs(&["中日大辞典"]), cfg.dictionaries.per_language["zh-Hans-CN"]);
    }

    /// It is already ordered.
    #[test]
    fn an_entry_that_already_claims_the_name_is_not_duplicated() {
        let mut cfg = with_list(cfg_with(&["Jitendex"]), "ja", &["Jitendex"]);
        dictionary_added(&mut cfg, "Jitendex.org [2026-07-09]");
        assert_eq!(strs(&["Jitendex"]), cfg.dictionaries.display_order);
        assert_eq!(strs(&["Jitendex"]), cfg.dictionaries.per_language["ja"]);
    }

    /// A blank pins everything.
    #[test]
    fn a_dictionary_with_no_title_adds_no_entry() {
        let mut cfg = with_list(cfg_with(&["大辞林"]), "ja", &["大辞林"]);
        dictionary_added(&mut cfg, "");
        assert_eq!(strs(&["大辞林"]), cfg.dictionaries.display_order);
        assert_eq!(strs(&["大辞林"]), cfg.dictionaries.per_language["ja"]);
    }

    /// Two writers, one result.
    #[test]
    fn an_incremental_removal_agrees_with_a_full_apply() {
        let cfg = cfg_with(&["大辞林", "Jitendex"]);
        let kept = vec![DictInfo { dict_id: 1, name: "Jitendex.org [2026-07-09]".into() }];
        let full = apply_to(&from_config(&cfg, &kept), &cfg);
        let mut incremental = cfg.clone();
        dictionary_removed(&mut incremental, "大辞林　第四版");
        assert_eq!(full.dictionaries, incremental.dictionaries);
    }

    /// Two writers, one result.
    #[test]
    fn an_incremental_addition_agrees_with_a_full_apply() {
        let cfg = cfg_with(&["大辞林"]);
        let full = apply_to(&from_config(&cfg, &dicts()), &cfg);
        let mut incremental = cfg.clone();
        dictionary_added(&mut incremental, "Jitendex.org [2026-07-09]");
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

    fn entry(file: &str, kind: Kind) -> crate::library::Entry {
        crate::library::Entry { file: file.into(), name: file.into(), kind }
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
        lib.entries.push(entry("dropped-in.zip", Kind::Term));
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
        lib.entries.push(entry("dropped-in.zip", Kind::Term));
        let found = drift(Some(EDITED_JSON), &lib);
        assert_eq!(strs(&["dropped-in.zip"]), found.unbuilt);
        assert_eq!(strs(&["daijirin.zip"]), found.orphaned);
    }

    /// The builder skips these.
    #[test]
    fn an_unreadable_archive_is_not_drift() {
        let mut lib = library();
        lib.entries.push(entry("broken.zip", Kind::Unreadable));
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
        let lib = Library { entries: vec![entry("freq.zip", Kind::Frequency)] };
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

