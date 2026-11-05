//! The screenshot-on-add rules.
//!
//! This module decides whether an add carries a picture, names the file, selects
//! the Anki field, and calls AnkiConnect. The platform bins hide windows, select
//! a region, grab pixels, and restore windows. These rules do not depend on
//! platform facts (ARCHITECTURE.md#workspace-and-seams).

use crate::config::{AnkiConfig, ImageFormat, ResolvedConfig};
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
    /// `<save_root>/<sanitized expr>_<epoch seconds>.<png|jpg>`.
    pub path: PathBuf,
    /// The Anki fields that receive the picture.
    pub picture_fields: Vec<String>,
    pub encoding: Encoding,
}

/// How [`encode`] writes the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Encoding {
    pub format: ImageFormat,
    pub jpeg_quality: u8,
    /// The longest side in physical pixels. 0 keeps the full size.
    pub max_size: u32,
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
    cfg: &ResolvedConfig,
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
    cfg: &ResolvedConfig,
    save_root: &Path,
    now: u64,
) -> ShotPlan {
    let shot = &cfg.actions.screenshot;
    let encoding = Encoding {
        format: shot.image_format,
        jpeg_quality: shot.jpeg_quality,
        max_size: shot.max_size,
    };
    let path = save_root.join(format!(
        "{}_{now}.{}",
        sanitize_filename(&expr),
        encoding.format.extension()
    ));
    let picture_fields = cfg
        .anki
        .field_map
        .iter()
        .filter(|m| m.source == "screenshot")
        .map(|m| m.anki_field.clone())
        .collect();
    ShotPlan { expr, fields, path, picture_fields, encoding }
}

/// Encodes top-down BGRA8 pixels as the plan asks.
pub fn encode(bgra: &[u8], w: i32, h: i32, plan: &ShotPlan) -> Result<Vec<u8>> {
    let small = crate::image::shrink_bgra(bgra, w, h, plan.encoding.max_size);
    let (bgra, w, h) = match &small {
        Some((px, w, h)) => (px.as_slice(), *w, *h),
        None => (bgra, w, h),
    };
    match plan.encoding.format {
        ImageFormat::Png => crate::image::encode_bgra_to_png(bgra, w, h),
        ImageFormat::Jpeg => {
            crate::image::encode_bgra_to_jpeg(bgra, w, h, plan.encoding.jpeg_quality)
        }
    }
}

/// Writes the picture and creates the save folder when needed.
pub fn save(image: &[u8], plan: &ShotPlan) -> Result<()> {
    if let Some(dir) = plan.path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&plan.path, image)
        .with_context(|| format!("writing {}", plan.path.display()))
}

/// Calls [`save`], then writes the note with its picture.
///
/// Anki copies the attached file to `collection.media`. The copy uses the
/// `chibipop-screenshot-` namespace and the saved file name, so a user can
/// match the two names.
pub fn save_and_add(
    image: &[u8],
    plan: &ShotPlan,
    anki: &AnkiConfig,
) -> Result<crate::anki::WriteResult> {
    save(image, plan)?;
    let picture = (!plan.picture_fields.is_empty()).then(|| crate::anki::NotePicture {
        data_base64: base64::engine::general_purpose::STANDARD.encode(image),
        filename: format!(
            "chibipop-screenshot-{}",
            plan.path.file_name().unwrap_or_default().to_string_lossy()
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
/// [`plan_add`] names files `<word>_<epoch seconds>.<png|jpg>`. Both bins pass this value
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
    

    const ROOT: &str = "/tmp/chibipop-shot-tests";

    fn cfg_on() -> ResolvedConfig {
        let mut cfg = ResolvedConfig::default();
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

    /// A 2901x1637 game window gave a 6.25 MB PNG that missed the AnkiConnect timeout.
    #[test]
    fn a_jpeg_plan_shrinks_a_large_capture_and_keeps_its_colors() {
        let mut cfg = cfg_on();
        cfg.actions.screenshot.image_format = ImageFormat::Jpeg;
        cfg.actions.screenshot.max_size = 1280;
        let plan = plan_add("猫", &HashMap::new(), &cfg, Path::new(ROOT), 1_700_000_000).unwrap();
        assert_eq!(Path::new(ROOT).join("猫_1700000000.jpg"), plan.path);

        let (w, h) = (2901, 1637);
        let bgra = [40u8, 120, 200, 0].repeat(w * h);
        let jpeg = encode(&bgra, w as i32, h as i32, &plan).unwrap();

        let mut decoder = zune_jpeg::JpegDecoder::new(std::io::Cursor::new(&jpeg));
        let rgb = decoder.decode().unwrap();
        assert_eq!(Some((1280, 722)), decoder.dimensions());
        for (got, want) in rgb[..3].iter().zip([200u8, 120, 40]) {
            assert!(got.abs_diff(want) <= 2, "BGR -> RGB: got {:?}", &rgb[..3]);
        }
    }

}
