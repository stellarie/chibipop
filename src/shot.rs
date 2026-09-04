//! The screenshot-on-add and Mining screenshot rules.
//!
//! This module decides whether an add carries a picture, names the file, selects
//! the Anki field, and calls AnkiConnect. The platform bins hide windows, select
//! a region, grab pixels, and restore windows. These rules do not depend on
//! platform facts (ARCHITECTURE.md#workspace-and-seams).

use crate::config::{AnkiConfig, Config};
use crate::controller::PopupView;
use anyhow::{Context, Result};
use base64::Engine;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The file and note data for one screenshot add.
#[derive(Debug, Clone, PartialEq)]
pub struct ShotPlan {
    /// The expression that identifies the card and the key that
    /// [`Event::NoteAdded`](crate::controller::Event) returns.
    pub expr: String,
    pub fields: HashMap<String, String>,
    /// `<save_root>/<sanitized expr>_<epoch seconds>.png`.
    pub path: PathBuf,
    /// The Anki field that receives the picture when the field map routes the
    /// `screenshot` source. `None` means that the note has no picture.
    pub picture_field: Option<String>,
}

/// Builds a screenshot add plan without guards.
///
/// This is the Mining screenshot plan. It saves a picture regardless of the
/// popup add state. It does not apply [`plan_add`]'s guards or read
/// `include_on_add`.
pub fn plan(view: &PopupView<'_>, cfg: &Config, save_root: &Path, now: u64) -> ShotPlan {
    // The Mining screenshot plan uses the same note as the Anki button.
    // It applies the same Card selection.
    let empty = crate::select::CardSelection::default();
    let selection = view.selection.card(0).unwrap_or(&empty);
    let (expr, fields) = crate::controller::note_payload(
        view.presentation,
        cfg.anki.first_dict_only,
        cfg.anki.include_dictionary_name,
        selection,
        cfg.anki.selection_separator.into(),
    );
    let path = save_root.join(format!("{}_{now}.png", sanitize_filename(&expr)));
    let picture_field = cfg
        .anki
        .field_map
        .iter()
        .find(|m| m.source == "screenshot")
        .map(|m| m.anki_field.clone());
    ShotPlan { expr, fields, path, picture_field }
}

/// Plans the screenshot that accompanies an add, or returns `None`.
///
/// The function returns `None` when `actions.screenshot.include_on_add` is off,
/// the payload has no expression for the file name, or the word is already in
/// the collection.
///
/// **`Controller::start_add`'s in-flight guard is deliberately not here.**
/// Both bins call this function after the state machine emits `Command::AddNote`.
/// `start_add` marks the popup with the add state before it emits that command.
/// A guard here would reject the authorized add, so no screenshot would accompany it.
/// `start_add` already rejects a second add at the same time. This function keeps
/// the other two guards because they describe the Card: a blank expression would
/// write `_<epoch>.png` and an empty note, and a word in the collection must not
/// be filed twice.
///
/// The function derives the payload through [`crate::controller::note_payload`]
/// instead of taking it from the command. `expr` is `written || reading` from
/// the same `Presentation` that built the command. `expr` does not depend on
/// `first_dict_only`, so the guard, file name, and `NoteAdded` key agree.
/// `fields` can differ only when a bin gives this function a `Config` that
/// differs from the `ControllerConfig` that built the state machine. Both bins
/// clone one value.
pub fn plan_add(
    view: &PopupView<'_>,
    cfg: &Config,
    save_root: &Path,
    now: u64,
) -> Option<ShotPlan> {
    if !cfg.actions.screenshot.include_on_add {
        return None;
    }
    let plan = plan(view, cfg, save_root, now);
    if plan.expr.is_empty() || view.anki.added.contains(&plan.expr) {
        return None;
    }
    Some(plan)
}

/// Writes the PNG and creates the save folder when needed.
pub fn save(png: &[u8], plan: &ShotPlan) -> Result<()> {
    if let Some(dir) = plan.path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&plan.path, png)
        .with_context(|| format!("writing {}", plan.path.display()))
}

/// Calls [`save`], then adds the note with its picture. Returns the note ID.
///
/// Anki copies the attached file to `collection.media`. The copy uses the
/// `chibipop-screenshot-` namespace and the saved file stem, so a user can
/// match the two names.
pub fn save_and_add(png: &[u8], plan: &ShotPlan, anki: &AnkiConfig) -> Result<i64> {
    save(png, plan)?;
    let picture = plan.picture_field.as_ref().map(|field| crate::anki::NotePicture {
        data_base64: base64::engine::general_purpose::STANDARD.encode(png),
        filename: format!(
            "chibipop-screenshot-{}.png",
            plan.path.file_stem().unwrap_or_default().to_string_lossy()
        ),
        fields: vec![field.clone()],
    });
    crate::anki::add_note_with_picture(
        &anki.url,
        &anki.deck,
        &anki.model,
        &plan.fields,
        &anki.field_map,
        picture.as_ref(),
    )
}

/// Returns seconds since the epoch for a screenshot file name.
///
/// [`plan`] names files `<word>_<epoch>.png`. Both bins pass this value as
/// `now`, so a screenshots folder keeps one name pattern on both platforms.
pub fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Returns a file name for `word` that works on every shipped platform.
///
/// Windows forbids `\ / : * ? " < > |`. Linux forbids only `/` and NUL.
/// This shared rule uses the strict superset on both platforms. The Linux
/// daemon therefore uses the stricter name instead of a second sanitizer.
pub fn sanitize_filename(word: &str) -> String {
    let cleaned: String = word
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect();
    let mut result = String::new();
    let mut prev_underscore = false;
    for c in cleaned.chars() {
        if c == '_' {
            if !prev_underscore {
                result.push('_');
            }
            prev_underscore = true;
        } else {
            result.push(c);
            prev_underscore = false;
        }
    }
    let result = result.trim_matches('_').to_string();
    if result.is_empty() {
        return "screenshot".to_string();
    }
    truncate_to_char_boundary(&result, 60)
}

/// Truncates a string at the nearest character boundary.
fn truncate_to_char_boundary(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FieldMapping;
    use crate::geom::PhysRect;
    use crate::present::{AnkiPopupState, Card, Presentation};

    const ROOT: &str = "/tmp/chibipop-shot-tests";

    fn presentation(written: Option<&str>, reading: Option<&str>) -> Presentation {
        Presentation {
            top: Some(Card {
                written: written.map(str::to_string),
                reading: reading.map(str::to_string),
                pos: Vec::new(),
                freq: None,
                blocks: Vec::new(),
                match_len: 1,
                pitch: Vec::new(),
            }),
            collapsed: Vec::new(),
            all_cards: Vec::new(),
            sentence: None,
            surface: None,
        }
    }

    fn view<'a>(p: &'a Presentation, anki: &'a AnkiPopupState) -> PopupView<'a> {
        static NONE: std::sync::LazyLock<crate::select::Selections> =
            std::sync::LazyLock::new(crate::select::Selections::default);
        PopupView {
            popup: PhysRect { x: 0, y: 0, w: 300, h: 200 },
            anchor: PhysRect { x: 0, y: 0, w: 20, h: 20 },
            scroll: 0,
            content_h: 200,
            view_h: 200,
            presentation: p,
            anki,
            show_back: false,
            selection: &NONE,
        }
    }

    /// Returns an Anki state with Anki enabled and connected, with no added note or active add.
    fn anki_state() -> AnkiPopupState {
        AnkiPopupState::fresh(true)
    }

    fn cfg_on() -> Config {
        let mut cfg = Config::default();
        cfg.actions.screenshot.include_on_add = true;
        cfg
    }

    #[test]
    fn a_plan_names_the_file_after_the_word_and_the_epoch_second() {
        let p = presentation(Some("\u{5bbf}\u{820e}"), None);
        let a = anki_state();
        let plan = plan_add(&view(&p, &a), &cfg_on(), Path::new(ROOT), 1_700_000_000).unwrap();
        assert_eq!(
            Path::new(ROOT).join("\u{5bbf}\u{820e}_1700000000.png"),
            plan.path
        );
        assert_eq!("\u{5bbf}\u{820e}", plan.expr);
    }

    #[test]
    fn the_picture_field_is_the_field_map_row_sourced_from_the_screenshot() {
        let p = presentation(Some("\u{5bbf}\u{820e}"), None);
        let a = anki_state();
        let mut cfg = cfg_on();
        cfg.anki.field_map.push(FieldMapping {
            anki_field: "Picture".into(),
            source: "screenshot".into(),
        });
        let plan = plan_add(&view(&p, &a), &cfg, Path::new(ROOT), 1).unwrap();
        assert_eq!(Some("Picture".to_string()), plan.picture_field);
    }

    #[test]
    fn a_field_map_that_routes_no_screenshot_plans_a_pictureless_add() {
        let p = presentation(Some("\u{5bbf}\u{820e}"), None);
        let a = anki_state();
        let plan = plan_add(&view(&p, &a), &cfg_on(), Path::new(ROOT), 1).unwrap();
        assert_eq!(None, plan.picture_field);
    }

    #[test]
    fn include_on_add_off_plans_nothing() {
        let p = presentation(Some("\u{5bbf}\u{820e}"), None);
        let a = anki_state();
        let cfg = Config::default();
        assert!(!cfg.actions.screenshot.include_on_add, "the shipped default");
        assert_eq!(None, plan_add(&view(&p, &a), &cfg, Path::new(ROOT), 1));
    }

    #[test]
    fn a_card_with_no_expression_plans_nothing_rather_than_an_epoch_only_name() {
        let p = presentation(None, None);
        let a = anki_state();
        assert_eq!(None, plan_add(&view(&p, &a), &cfg_on(), Path::new(ROOT), 1));
    }

    #[test]
    fn a_popup_with_no_card_plans_nothing() {
        let p = Presentation {
            top: None,
            collapsed: Vec::new(),
            all_cards: Vec::new(),
            sentence: None,
            surface: None,
        };
        let a = anki_state();
        assert_eq!(None, plan_add(&view(&p, &a), &cfg_on(), Path::new(ROOT), 1));
    }

    #[test]
    fn a_word_already_in_the_collection_plans_nothing() {
        let p = presentation(Some("\u{5bbf}\u{820e}"), None);
        let mut a = anki_state();
        a.added.insert("\u{5bbf}\u{820e}".to_string());
        assert_eq!(None, plan_add(&view(&p, &a), &cfg_on(), Path::new(ROOT), 1));
    }

    /// This test sets `adding` before it calls `plan_add`. The state machine already
    /// passed the `start_add` guard before it emitted `AddNote`, so `plan_add` must
    /// still create this screenshot plan.
    #[test]
    fn an_add_already_marked_in_flight_still_plans_because_start_add_owns_that_guard() {
        let p = presentation(Some("\u{5bbf}\u{820e}"), None);
        let mut a = anki_state();
        a.adding = true;
        assert!(plan_add(&view(&p, &a), &cfg_on(), Path::new(ROOT), 1).is_some());
    }

    #[test]
    fn the_ungated_plan_ignores_include_on_add_and_the_add_guards() {
        let p = presentation(Some("\u{5bbf}\u{820e}"), None);
        let mut a = anki_state();
        a.adding = true;
        a.added.insert("\u{5bbf}\u{820e}".to_string());
        // The default config has `include_on_add` off.
        let plan = plan(&view(&p, &a), &Config::default(), Path::new(ROOT), 7);
        assert_eq!(Path::new(ROOT).join("\u{5bbf}\u{820e}_7.png"), plan.path);
    }

    #[test]
    fn the_expression_falls_back_to_the_reading_the_way_the_note_payload_does() {
        let p = presentation(None, Some("\u{306D}\u{3053}"));
        let a = anki_state();
        let plan = plan_add(&view(&p, &a), &cfg_on(), Path::new(ROOT), 3).unwrap();
        assert_eq!("\u{306D}\u{3053}", plan.expr);
        assert_eq!(Path::new(ROOT).join("\u{306D}\u{3053}_3.png"), plan.path);
    }

    #[test]
    fn saving_creates_the_folder_it_was_told_to_write_into() {
        let dir = std::env::temp_dir().join(format!("chibipop-shot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plan = ShotPlan {
            expr: "x".into(),
            fields: HashMap::new(),
            path: dir.join("deeper").join("x_1.png"),
            picture_field: None,
        };
        save(b"not really a png", &plan).unwrap();
        assert_eq!(b"not really a png".to_vec(), std::fs::read(&plan.path).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sanitizing_replaces_every_windows_illegal_char() {
        assert_eq!("a_b_c", sanitize_filename("a/b\\c"));
        assert_eq!("a_b", sanitize_filename("a:b"));
        assert_eq!("a_b", sanitize_filename("a*b"));
        assert_eq!("a_b", sanitize_filename("a?b"));
        assert_eq!("a_b", sanitize_filename("a\"b"));
        assert_eq!("a_b", sanitize_filename("a<b"));
        assert_eq!("a_b", sanitize_filename("a>b"));
        assert_eq!("a_b", sanitize_filename("a|b"));
    }

    #[test]
    fn sanitizing_collapses_consecutive_underscores() {
        assert_eq!("a_b", sanitize_filename("a///b"));
    }

    #[test]
    fn sanitizing_truncates_long_names_at_a_char_boundary() {
        let long = "\u{3042}".repeat(100);
        let result = sanitize_filename(&long);
        assert!(result.len() <= 60, "{} bytes", result.len());
        assert_eq!("\u{3042}".repeat(20), result, "20 three-byte chars fit in 60");
    }

    #[test]
    fn sanitizing_an_empty_name_falls_back() {
        assert_eq!("screenshot", sanitize_filename(""));
    }

    #[test]
    fn sanitizing_an_all_illegal_name_falls_back() {
        assert_eq!("screenshot", sanitize_filename("///"));
    }

    #[test]
    fn sanitizing_passes_japanese_through() {
        assert_eq!("\u{5bbf}\u{820e}", sanitize_filename("\u{5bbf}\u{820e}"));
    }
}
