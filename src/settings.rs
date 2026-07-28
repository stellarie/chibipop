//! The settings window's data model: a [`Config`] flattened into plain fields
//! and folded back again.
//!
//! Deliberately free of the `windows` crate, so every decision this round
//! makes that is worth testing can be tested without a screen. `ui`'s settings
//! window owns the controls and speaks only these types; it holds no opinion
//! about what any of them mean.

use crate::config::{Config, TriggerMode};
use crate::present::{dict_order_rank, DictInfo};

/// The bounds live beside the fields they bound, so `config::load_or_create`
/// can apply the same ones to a hand-edited file that never reaches Apply.
pub use crate::config::{MAX_HEIGHT_RANGE, PASSES_RANGE, SUMMARY_RANGE};

/// Every user-facing setting, as the window edits it.
///
/// `dict_names` holds **live dictionary names**, not the substrings the config
/// stores — the window shows the user what is actually installed, and
/// [`apply_to`] converts back. See its documentation for why that conversion
/// is emphatically not the identity.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsForm {
    pub mode: TriggerMode,
    pub theme: String,
    pub font: String,
    pub max_height_percent: u8,
    pub summary_chars: usize,
    pub highlight_match: bool,
    pub scroll_popup: bool,
    pub exclude_from_capture: bool,
    pub dict_names: Vec<String>,
    pub max_ocr_passes: u8,
    pub show_scan_region: bool,
}

/// Flatten `cfg` for editing, listing `dicts` in the order the popup would
/// actually show them.
///
/// The ordering runs through [`dict_order_rank`], the same function
/// `present::build` uses, so the list cannot show an order the popup will not
/// honour.
pub fn from_config(cfg: &Config, dicts: &[DictInfo]) -> SettingsForm {
    let mut ordered: Vec<&DictInfo> = dicts.iter().collect();
    ordered.sort_by_key(|d| {
        (
            dict_order_rank(&d.name, &cfg.dictionaries.display_order).unwrap_or(usize::MAX),
            d.dict_id,
        )
    });

    SettingsForm {
        mode: cfg.trigger.mode,
        theme: cfg.popup.theme.clone(),
        font: cfg.popup.font.clone(),
        max_height_percent: cfg.popup.max_height_percent,
        summary_chars: cfg.popup.summary_chars,
        highlight_match: cfg.popup.highlight_match,
        scroll_popup: cfg.popup.scroll_popup,
        exclude_from_capture: cfg.popup.exclude_from_capture,
        dict_names: ordered.iter().map(|d| d.name.clone()).collect(),
        max_ocr_passes: cfg.ocr.max_ocr_passes,
        show_scan_region: cfg.debug.show_scan_region,
    }
}

/// Fold `form` back onto `cfg`, returning the config to write.
///
/// **`display_order` is rebuilt from substrings, never from live names**, and
/// that is the whole reason this is not a field-for-field copy.
/// `dictionaries.display_order` holds case-insensitive *substrings* because
/// Jitendex's stored name embeds a release date that changes on every rebuild;
/// writing live names back would work perfectly until the next rebuild and
/// then silently stop applying, dropping that dictionary to the bottom with no
/// error anywhere. See [`order_key`].
///
/// Numbers are clamped rather than rejected: the window's spin controls cannot
/// produce an out-of-range value, so one that is out of range came from a
/// hand-edited file, and quietly bringing it into range beats refusing to
/// apply anything at all.
pub fn apply_to(form: &SettingsForm, cfg: &Config) -> Config {
    let mut out = cfg.clone();
    out.trigger.mode = form.mode;
    out.popup.theme = form.theme.clone();
    out.popup.font = form.font.clone();
    out.popup.max_height_percent =
        form.max_height_percent.clamp(MAX_HEIGHT_RANGE.0, MAX_HEIGHT_RANGE.1);
    out.popup.summary_chars = form.summary_chars.clamp(SUMMARY_RANGE.0, SUMMARY_RANGE.1);
    out.popup.highlight_match = form.highlight_match;
    out.popup.scroll_popup = form.scroll_popup;
    out.popup.exclude_from_capture = form.exclude_from_capture;
    out.ocr.max_ocr_passes = form.max_ocr_passes.clamp(PASSES_RANGE.0, PASSES_RANGE.1);
    out.debug.show_scan_region = form.show_scan_region;
    out.dictionaries.display_order = form
        .dict_names
        .iter()
        .map(|name| order_key(name, &cfg.dictionaries.display_order))
        .collect();
    out
}

/// The `display_order` entry to write for a dictionary called `name`.
///
/// Keeps whichever existing entry already matches, **verbatim**, so a config
/// the user has been running keeps working across dictionary rebuilds. Only a
/// dictionary that has never been ordered gets a derived key, truncated at the
/// first bracket so a release date cannot become part of it.
///
/// Falls back to the whole name when truncation would leave nothing — an empty
/// entry would match *every* dictionary and silently pin the order.
fn order_key(name: &str, existing: &[String]) -> String {
    let lower = name.to_lowercase();
    if let Some(entry) = existing.iter().find(|e| lower.contains(&e.to_lowercase())) {
        return entry.clone();
    }
    let cut = name.find(['[', '(']).unwrap_or(name.len());
    let derived = name[..cut].trim();
    if derived.is_empty() { name.to_string() } else { derived.to_string() }
}

/// `display_order` entries matching no installed dictionary (spec D6a).
///
/// This is what a dictionary rename looks like from inside the settings
/// window, and it is worth reporting precisely: [`dict_order_rank`] returns
/// `None` for an unmatched dictionary and the caller sorts it **last**, so the
/// user-visible symptom is a dictionary silently dropping to the bottom —
/// which reads as chibipop reordering things by itself rather than as a stale
/// line in a file.
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

    /// A dictionary matching no entry sorts last, by dict_id - the same rule
    /// `present::build` applies, so the window cannot show an order the popup
    /// will not honour.
    #[test]
    fn an_unordered_dictionary_is_listed_last() {
        let form = from_config(&cfg_with(&["大辞林"]), &dicts());
        assert_eq!(vec!["大辞林　第四版", "Jitendex.org [2026-07-09]"], form.dict_names);
    }

    /// THE trap (spec D6). `display_order` holds substrings because
    /// Jitendex's name carries a release date that changes on every rebuild.
    /// Reordering must return the user's own strings, never the live names.
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

    /// A name that is entirely bracketed would derive an empty key, which
    /// would then match every dictionary.
    #[test]
    fn a_key_that_would_be_empty_falls_back_to_the_whole_name() {
        let cfg = cfg_with(&[]);
        let odd = vec![DictInfo { dict_id: 1, name: "[2026] Something".into() }];
        let form = from_config(&cfg, &odd);
        let out = apply_to(&form, &cfg);
        assert_eq!(vec!["[2026] Something".to_string()], out.dictionaries.display_order);
    }

    /// D6a: an entry matching nothing is what a rename looks like from here.
    #[test]
    fn an_entry_matching_no_dictionary_is_reported() {
        let stale = stale_order_entries(&cfg_with(&["大辞林", "Kenkyusha"]), &dicts());
        assert_eq!(vec!["Kenkyusha".to_string()], stale);
    }

    #[test]
    fn entries_that_all_match_report_nothing() {
        assert!(stale_order_entries(&cfg_with(&["大辞林", "Jitendex"]), &dicts()).is_empty());
    }

    /// The detection must agree with the ordering it warns about. A warning
    /// that disagreed with `dict_order_rank` would be worse than none.
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

    /// The window must never silently change a setting the user did not
    /// touch. This is the assertion that catches a mis-wired control.
    #[test]
    fn an_untouched_form_round_trips_to_an_equal_config() {
        let cfg = cfg_with(&["大辞林", "Jitendex"]);
        let form = from_config(&cfg, &dicts());
        assert_eq!(cfg, apply_to(&form, &cfg));
    }

    #[test]
    fn every_setting_survives_the_round_trip() {
        let mut cfg = cfg_with(&["大辞林", "Jitendex"]);
        cfg.trigger.mode = TriggerMode::HoldShift;
        cfg.popup.theme = "light".into();
        cfg.popup.font = "Noto Sans JP".into();
        cfg.popup.max_height_percent = 70;
        cfg.popup.summary_chars = 25;
        cfg.popup.highlight_match = false;
        cfg.popup.scroll_popup = false;
        cfg.popup.exclude_from_capture = true;
        cfg.ocr.max_ocr_passes = 3;
        cfg.debug.show_scan_region = true;
        let form = from_config(&cfg, &dicts());
        assert_eq!(cfg, apply_to(&form, &cfg));
    }

    #[test]
    fn out_of_range_numbers_are_clamped_not_rejected() {
        let cfg = cfg_with(&[]);
        let mut form = from_config(&cfg, &dicts());
        form.max_height_percent = 250;
        form.summary_chars = 1;
        form.max_ocr_passes = 99;
        let out = apply_to(&form, &cfg);
        assert_eq!(MAX_HEIGHT_RANGE.1, out.popup.max_height_percent);
        assert_eq!(SUMMARY_RANGE.0, out.popup.summary_chars);
        assert_eq!(PASSES_RANGE.1, out.ocr.max_ocr_passes);
    }

    /// An entry that no longer matches is KEPT, not deleted - it may match a
    /// dictionary that simply is not built right now, and dropping a user's
    /// setting because it is momentarily inapplicable is worse than carrying
    /// it. It disappears only when the user reorders and applies.
    #[test]
    fn a_stale_entry_is_replaced_by_applying_not_by_reporting() {
        let cfg = cfg_with(&["Kenkyusha", "大辞林"]);
        assert_eq!(vec!["Kenkyusha".to_string()], stale_order_entries(&cfg, &dicts()));
        let out = apply_to(&from_config(&cfg, &dicts()), &cfg);
        assert!(!out.dictionaries.display_order.contains(&"Kenkyusha".to_string()));
        assert!(stale_order_entries(&out, &dicts()).is_empty());
    }
}
