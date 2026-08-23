//! Mining context screenshot.

use crate::action::{Action, ActionContext, ActionOutcome, AppState};
use crate::text::capture;
use anyhow::Result;
use std::path::PathBuf;

/// Captures a region for mining.
pub struct MiningContextScreenshot;

impl Action for MiningContextScreenshot {
    fn name(&self) -> &str {
        "screenshot"
    }

    fn is_available(&self, state: &AppState) -> bool {
        state.popup_visible
            && state
                .presentation
                .as_ref()
                .and_then(|p| p.top.as_ref())
                .is_some()
    }

    fn execute(&mut self, ctx: &mut ActionContext) -> Result<ActionOutcome> {
        let region = match ctx.selection.run() {
            Some(r) => r,
            None => return Ok(ActionOutcome::Cancelled),
        };

        let cap = capture::capture_upscaled_by(region, 1)?;

        let cfg = &ctx.config.screenshot;
        let save_dir = if std::path::Path::new(&cfg.save_dir).is_absolute() {
            PathBuf::from(&cfg.save_dir)
        } else {
            ctx.exe_dir.join(&cfg.save_dir)
        };

        Ok(ActionOutcome::ScreenshotCaptured {
            bgra_buf: cap.buf,
            width: cap.w,
            height: cap.h,
            save_dir,
        })
    }
}

/// Sanitizes a Windows filename.
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

    #[test]
    fn sanitizes_illegal_windows_chars() {
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
    fn collapses_consecutive_underscores() {
        assert_eq!("a_b", sanitize_filename("a///b"));
    }

    #[test]
    fn truncates_long_names() {
        let long = "あ".repeat(100);
        let result = sanitize_filename(&long);
        assert!(result.len() <= 60);
    }

    #[test]
    fn empty_falls_back() {
        assert_eq!("screenshot", sanitize_filename(""));
    }

    #[test]
    fn all_illegal_falls_back() {
        assert_eq!("screenshot", sanitize_filename("///"));
    }

    #[test]
    fn japanese_passes_through() {
        assert_eq!("宿舎", sanitize_filename("宿舎"));
    }
}
