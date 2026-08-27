//! Hotkey-triggered actions.

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

/// A hotkey-triggered behavior.
pub trait Action {
    /// Short, stable identifier.
    fn name(&self) -> &str;
    /// Can this run right now?
    fn is_available(&self, state: &AppState) -> bool;
    /// Runs the action.
    fn execute(&mut self, ctx: &mut ActionContext) -> Result<ActionOutcome>;
}

/// State snapshot for gating.
pub struct AppState<'a> {
    pub popup_visible: bool,
    pub presentation: Option<&'a Presentation>,
    pub anchor: Option<PhysRect>,
    pub anki_connected: bool,
}

/// What an action may use.
pub struct ActionContext<'a> {
    pub selection: &'a mut selection::RegionSelection,
    pub config: &'a crate::config::ActionsConfig,
    pub exe_dir: &'a Path,
    pub screenshot_tx: &'a mpsc::Sender<ScreenshotCommand>,
    /// Owned: it is two channel senders, cloned per dispatch.
    pub ocr_jobs: OcrJobs,
}

/// Pixels sent to the worker's OCR owner.
pub struct OcrRequest {
    pub bgra_buf: Vec<u8>,
    pub width: i32,
    pub height: i32,
    pub result_tx: mpsc::Sender<std::result::Result<Vec<OcrLine>, String>>,
}

/// The one-off OCR queue, and the worker's nudge.
///
/// The worker owns the only OCR engine (they are thread-affine) and runs
/// this queue from its `serve` hook - but it blocks on its own trigger
/// channel and cannot see this one, so queueing pixels is only half of
/// handing them over. One type, so the two halves cannot come apart.
#[derive(Clone)]
pub struct OcrJobs {
    tx: mpsc::Sender<OcrRequest>,
    nudge: ServeNudge,
}

impl OcrJobs {
    pub fn new(tx: mpsc::Sender<OcrRequest>, nudge: ServeNudge) -> Self {
        OcrJobs { tx, nudge }
    }

    /// Queue pixels, then wake the worker to read them.
    pub fn send(&self, request: OcrRequest) -> Result<()> {
        self.tx.send(request).context("sending OCR request")?;
        self.nudge.nudge();
        Ok(())
    }
}

impl ActionContext<'_> {
    /// Test-only, minimal context.
    #[cfg(test)]
    pub fn empty() -> ActionContext<'static> {
        let (tx, _rx) = mpsc::channel();
        ActionContext {
            selection: Box::leak(Box::new(selection::RegionSelection::dummy())),
            config: Box::leak(Box::new(crate::config::ActionsConfig::default())),
            exe_dir: Path::new("."),
            screenshot_tx: Box::leak(Box::new(tx)),
            // No worker behind it: a queued job is never served, which
            // is what a test without one wants.
            ocr_jobs: OcrJobs::new(mpsc::channel().0, ServeNudge::disconnected()),
        }
    }
}

/// How an action's run ended.
#[derive(Debug)]
pub enum ActionOutcome {
    Completed,
    /// Pixels ready for the pump.
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

/// Worker's input: raw pixels plus the whole add, already decided.
///
/// The rule lives in `chibipop::shot` (spec D4) - the pump plans, the
/// worker only encodes, writes and posts.
pub struct ScreenshotCommand {
    pub bgra_buf: Vec<u8>,
    pub width: i32,
    pub height: i32,
    pub plan: crate::shot::ShotPlan,
    /// The Anki section as the pump saw it, field map normalised.
    pub anki: crate::config::AnkiConfig,
    /// AnkiConnect answered a dupe check: without it the PNG is still
    /// written, but nothing is filed.
    pub anki_connected: bool,
}

/// Worker's output: what became of the picture.
///
/// Three answers, not two. A mining screenshot with Anki out of reach
/// still writes its PNG, and that is neither a card the popup may call
/// an add nor a failure - flattening it into an error flag is how the
/// popup came to claim notes Anki had never seen.
pub struct ScreenshotResult {
    pub expr: String,
    /// Where the PNG landed - all the filed-nothing diagnostic has to
    /// report, and the reason that case is worth a line at all.
    pub dir: PathBuf,
    /// `Ok(Some(id))` filed a note, `Ok(None)` wrote the picture and
    /// nothing else, `Err` got neither done.
    pub filed: Result<Option<i64>, String>,
}

impl ScreenshotResult {
    /// The add lifecycle this answer closes - `Some(failed)` - or
    /// `None` when there is none to close.
    ///
    /// Filing nothing is a picture with no card behind it, so nothing
    /// may claim the word was filed; the other two both answer the
    /// popup, which `start_add` marked adding before it authorised the
    /// picture. A screenshot taken with no popup up carries no word,
    /// so nothing is waiting on that one either.
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

/// Actions, indexed by hotkey.
#[derive(Default)]
pub struct ActionRegistry {
    actions: Vec<Option<Box<dyn Action>>>,
}

impl ActionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one action.
    pub fn register(&mut self, action: Box<dyn Action>) {
        self.actions.push(Some(action));
    }

    /// Registers an action at its hotkey slot.
    pub fn register_at(&mut self, index: usize, action: Box<dyn Action>) {
        if self.actions.len() <= index {
            self.actions.resize_with(index + 1, || None);
        }
        self.actions[index] = Some(action);
    }

    /// None if skipped or missing.
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
        // Anki never saw the word: calling this an add flips the
        // button and caches a dupe for a note that does not exist.
        assert_eq!(shot_result("猫", Ok(None)).add_failed(), None);
    }

    #[test]
    fn a_shot_that_never_landed_closes_the_add_as_failed() {
        assert_eq!(shot_result("猫", Err("disk full".into())).add_failed(), Some(true));
    }

    #[test]
    fn a_wordless_screenshot_has_no_add_to_close() {
        // The plain hotkey, pressed with no popup up: no expr, so no
        // lifecycle to close - not even when the write fails.
        assert_eq!(shot_result("", Ok(None)).add_failed(), None);
        assert_eq!(shot_result("", Err("disk full".into())).add_failed(), None);
    }
}
