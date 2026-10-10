//! This module defines actions that hotkeys trigger.

pub mod ocr_clipboard;
pub mod screenshot;
pub mod selected_text;
pub mod selection;

use crate::config::ProfileSession;
use crate::geom::PhysRect;
use crate::present::Presentation;
use crate::text::layout::OcrLine;
use anyhow::{Context, Result};
use chibipop::worker::ServeNudge;
use std::path::PathBuf;
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
    pub session: ProfileSession,
    /// This value owns two channel senders. The pump clones it for each dispatch.
    pub ocr_jobs: OcrJobs,
}

/// Pixel data that the Worker receives for OCR.
pub struct OcrRequest {
    pub bgra_buf: Vec<u8>,
    pub width: i32,
    pub height: i32,
    pub session: ProfileSession,
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
    pub fn empty<'a>(
        selection: &'a mut selection::RegionSelection,
        session: ProfileSession,
    ) -> ActionContext<'a> {
        ActionContext {
            selection,
            session,
            // No Worker reads this queue. Tests can use this context without a Worker.
            ocr_jobs: OcrJobs::new(mpsc::channel().0, ServeNudge::disconnected()),
        }
    }
}
/// The result of one action run.
#[derive(Debug)]
pub enum ActionOutcome {
    Completed,
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
/// This result has three states. Screenshot-on-add still writes its PNG when Anki is
/// unreachable.
/// That state is neither a card that the popup can report as added nor a failure.
/// A single error flag would make the popup claim that Anki saw a note when it did not.
pub struct ScreenshotResult {
    pub expr: String,
    /// Directory that the no-card diagnostic reports.
    /// The result includes it because the PNG can exist without a filed card.
    pub dir: PathBuf,
    /// The Worker files a note when the result is `Ok(Some(status))`.
    /// The Worker writes the picture without a note when the result is `Ok(None)`.
    /// `Err` means that an error stopped the operation. The picture can still exist.
    pub filed: Result<Option<crate::anki::WriteResult>, String>,
}

impl ScreenshotResult {
    /// Return the add result for this screenshot.
    ///
    /// Return a status when `expr` is non-empty and the Worker files the note.
    /// Return `None` when `expr` is empty or the Worker saves the PNG without a card.
    ///
    /// A saved PNG without a card does not mean that the word was filed.
    /// A filed card or an error closes the popup state that `start_add` marked before it sent
    /// the command.
    /// A screenshot without a popup has no word, so no add waits for it.
    pub fn write_status(&self) -> Option<crate::controller::NoteWriteStatus> {
        if self.expr.is_empty() {
            return None;
        }
        match self.filed {
            Ok(Some(crate::anki::WriteResult::Added(_))) => {
                Some(crate::controller::NoteWriteStatus::Added)
            }
            Ok(Some(crate::anki::WriteResult::Updated(_))) => {
                Some(crate::controller::NoteWriteStatus::Updated)
            }
            Ok(None) => Some(crate::controller::NoteWriteStatus::Failed),
            Err(_) => Some(crate::controller::NoteWriteStatus::Failed),
        }
    }

    pub fn add_failed(&self) -> Option<bool> {
        self.write_status().map(|status| matches!(status, crate::controller::NoteWriteStatus::Failed))
    }
}

/// Store one action for each configured bind action.
#[derive(Default)]
pub struct ActionRegistry {
    actions: Vec<(crate::config::BindAction, Box<dyn Action>)>,
}

impl ActionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an action for its configured bind type.
    pub fn register(&mut self, kind: crate::config::BindAction, action: Box<dyn Action>) {
        if let Some((_, current)) = self.actions.iter_mut().find(|(registered, _)| *registered == kind) {
            *current = action;
        } else {
            self.actions.push((kind, action));
        }
    }

    /// Return `None` when no action is registered or the action cannot run.
    /// Return an `ActionOutcome` when the action runs. Use `Failed` for an error.
    pub fn dispatch(
        &mut self,
        kind: crate::config::BindAction,
        state: &AppState,
        ctx: &mut ActionContext,
    ) -> Option<ActionOutcome> {
        let (_, action) = self.actions.iter_mut().find(|(registered, _)| *registered == kind)?;
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
    }

    impl Action for StubAction {
        fn name(&self) -> &str {
            "stub"
        }

        fn is_available(&self, _state: &AppState) -> bool {
            self.available
        }

        fn execute(&mut self, _ctx: &mut ActionContext) -> Result<ActionOutcome> {
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

    fn test_session() -> ProfileSession {
        crate::config::ProfileCatalog::new(&crate::config::Config::default(), &[])
            .unwrap()
            .session(None)
            .unwrap()
    }

    #[test]
    fn dispatch_uses_the_configured_action_type() {
        let mut registry = ActionRegistry::new();
        registry.register(crate::config::BindAction::OcrClipboard, Box::new(StubAction { available: true }));
        let mut selection = selection::RegionSelection::dummy();
        let mut ctx = ActionContext::empty(&mut selection, test_session());

        assert!(matches!(
            registry.dispatch(crate::config::BindAction::OcrClipboard, &empty_state(), &mut ctx),
            Some(ActionOutcome::Completed),
        ));
    }

    #[test]
    fn dispatch_does_not_route_one_bind_type_to_another() {
        let mut registry = ActionRegistry::new();
        registry.register(crate::config::BindAction::Search, Box::new(StubAction { available: true }));
        let mut selection = selection::RegionSelection::dummy();
        let mut ctx = ActionContext::empty(&mut selection, test_session());

        assert!(registry
            .dispatch(crate::config::BindAction::OcrClipboard, &empty_state(), &mut ctx)
            .is_none());
    }

    #[test]
    fn dispatch_skips_an_unavailable_configured_action() {
        let mut registry = ActionRegistry::new();
        registry.register(crate::config::BindAction::OcrClipboard, Box::new(StubAction { available: false }));
        let mut selection = selection::RegionSelection::dummy();
        let mut ctx = ActionContext::empty(&mut selection, test_session());

        assert!(registry
            .dispatch(crate::config::BindAction::OcrClipboard, &empty_state(), &mut ctx)
            .is_none());
    }
}

#[cfg(test)]
mod screenshot_result_tests {
    use super::*;

    fn shot_result(
        expr: &str,
        filed: Result<Option<crate::anki::WriteResult>, String>,
    ) -> ScreenshotResult {
        ScreenshotResult { expr: expr.to_string(), dir: PathBuf::from("shots"), filed }
    }

    #[test]
    fn a_filed_note_closes_the_add() {
        assert_eq!(
            shot_result("猫", Ok(Some(crate::anki::WriteResult::Added(1729)))).add_failed(),
            Some(false)
        );
    }

    #[test]
    fn filing_nothing_closes_the_add_as_failed() {
        assert_eq!(shot_result("猫", Ok(None)).add_failed(), Some(true));
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
