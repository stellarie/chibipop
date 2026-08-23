//! Hotkey-triggered actions.

pub mod screenshot;
pub mod selection;

use crate::geom::PhysRect;
use crate::present::Presentation;
use anyhow::Result;
use std::path::Path;

/// One hotkey-triggered behavior.
pub trait Action {
    /// Short, stable identifier.
    fn name(&self) -> &str;
    /// Can this run right now?
    fn is_available(&self, state: &AppState) -> bool;
    /// Runs the action.
    fn execute(&mut self, ctx: &mut ActionContext) -> Result<ActionOutcome>;
}

/// Read-only app snapshot for gating.
pub struct AppState<'a> {
    pub popup_visible: bool,
    pub presentation: Option<&'a Presentation>,
    pub anchor: Option<PhysRect>,
    pub anki_connected: bool,
}

/// What a running action may use.
pub struct ActionContext<'a> {
    pub exe_dir: &'a Path,
}

impl ActionContext<'_> {
    /// Test-only, minimal context.
    #[cfg(test)]
    pub fn empty() -> ActionContext<'static> {
        ActionContext {
            exe_dir: Path::new("."),
        }
    }
}

/// How an action's run ended.
#[derive(Debug)]
pub enum ActionOutcome {
    Completed,
    Cancelled,
    Failed(String),
}

/// Ordered, hotkey-indexed actions.
#[derive(Default)]
pub struct ActionRegistry {
    actions: Vec<Box<dyn Action>>,
}

impl ActionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends to the end of the list.
    pub fn register(&mut self, action: Box<dyn Action>) {
        self.actions.push(action);
    }

    /// `None` if unavailable or out of range.
    pub fn dispatch(
        &mut self,
        index: usize,
        state: &AppState,
        ctx: &mut ActionContext,
    ) -> Option<ActionOutcome> {
        let action = self.actions.get_mut(index)?;
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
}
