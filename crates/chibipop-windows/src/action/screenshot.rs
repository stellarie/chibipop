//! This module captures a Mining screenshot.

use crate::action::{Action, ActionContext, ActionOutcome, AppState};
use crate::text::capture;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Resolve `save_dir` for the Windows platform bin.
///
/// An absolute `save_dir` stays unchanged.
/// A relative `save_dir` resolves beside the executable.
/// The Linux platform bin uses `Paths::screenshots_dir`.
/// It keeps an absolute `save_dir` unchanged.
/// It resolves a relative `save_dir` beside the executable in `Portable` mode.
/// It resolves a relative `save_dir` under `data_dir` in `Explicit` or XDG mode.
/// Keep this rule in the platform bin because each platform resolves paths differently.
pub fn save_root(cfg: &crate::config::ScreenshotConfig, exe_dir: &Path) -> PathBuf {
    if Path::new(&cfg.save_dir).is_absolute() {
        PathBuf::from(&cfg.save_dir)
    } else {
        exe_dir.join(&cfg.save_dir)
    }
}

/// Capture a region for a Mining screenshot.
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

        Ok(ActionOutcome::ScreenshotCaptured {
            bgra_buf: cap.buf,
            width: cap.w,
            height: cap.h,
            save_dir: save_root(&ctx.config.screenshot, ctx.exe_dir),
        })
    }
}
