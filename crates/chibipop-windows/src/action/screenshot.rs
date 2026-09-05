//! This module captures a Mining screenshot.

use crate::action::{Action, ActionContext, ActionOutcome, AppState};
use crate::action::selection::SelectionTarget;
use crate::config::ScreenshotMode;
use crate::text::capture;
use anyhow::{Context, Result};
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

/// Validate and convert a saved fixed region.
fn fixed_region(target: [i32; 4]) -> Result<crate::geom::PhysRect> {
    let rect = crate::geom::PhysRect {
        x: target[0],
        y: target[1],
        w: target[2],
        h: target[3],
    };
    if rect.w <= 0 || rect.h <= 0 {
        anyhow::bail!("the saved screenshot region has invalid geometry");
    }
    rect.x
        .checked_add(rect.w)
        .context("the saved screenshot region overflows horizontally")?;
    rect.y
        .checked_add(rect.h)
        .context("the saved screenshot region overflows vertically")?;
    Ok(rect)
}

/// Select or resolve the target for one screenshot.
///
/// A saved fixed target bypasses the selector. A fixed mode without a saved
/// target asks the user once, and the caller persists the returned target after
/// a successful capture.
pub fn select_target(
    selection: &mut crate::action::selection::RegionSelection,
    screenshot: &crate::config::ScreenshotConfig,
) -> Result<Option<SelectionTarget>> {
    let selected = match screenshot.capture_mode {
        ScreenshotMode::FixedRegion => match screenshot.fixed_region {
            Some(target) => Some(SelectionTarget::Region(fixed_region(target)?)),
            None => selection.run_target(screenshot.capture_mode),
        },
        ScreenshotMode::FixedWindow => screenshot
            .fixed_window
            .as_ref()
            .map(|target| -> Result<SelectionTarget> {
                let rect = crate::action::selection::resolve_window(target)?;
                Ok(SelectionTarget::Window {
                    rect,
                    target: target.clone(),
                })
            })
            .transpose()?
            .or_else(|| selection.run_target(screenshot.capture_mode)),
        ScreenshotMode::Region | ScreenshotMode::Window => {
            selection.run_target(screenshot.capture_mode)
        }
    };
    Ok(selected)
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
        let selected = select_target(&mut *ctx.selection, &ctx.config.screenshot)?;
        let Some(selected) = selected else {
            return Ok(ActionOutcome::Cancelled);
        };
        let cap = capture::capture_upscaled_by(selected.rect(), 1)?;

        Ok(ActionOutcome::ScreenshotCaptured {
            bgra_buf: cap.buf,
            width: cap.w,
            height: cap.h,
            save_dir: save_root(&ctx.config.screenshot, ctx.exe_dir),
            target: selected,
        })
    }
}
