//! OCR selected pixels into the clipboard.

use crate::action::{Action, ActionContext, ActionOutcome, AppState, OcrRequest};
use crate::text::capture;
use anyhow::{anyhow, Context, Result};
use std::sync::mpsc;

/// Copies OCR text from a selected region.
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
        // The joining rule is core's (`chibipop::text::layout`, reached
        // through this crate's re-export): the Linux daemon copies the
        // same text out of the same seam, and two implementations would
        // be two answers.
        let text = crate::text::layout::join_lines(&lines);
        if text.is_empty() {
            return Ok(ActionOutcome::Cancelled);
        }
        Ok(ActionOutcome::TextCaptured { text })
    }
}
