//! Hotkey-triggered actions.

pub mod ocr_clipboard;
pub mod screenshot;
pub mod selection;

use crate::geom::PhysRect;
use crate::present::Presentation;
use crate::text::layout::OcrLine;
use anyhow::Result;
use std::collections::HashMap;
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
    pub ocr_tx: &'a mpsc::Sender<OcrRequest>,
}

/// Pixels sent to the worker's OCR owner.
pub struct OcrRequest {
    pub bgra_buf: Vec<u8>,
    pub width: i32,
    pub height: i32,
    pub result_tx: mpsc::Sender<std::result::Result<Vec<OcrLine>, String>>,
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
            ocr_tx: Box::leak(Box::new(mpsc::channel().0)),
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

/// Worker's input.
pub struct ScreenshotCommand {
    pub bgra_buf: Vec<u8>,
    pub width: i32,
    pub height: i32,
    pub save_path: PathBuf,
    pub expr: String,
    pub fields: HashMap<String, String>,
    pub field_map: Vec<crate::config::FieldMapping>,
    pub anki_url: String,
    pub anki_deck: String,
    pub anki_model: String,
    pub anki_connected: bool,
}

/// Worker's output.
pub struct ScreenshotResult {
    pub expr: String,
    pub err: Option<String>,
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
}
