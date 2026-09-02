//! This module defines actions that hotkeys trigger.

pub mod ocr_clipboard;
pub mod screenshot;
pub mod selection;

use crate::geom::PhysRect;
use crate::present::Presentation;
use crate::text::layout::OcrLine;
use anyhow::{Context, Result};
use chibipop::worker::ServeNudge;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// Define one operation that `ActionRegistry` can dispatch from a hotkey.
pub trait Action {
    /// Return this action's short, stable identifier.
    fn name(&self) -> &str;
    /// Report whether this action can run with the current state.
    fn is_available(&self, state: &AppState) -> bool;
    /// Run this action with the supplied context.
    fn execute(&mut self, ctx: &mut ActionContext) -> Result<ActionOutcome>;
}

/// State that an action checks before it runs.
pub struct AppState<'a> {
    pub popup_visible: bool,
    pub presentation: Option<&'a Presentation>,
    pub anchor: Option<PhysRect>,
    pub anki_connected: bool,
}

/// Resources that an action can use while it runs.
pub struct ActionContext<'a> {
    pub selection: &'a mut selection::RegionSelection,
    pub config: &'a crate::config::ActionsConfig,
    pub exe_dir: &'a Path,
    pub screenshot_tx: &'a mpsc::Sender<ScreenshotCommand>,
    /// This value owns two channel senders. The pump clones it for each dispatch.
    pub ocr_jobs: OcrJobs,
}

/// Pixel data that the Worker receives for OCR.
pub struct OcrRequest {
    pub bgra_buf: Vec<u8>,
    pub width: i32,
    pub height: i32,
    pub result_tx: mpsc::Sender<std::result::Result<Vec<OcrLine>, String>>,
}

/// Connects the one-off OCR queue to the Worker.
///
/// The Worker owns the only `OcrEngine` because the engine is thread-affine.
/// Its `serve` hook reads this queue, but the Worker blocks on its own trigger channel
/// and cannot see this queue.
/// This type keeps the pixel queue and wake signal together.
#[derive(Clone)]
pub struct OcrJobs {
    tx: mpsc::Sender<OcrRequest>,
    nudge: ServeNudge,
}

impl OcrJobs {
    pub fn new(tx: mpsc::Sender<OcrRequest>, nudge: ServeNudge) -> Self {
        OcrJobs { tx, nudge }
    }

    /// Queue the pixels and wake the Worker to read them.
    pub fn send(&self, request: OcrRequest) -> Result<()> {
        self.tx.send(request).context("sending OCR request")?;
        self.nudge.nudge();
        Ok(())
    }
}

impl ActionContext<'_> {
    /// Return a minimal context for tests.
    #[cfg(test)]
    pub fn empty() -> ActionContext<'static> {
        let (tx, _rx) = mpsc::channel();
        ActionContext {
            selection: Box::leak(Box::new(selection::RegionSelection::dummy())),
            config: Box::leak(Box::new(crate::config::ActionsConfig::default())),
            exe_dir: Path::new("."),
            screenshot_tx: Box::leak(Box::new(tx)),
            // No Worker reads this queue. Tests can use this context without a Worker.
            ocr_jobs: OcrJobs::new(mpsc::channel().0, ServeNudge::disconnected()),
        }
    }
}

/// The result of one action run.
#[derive(Debug)]
pub enum ActionOutcome {
    Completed,
    /// Captured pixels that the pump can send to the Worker.
    ScreenshotCaptured {
        bgra_buf: Vec<u8>,
        width: i32,
        height: i32,
        save_dir: PathBuf,
    },
    TextCaptured {
        text: String,
    },
    Cancelled,
    Failed(String),
}

/// Input for the Worker. It contains raw pixels and the complete `ShotPlan`.
///
/// Core (`chibipop::shot`) owns the screenshot rule. The pump creates the plan.
/// The Worker only encodes the pixels, writes the PNG, and posts the note.
pub struct ScreenshotCommand {
    pub bgra_buf: Vec<u8>,
    pub width: i32,
    pub height: i32,
    pub plan: crate::shot::ShotPlan,
    /// The normalized Anki configuration that the pump uses for this command.
    pub anki: crate::config::AnkiConfig,
    /// True when AnkiConnect answered the duplicate check.
    /// If false, the Worker still writes the PNG but does not file a card.
    pub anki_connected: bool,
}

/// Result that the Worker returns after it handles the picture.
///
/// This result has three states. A Mining screenshot still writes its PNG when Anki is
/// unreachable.
/// That state is neither a card that the popup can report as added nor a failure.
/// A single error flag would make the popup claim that Anki saw a note when it did not.
pub struct ScreenshotResult {
    pub expr: String,
    /// Directory that the no-card diagnostic reports.
    /// The result includes it because the PNG can exist without a filed card.
    pub dir: PathBuf,
    /// The Worker files a note when the result is `Ok(Some(id))`.
    /// The Worker writes the picture without a note when the result is `Ok(None)`.
    /// `Err` means that an error stopped the operation. The picture can still exist.
    pub filed: Result<Option<i64>, String>,
}

impl ScreenshotResult {
    /// Return the add result for this screenshot.
    ///
    /// Return `Some(false)` when `expr` is non-empty and the Worker files the note.
    /// Return `Some(true)` when `expr` is non-empty and the Worker reports an error.
    /// Return `None` when `expr` is empty or the Worker saves the PNG without a card.
    ///
    /// A saved PNG without a card does not mean that the word was filed.
    /// A filed card or an error closes the popup state that `start_add` marked before it sent
    /// the command.
    /// A screenshot without a popup has no word, so no add waits for it.
    pub fn add_failed(&self) -> Option<bool> {
        if self.expr.is_empty() {
            return None;
        }
        match self.filed {
            Ok(Some(_)) => Some(false),
            Ok(None) => None,
            Err(_) => Some(true),
        }
    }
}

/// Store actions by their hotkey index.
#[derive(Default)]
pub struct ActionRegistry {
    actions: Vec<Option<Box<dyn Action>>>,
}

impl ActionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an action after the current last slot.
    pub fn register(&mut self, action: Box<dyn Action>) {
        self.actions.push(Some(action));
    }

    /// Place an action at a hotkey index.
    pub fn register_at(&mut self, index: usize, action: Box<dyn Action>) {
        if self.actions.len() <= index {
            self.actions.resize_with(index + 1, || None);
        }
        self.actions[index] = Some(action);
    }

    /// Return `None` when the slot has no action or the action cannot run.
    /// Return an `ActionOutcome` when the action runs. Use `Failed` for an error.
    pub fn dispatch(
        &mut self,
        index: usize,
        state: &AppState,
        ctx: &mut ActionContext,
    ) -> Option<ActionOutcome> {
        let action = self.actions.get_mut(index)?.as_mut()?;
        if !action.is_available(state) {
            return None;
        }
        match action.execute(ctx) {
            Ok(outcome) => Some(outcome),
            Err(e) => Some(ActionOutcome::Failed(format!("{e:#}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubAction {
        available: bool,
        called: bool,
    }

    impl Action for StubAction {
        fn name(&self) -> &str {
            "stub"
        }

        fn is_available(&self, _state: &AppState) -> bool {
            self.available
        }

        fn execute(&mut self, _ctx: &mut ActionContext) -> Result<ActionOutcome> {
            self.called = true;
            Ok(ActionOutcome::Completed)
        }
    }

    fn empty_state() -> AppState<'static> {
        AppState {
            popup_visible: false,
            presentation: None,
            anchor: None,
            anki_connected: false,
        }
    }

    #[test]
    fn dispatch_fires_when_available() {
        let mut reg = ActionRegistry::new();
        reg.register(Box::new(StubAction {
            available: true,
            called: false,
        }));
        let mut ctx = ActionContext::empty();
        let outcome = reg.dispatch(0, &empty_state(), &mut ctx);
        assert!(matches!(outcome, Some(ActionOutcome::Completed)));
    }

    #[test]
    fn dispatch_skips_when_unavailable() {
        let mut reg = ActionRegistry::new();
        reg.register(Box::new(StubAction {
            available: false,
            called: false,
        }));
        let mut ctx = ActionContext::empty();
        let outcome = reg.dispatch(0, &empty_state(), &mut ctx);
        assert!(outcome.is_none());
    }

    #[test]
    fn dispatch_out_of_bounds_returns_none() {
        let mut reg = ActionRegistry::new();
        let mut ctx = ActionContext::empty();
        let outcome = reg.dispatch(5, &empty_state(), &mut ctx);
        assert!(outcome.is_none());
    }

    #[test]
    fn register_at_preserves_unregistered_slots() {
        let mut reg = ActionRegistry::new();
        reg.register_at(
            2,
            Box::new(StubAction {
                available: true,
                called: false,
            }),
        );
        let mut ctx = ActionContext::empty();
        assert!(reg.dispatch(1, &empty_state(), &mut ctx).is_none());
        assert!(matches!(
            reg.dispatch(2, &empty_state(), &mut ctx),
            Some(ActionOutcome::Completed)
        ));
    }

    fn shot_result(expr: &str, filed: Result<Option<i64>, String>) -> ScreenshotResult {
        ScreenshotResult { expr: expr.to_string(), dir: PathBuf::from("shots"), filed }
    }

    #[test]
    fn a_filed_note_closes_the_add() {
        assert_eq!(shot_result("猫", Ok(Some(1729))).add_failed(), Some(false));
    }

    #[test]
    fn filing_nothing_is_not_an_add() {
        // Anki did not see the word. This is not an add.
        // An add would change the button and cache a duplicate for a note that does not exist.
        assert_eq!(shot_result("猫", Ok(None)).add_failed(), None);
    }

    #[test]
    fn a_shot_that_never_landed_closes_the_add_as_failed() {
        assert_eq!(shot_result("猫", Err("disk full".into())).add_failed(), Some(true));
    }

    #[test]
    fn a_wordless_screenshot_has_no_add_to_close() {
        // The plain hotkey has no popup, so it has no `expr` or add lifecycle to close.
        // A failed write does not change this result.
        assert_eq!(shot_result("", Ok(None)).add_failed(), None);
        assert_eq!(shot_result("", Err("disk full".into())).add_failed(), None);
    }
}
