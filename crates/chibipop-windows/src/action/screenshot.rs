//! Mining context screenshot.

use crate::action::{Action, ActionContext, ActionOutcome, AppState};
use crate::text::capture;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Where screenshots land on Windows: `save_dir` as written when it is
/// absolute, otherwise beside the executable. The Linux twin is
/// `Paths::screenshots_dir`, which resolves against XDG instead - which
/// is why this rule is the bin's and not core's.
pub fn save_root(cfg: &crate::config::ScreenshotConfig, exe_dir: &Path) -> PathBuf {
    if Path::new(&cfg.save_dir).is_absolute() {
        PathBuf::from(&cfg.save_dir)
    } else {
        exe_dir.join(&cfg.save_dir)
    }
}

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

        Ok(ActionOutcome::ScreenshotCaptured {
            bgra_buf: cap.buf,
            width: cap.w,
            height: cap.h,
            save_dir: save_root(&ctx.config.screenshot, ctx.exe_dir),
        })
    }
}
