//! The screenshot-on-add rule (spec D4).
//!
//! One home for every decision a screenshot add makes: whether a picture
//! rides along at all, what the file is called, which Anki field the
//! picture lands in, and the AnkiConnect call that files it. The bins
//! own only the OS work either side - hide the windows, pick a region,
//! grab the pixels, put them back - because none of the rules here is a
//! platform fact (ADR-0001).

use crate::config::{AnkiConfig, Config};
use crate::controller::PopupView;
use anyhow::{Context, Result};
use base64::Engine;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One screenshot's add: the file to write and the note to file it on.
#[derive(Debug, Clone, PartialEq)]
pub struct ShotPlan {
    /// The word the card is filed under, and the key
    /// [`Event::NoteAdded`](crate::controller::Event) answers with.
    pub expr: String,
    pub fields: HashMap<String, String>,
    /// `<save_root>/<sanitized expr>_<epoch seconds>.png`.
    pub path: PathBuf,
    /// The Anki field the picture is embedded in, if the field map
    /// routes the `screenshot` source anywhere. `None` files the note
    /// with no picture at all.
    pub picture_field: Option<String>,
}

/// The shape of one screenshot add, gated by nothing.
///
/// This is the mining screenshot's plan: it saves a picture whatever the
/// popup's add state is, so it applies none of [`plan_add`]'s guards and
/// does not consult `include_on_add`.
pub fn plan(view: &PopupView<'_>, cfg: &Config, save_root: &Path, now: u64) -> ShotPlan {
    let (expr, fields) =
        crate::controller::note_payload(view.presentation, cfg.anki.first_dict_only);
    let path = save_root.join(format!("{}_{now}.png", sanitize_filename(&expr)));
    let picture_field = cfg
        .anki
        .field_map
        .iter()
        .find(|m| m.source == "screenshot")
        .map(|m| m.anki_field.clone());
    ShotPlan { expr, fields, path, picture_field }
}

/// The screenshot that rides along with an add, or `None` when none
/// does: `actions.screenshot.include_on_add` is off, the payload has no
/// expression to file the picture under, or the word is already in the
/// collection.
///
/// **`Controller::start_add`'s in-flight guard is deliberately not
/// here.** Both bins reach this function from the `Command::AddNote` the
/// state machine emits, and `start_add` marks the popup adding *before*
/// it emits that command - reading the flag here would refuse the very
/// add the machine just authorised, and no screenshot would ever ride
/// along. Refusing a second concurrent add is `start_add`'s job and it
/// has already done it. The other two are kept because they are facts
/// about the card rather than about the machine: a blank expression
/// would write `_<epoch>.png` and file an empty note, and a word already
/// in the collection must not be filed twice.
///
/// The payload is re-derived through [`crate::controller::note_payload`]
/// rather than taken from the command, and cannot disagree with it:
/// `expr` is `written || reading` off the same `Presentation` the
/// command was built from and does not depend on `first_dict_only` at
/// all, so the guard, the filename and the `NoteAdded` key are exact.
/// `fields` would differ only if a bin fed this function a `Config` out
/// of sync with the `ControllerConfig` it built the machine from, which
/// is one clone of one value in both bins.
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

/// Writes the PNG, creating the save folder if it is not there.
pub fn save(png: &[u8], plan: &ShotPlan) -> Result<()> {
    if let Some(dir) = plan.path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&plan.path, png)
        .with_context(|| format!("writing {}", plan.path.display()))
}

/// [`save`], then the add that carries the picture. Returns the note id.
///
/// The attached filename is Anki's own copy in `collection.media`, not
/// the saved file: it is namespaced `chibipop-screenshot-` so a
/// collection can tell our pictures from everything else, and it carries
/// the saved file's stem so the two can be matched up by eye.
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

/// Seconds since the epoch, for the screenshot filename.
///
/// One clock, here with the rule that consumes it: [`plan`] names files
/// `<word>_<epoch>.png`, so both bins pass this as `now` and a
/// screenshots folder carried between platforms is named one way.
pub fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// A filename for `word`, safe on every platform this ships on.
///
/// The illegal set is Windows': `\ / : * ? " < > |`. It is the strict
/// superset - Linux forbids only `/` and NUL - and one shared rule is
/// what lets a screenshots folder be carried between the two, so the
/// Linux daemon deliberately files under the stricter name rather than
/// growing a second sanitizer.
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

/// Cuts at the nearest boundary.
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
        }
    }

    fn view<'a>(p: &'a Presentation, anki: &'a AnkiPopupState) -> PopupView<'a> {
        PopupView {
            popup: PhysRect { x: 0, y: 0, w: 300, h: 200 },
            anchor: PhysRect { x: 0, y: 0, w: 20, h: 20 },
            scroll: 0,
            content_h: 200,
            view_h: 200,
            presentation: p,
            anki,
            show_back: false,
        }
    }

    /// Anki on and reachable, nothing added, nothing in flight.
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

    /// The one `start_add` guard this function must not restate: it runs
    /// from the `AddNote` that `start_add` emits *after* setting the
    /// flag, so restating it would refuse every screenshot add there is.
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
        // include_on_add is off in the default config.
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
