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
        ctx.ocr_tx
            .send(OcrRequest {
                bgra_buf: cap.buf,
                width: cap.w,
                height: cap.h,
                result_tx,
            })
            .context("sending OCR request")?;
        let lines = result_rx
            .recv()
            .context("OCR worker ended before returning text")?
            .map_err(|error| anyhow!(error))?;
        let text = join_lines(&lines);
        if text.is_empty() {
            return Ok(ActionOutcome::Cancelled);
        }
        Ok(ActionOutcome::TextCaptured { text })
    }
}

/// Joins OCR words without spaces.
fn join_lines(lines: &[crate::text::layout::OcrLine]) -> String {
    lines
        .iter()
        .map(|line| {
            line.words
                .iter()
                .map(|word| word.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::PhysRect;
    use crate::text::layout::{OcrLine, OcrWord};

    fn word(text: &str) -> OcrWord {
        OcrWord {
            text: text.to_string(),
            rect: PhysRect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
        }
    }

    #[test]
    fn joins_words_and_lines() {
        let lines = vec![
            OcrLine {
                words: vec![word("これは"), word("テスト")],
            },
            OcrLine {
                words: vec![word("二行目")],
            },
        ];
        assert_eq!("これはテスト\n二行目", join_lines(&lines));
    }

    #[test]
    fn empty_lines_produce_empty_text() {
        assert_eq!("", join_lines(&[]));
    }
}
