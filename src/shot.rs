//! The screenshot-on-add rules.
//!
//! This module decides whether an add carries a picture, names the file, selects
//! the Anki field, and calls AnkiConnect. The platform bins hide windows, select
//! a region, grab pixels, and restore windows. These rules do not depend on
//! platform facts (ARCHITECTURE.md#workspace-and-seams).

use crate::config::{AnkiConfig, Config};
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
    /// The Anki fields that receive the picture.
    pub picture_fields: Vec<String>,
}

/// Plans the screenshot that accompanies an already-authorized add, or returns
/// `None` when the screenshot-on-add setting is off.
///
/// `Command::AddNote` has already passed the Controller's add guards. Its
/// `expr` and `fields` are therefore the complete note payload for this plan.
/// In particular, this function must not read a popup or rebuild a payload:
/// the popup may have changed by the time the platform performs the screenshot.
///
/// This seam owns only screenshot choices: whether the feature is enabled, the
/// filename derived from the authorized expression, and the picture field.
pub fn plan_add(
    expr: &str,
    fields: &HashMap<String, String>,
    cfg: &Config,
    save_root: &Path,
    now: u64,
) -> Option<ShotPlan> {
    if !cfg.actions.screenshot.include_on_add {
        return None;
    }
    Some(make_plan(expr.to_string(), fields.clone(), cfg, save_root, now))
}

fn make_plan(
    expr: String,
    fields: HashMap<String, String>,
    cfg: &Config,
    save_root: &Path,
    now: u64,
) -> ShotPlan {
    let path = save_root.join(format!("{}_{now}.png", sanitize_filename(&expr)));
    let picture_fields = cfg
        .anki
        .field_map
        .iter()
        .filter(|m| m.source == "screenshot")
        .map(|m| m.anki_field.clone())
        .collect();
    ShotPlan { expr, fields, path, picture_fields }
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

/// Calls [`save`], then writes the note with its picture.
///
/// Anki copies the attached file to `collection.media`. The copy uses the
/// `chibipop-screenshot-` namespace and the saved file stem, so a user can
/// match the two names.
pub fn save_and_add(
    png: &[u8],
    plan: &ShotPlan,
    anki: &AnkiConfig,
) -> Result<crate::anki::WriteResult> {
    save(png, plan)?;
    let picture = (!plan.picture_fields.is_empty()).then(|| crate::anki::NotePicture {
        data_base64: base64::engine::general_purpose::STANDARD.encode(png),
        filename: format!(
            "chibipop-screenshot-{}.png",
            plan.path.file_stem().unwrap_or_default().to_string_lossy()
        ),
        fields: plan.picture_fields.clone(),
    });
    crate::anki::write_note(
        &anki.url,
        &anki.deck,
        &anki.model,
        &plan.fields,
        &anki.field_map,
        picture.as_ref(),
        anki.overwrite_duplicates,
    )
}

/// Returns seconds since the epoch for a screenshot file name.
///
/// [`plan_add`] names files `<word>_<epoch seconds>.png`. Both bins pass this value
/// as `now`, so a screenshots folder keeps one name pattern on both platforms.
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

    const ROOT: &str = "/tmp/chibipop-shot-tests";

    fn cfg_on() -> Config {
        let mut cfg = Config::default();
        cfg.actions.screenshot.include_on_add = true;
        cfg
    }

    #[test]
    fn an_authorized_payload_names_the_file_after_its_expression_and_epoch_second() {
        let fields = HashMap::new();
        let plan =
            plan_add("\u{5bbf}\u{820e}", &fields, &cfg_on(), Path::new(ROOT), 1_700_000_000)
                .unwrap();
        assert_eq!(
            Path::new(ROOT).join("\u{5bbf}\u{820e}_1700000000.png"),
            plan.path
        );
        assert_eq!("\u{5bbf}\u{820e}", plan.expr);
        assert_eq!(fields, plan.fields);
    }

    #[test]
    fn an_authorized_payload_is_used_without_rebuilding_it() {
        let fields = HashMap::from([("Expression".to_string(), "card A".to_string())]);
        let plan = plan_add("card A", &fields, &cfg_on(), Path::new(ROOT), 11).unwrap();

        assert_eq!("card A", plan.expr);
        assert_eq!(fields, plan.fields);
        assert_eq!(Path::new(ROOT).join("card A_11.png"), plan.path);
    }

    #[test]
    fn the_picture_fields_are_the_screenshot_rows() {
        let mut cfg = cfg_on();
        cfg.anki.field_map.push(FieldMapping {
            anki_field: "Picture".into(),
            source: "screenshot".into(),
        });
        cfg.anki.field_map.push(FieldMapping {
            anki_field: "Picture2".into(),
            source: "screenshot".into(),
        });
        let plan = plan_add("", &HashMap::new(), &cfg, Path::new(ROOT), 1).unwrap();
        assert_eq!(vec!["Picture".to_string(), "Picture2".to_string()], plan.picture_fields);
    }

    #[test]
    fn a_field_map_that_routes_no_screenshot_plans_a_pictureless_add() {
        let plan = plan_add("宿舎", &HashMap::new(), &cfg_on(), Path::new(ROOT), 1).unwrap();
        assert!(plan.picture_fields.is_empty());
    }

    #[test]
    fn include_on_add_off_plans_nothing() {
        let cfg = Config::default();
        assert!(!cfg.actions.screenshot.include_on_add, "the shipped default");
        assert_eq!(None, plan_add("宿舎", &HashMap::new(), &cfg, Path::new(ROOT), 1));
    }

    #[test]
    fn an_authorized_empty_expression_still_has_a_payload_plan() {
        let fields = HashMap::new();
        let plan = plan_add("", &fields, &cfg_on(), Path::new(ROOT), 1).unwrap();
        assert_eq!("", plan.expr);
        assert_eq!(fields, plan.fields);
        assert_eq!(Path::new(ROOT).join("screenshot_1.png"), plan.path);
    }

    #[test]
    fn saving_creates_the_folder_it_was_told_to_write_into() {
        let dir = std::env::temp_dir().join(format!("chibipop-shot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plan = ShotPlan {
            expr: "x".into(),
            fields: HashMap::new(),
            path: dir.join("deeper").join("x_1.png"),
            picture_fields: Vec::new(),
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
