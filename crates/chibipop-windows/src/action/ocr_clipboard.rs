//! This module captures OCR text from a selected region.

use crate::action::{Action, ActionContext, ActionOutcome, AppState, OcrRequest};
use crate::text::capture;
use anyhow::{anyhow, Context, Result};
use std::sync::mpsc;

/// Capture OCR text from a selected region for the clipboard.
pub struct OcrClipboardAction;

impl Action for OcrClipboardAction {
    fn name(&self) -> &str {
        "ocr-clipboard"
    }

    fn is_available(&self, _state: &AppState) -> bool {
        true
    }

    fn execute(&mut self, ctx: &mut ActionContext) -> Result<ActionOutcome> {
        let region = match ctx.selection.run() {
            Some(region) => region,
            None => return Ok(ActionOutcome::Cancelled),
        };
        let cap = capture::capture_upscaled_by(region, 2)?;
        let (result_tx, result_rx) = mpsc::channel();
        ctx.ocr_jobs.send(OcrRequest {
            bgra_buf: cap.buf,
            width: cap.w,
            height: cap.h,
            result_tx,
        })?;
        let lines = result_rx
            .recv()
            .context("OCR worker ended before returning text")?
            .map_err(|error| anyhow!(error))?;
        // Core owns the line-join rule in `chibipop::text::layout`.
        // This crate re-exports that module.
        // Both platform bins use that seam. Keep one implementation so both bins
        // return the same text.
        let text = crate::text::layout::join_lines(&lines);
        if text.is_empty() {
            return Ok(ActionOutcome::Cancelled);
        }
        Ok(ActionOutcome::TextCaptured { text })
    }
}
