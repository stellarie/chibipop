//! This module captures OCR text from a selected region.

use crate::action::{Action, ActionContext, ActionOutcome, AppState, OcrRequest};
use crate::text::capture;
use anyhow::{anyhow, Result};
use std::sync::mpsc;
use std::time::Duration;

fn wait_lines(
    results: mpsc::Receiver<std::result::Result<Vec<crate::text::layout::OcrLine>, String>>,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Vec<crate::text::layout::OcrLine>>> {
    loop {
        if cancelled() { return Ok(None); }
        match results.recv_timeout(Duration::from_millis(20)) {
            Ok(_) if cancelled() => return Ok(None),
            Ok(result) => return result.map(Some).map_err(|error| anyhow!(error)),
            Err(mpsc::RecvTimeoutError::Timeout) => {},
            Err(mpsc::RecvTimeoutError::Disconnected) => anyhow::bail!("OCR worker ended before returning text"),
        }
    }
}

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
        let cancellation = crate::input::hooks::EscapeCancellation::new();
        let region = match ctx.selection.run() {
            Some(region) => region,
            None => return Ok(ActionOutcome::Cancelled),
        };
        let cap = capture::capture_upscaled_by(region, 2)?;
        if cancellation.cancelled() { return Ok(ActionOutcome::Cancelled); }
        let (result_tx, result_rx) = mpsc::channel();
        ctx.ocr_jobs.send(OcrRequest {
            bgra_buf: cap.buf,
            width: cap.w,
            height: cap.h,
            result_tx,
        })?;
        let Some(lines) = wait_lines(result_rx, || cancellation.cancelled())? else {
            return Ok(ActionOutcome::Cancelled);
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_discards_ready_and_late_ocr_results() {
        let (sender, receiver) = mpsc::channel();
        sender.send(Ok(Vec::new())).unwrap();
        assert!(wait_lines(receiver, || true).unwrap().is_none());
        let (sender, receiver) = mpsc::channel();
        assert!(wait_lines(receiver, || true).unwrap().is_none());
        assert!(sender.send(Ok(Vec::new())).is_err());
    }

    #[test]
    fn waiting_ocr_observes_cancellation_and_worker_failure() {
        let (_sender, receiver) = mpsc::channel();
        let checks = std::cell::Cell::new(0);
        assert!(wait_lines(receiver, || { checks.set(checks.get() + 1); checks.get() > 1 }).unwrap().is_none());
        let (sender, receiver) = mpsc::channel();
        sender.send(Err("worker failed".into())).unwrap();
        assert!(wait_lines(receiver, || false).unwrap_err().to_string().contains("worker failed"));
    }
}
