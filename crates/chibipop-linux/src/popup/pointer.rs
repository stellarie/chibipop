//! Popup pointer input (ADR-0003, ADR-0004): the wheel, entry clicks,
//! the back affordance and the Anki slot, all served from the panel's
//! own input region.
//!
//! Wayland has no machine-wide mouse hook, so the Windows `WH_MOUSE_LL`
//! swallow is replaced by `wl_pointer` events on the layer surface
//! itself. What Windows arms per dispatch tick (`SetScrollArmed`,
//! `SetClickArmed`) is a property of the surface here: the input region
//! is the whole panel while the popup is shown and empty while it is
//! hidden ([`InputRegion`]), so a hidden popup is click-through and a
//! shown one needs no arming at all. `keyboard_interactivity` stays
//! `none` - the popup takes pointer input and never a key.
//!
//! Three coordinate spaces meet here, and the order matters:
//!
//! 1. `wl_pointer` speaks surface-local **logical** units, as f64.
//! 2. The panel - and everything core measured - is in **physical**
//!    pixels at the output's fractional scale, so a click is
//!    `floor(pos * scale)`. That is the Windows renderer's `hit_test`
//!    run the other way round: it lays out in DIPs and *divides* by the
//!    DPI scale.
//! 3. A [`PopupScene`]'s own y is **unscrolled** - the painter subtracts
//!    the scroll offset and core culls what falls off the panel - so a
//!    scene lookup adds the scroll back on.
//!
//! [`HitScene`] carries exactly those three facts plus the targets, and
//! it is rebuilt by every repaint (`Popup::draw`), so what the pointer
//! resolves against can never drift from what is on screen.

use super::place::Visibility;
use super::{forward, Popup};
use crate::daemon::App;
use chibipop::controller::HitAction;
use chibipop::geom::PhysPoint;
use chibipop::ui::layout::{HitTarget, PopupScene, SceneRect};
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::globals::GlobalData;
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use smithay_client_toolkit::seat::pointer::{
    AxisScroll, PointerData, PointerEvent, PointerEventKind, PointerHandler, BTN_LEFT,
};
use smithay_client_toolkit::seat::{Capability, SeatData, SeatHandler, SeatState};
use std::sync::Arc;
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;

/// One wheel notch in `wl_pointer.axis_value120`'s units - the same 120
/// `WHEEL_DELTA` uses, so both platforms bank the same arithmetic and a
/// high-resolution wheel scrolls the popup by the same amount on each.
const NOTCH_120: i32 = 120;

/// Logical pixels of *continuous* scroll worth one notch.
///
/// Only a touchpad (or a free-spinning wheel behind a compositor that
/// sends neither `axis_value120` nor `axis_discrete`) reaches this rung:
/// those sources report finger movement and nothing else. Ten units is
/// what one wheel click's companion `axis` value carries, so the
/// fallback lands on the same scale as the notch rungs instead of
/// inventing its own feel.
const NOTCH_PX: f64 = 10.0;

/// What a point on the panel resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit {
    /// A scene target: expand, drill down, or back.
    Action(HitAction),
    /// The Anki slot. Core reserves it and the painter fills it
    /// (ADR-0004); a click on it is the Controller's `AddRequested`.
    Anki,
}

/// One popup-local interaction, ready to become a Controller `Event`.
#[derive(Debug, Clone, PartialEq)]
pub enum Interaction {
    /// Whole wheel notches, in core's sign: wheel-up is positive.
    Scroll { notches: i32 },
    /// A left click on the panel. `hit` is `None` for a click that
    /// landed on no target, which the Controller ignores.
    Click { local: PhysPoint, hit: Option<HitAction> },
    /// A left click on the Anki slot.
    Anki { local: PhysPoint },
}

/// The scene under the pointer, as painted.
///
/// Rebuilt by every repaint, which is what keeps hit targets honest
/// across a scroll (the offset changes) and a scale change (every rect
/// does).
#[derive(Debug, Clone, PartialEq)]
pub struct HitScene {
    /// Which surface this frame belongs to.
    pub panel: usize,
    /// In paint order - main column, then the side column - because
    /// resolution takes the first match.
    pub targets: Vec<HitTarget>,
    /// The Anki strip, when core reserved one. Painted unscrolled, so
    /// it is tested unscrolled.
    pub anki: Option<SceneRect>,
    /// Where the body view ends. Below it there is only the strip.
    pub view_h: f32,
    /// The scroll this frame was painted with, physical px.
    pub scroll: f32,
    /// The fractional scale it was rastered at.
    pub scale: f64,
}

impl HitScene {
    /// Take one painted frame's targets.
    pub fn of(panel: usize, scene: &PopupScene, scroll: f32, scale: f64) -> HitScene {
        HitScene {
            panel,
            targets: scene.hit_targets(),
            anki: scene.anki.as_ref().map(|a| a.rect),
            view_h: scene.view_h,
            scroll,
            scale: if scale > 0.0 { scale } else { 1.0 },
        }
    }

    /// Surface-local logical -> panel-local physical, which is the
    /// space `Event::Clicked` is counted in.
    ///
    /// Floor, not round: the answer names the pixel the pointer is
    /// inside, and a pointer at 0.9 logical px is in physical pixel 1
    /// at 1.5x, not 2.
    pub fn local(&self, pos: (f64, f64)) -> PhysPoint {
        PhysPoint {
            x: (pos.0 * self.scale).floor() as i32,
            y: (pos.1 * self.scale).floor() as i32,
        }
    }

    /// The action at a panel-local physical point, if any.
    pub fn hit(&self, local: PhysPoint) -> Option<Hit> {
        let (x, y) = (local.x as f32, local.y as f32);
        // The strip first, and unscrolled: it is painted after the body
        // and never moves, so a target scrolled under it must not win.
        if let Some(strip) = self.anki {
            if within(strip.x, strip.w, x) && within(strip.y, strip.h, y) {
                return Some(Hit::Anki);
            }
        }
        if y >= self.view_h {
            return None;
        }
        // Scene y is unscrolled; the painter subtracted the offset, so
        // the lookup adds it back.
        let scene_y = y + self.scroll;
        self.targets
            .iter()
            .find(|t| {
                let across = match (t.x, t.w) {
                    (Some(tx), Some(tw)) => within(tx, tw, x),
                    // `None` spans the panel, however wide it is.
                    _ => true,
                };
                across && within(t.y, t.h, scene_y)
            })
            .map(|t| Hit::Action(t.action.clone()))
    }
}

/// Half-open, like every rect test in core.
fn within(origin: f32, len: f32, at: f32) -> bool {
    at >= origin && at < origin + len
}

/// The surface's pointer input region for one commit.
///
/// Surface-local logical units, because `wl_region` is. The two states
/// are the whole ADR-0003 bargain: while the popup is up the pointer
/// belongs to it (the app underneath loses hover, which the popup was
/// covering anyway), and the moment it is hidden every event falls
/// straight through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputRegion {
    /// The whole panel accepts pointer input.
    Panel { w: i32, h: i32 },
    /// The empty region: click-through.
    Empty,
}

impl InputRegion {
    /// What one panel's frame is owed, given what is on screen.
    ///
    /// A frame for a surface that is not the one showing gets the empty
    /// region: a coalesced repaint can outlive the show that queued it,
    /// and a stale buffer must never take input.
    pub fn of(vis: Visibility, panel: usize, logical: (i32, i32)) -> InputRegion {
        match vis.shown() {
            Some(shown) if shown.output == panel => {
                InputRegion::Panel { w: logical.0.max(1), h: logical.1.max(1) }
            }
            _ => InputRegion::Empty,
        }
    }

    /// The box to hand `wl_region.add`, or `None` for the empty region.
    pub fn rect(self) -> Option<(i32, i32, i32, i32)> {
        match self {
            InputRegion::Panel { w, h } => Some((0, 0, w, h)),
            InputRegion::Empty => None,
        }
    }
}

/// Whole notches out, sub-notch remainder banked.
///
/// The Windows hook's `take_whole_notches`, with one sign flip: a
/// `wl_pointer` axis counts positive *downward* (the content moves up)
/// while `WM_MOUSEWHEEL` and core's `Event::Scrolled` count wheel-up
/// positive.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Wheelbank {
    /// 120ths of a notch, signed, as the compositor sent them.
    banked: i32,
}

impl Wheelbank {
    /// One axis frame's vertical scroll as whole notches.
    pub fn take(&mut self, axis: &AxisScroll) -> i32 {
        let sent = hundred_twentieths(axis);
        if sent == 0 {
            return 0;
        }
        let total = self.banked.saturating_add(sent);
        let remainder = total % NOTCH_120;
        self.banked = remainder;
        -((total - remainder) / NOTCH_120)
    }

    /// Drop the bank: `Command::DiscardScroll`, and every hide.
    pub fn discard(&mut self) {
        self.banked = 0;
    }
}

/// One frame's vertical scroll in 120ths, by the best rung it carries.
///
/// A wheel frame carries `axis_value120` (v8+) or `axis_discrete`
/// (v5-7) *beside* the continuous `axis` value, so the rungs are
/// exclusive on purpose - reading both would count every click twice.
/// `relative_direction` is deliberately ignored: it reports whether the
/// compositor already inverted the delta for natural scrolling, which
/// is the user's preference and must be honoured, not undone.
fn hundred_twentieths(axis: &AxisScroll) -> i32 {
    if axis.value120 != 0 {
        axis.value120
    } else if axis.discrete != 0 {
        axis.discrete.saturating_mul(NOTCH_120)
    } else {
        (axis.absolute / NOTCH_PX * f64::from(NOTCH_120)).round() as i32
    }
}

/// Where the pointer is, in the popup's terms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Focus {
    pub panel: usize,
    /// Surface-local logical, as `wl_pointer` reports it.
    pub pos: (f64, f64),
}

/// The popup's pointer: the seat objects, the focus, and the bank.
pub struct Pointer {
    seats: SeatState,
    /// `wp_cursor_shape_v1` where advertised. Where it is not, the
    /// cursor is left alone rather than us loading XCursor themes
    /// (ADR-0004).
    shapes: Option<CursorShapeManager>,
    pointer: Option<WlPointer>,
    device: Option<WpCursorShapeDeviceV1>,
    /// The last `enter` serial, which `set_shape` has to quote.
    serial: Option<u32>,
    focus: Option<Focus>,
    bank: Wheelbank,
    /// The last shape asked for, so a hover across one target costs one
    /// request rather than one per motion event.
    shape: Option<Shape>,
    /// `popup.scroll_popup`. Windows arms its hook per tick; the same
    /// setting gates the wheel here.
    wheel_enabled: bool,
    /// `CHIBIPOP_POINTER_SCRIPT`, parsed into passes.
    script: Vec<Vec<Step>>,
    /// How many passes have run. One pass per fresh frame, and never
    /// twice.
    pass: usize,
    /// A fresh frame is owed a pass.
    armed: bool,
}

impl Pointer {
    pub fn new(seats: SeatState, shapes: Option<CursorShapeManager>, wheel_enabled: bool) -> Pointer {
        Pointer {
            seats,
            shapes,
            pointer: None,
            device: None,
            serial: None,
            focus: None,
            bank: Wheelbank::default(),
            shape: None,
            wheel_enabled,
            script: Vec::new(),
            pass: 0,
            armed: false,
        }
    }

    pub fn seats(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    pub fn shapes_advertised(&self) -> bool {
        self.shapes.is_some()
    }

    pub fn focus(&self) -> Option<Focus> {
        self.focus
    }

    pub fn set_wheel_enabled(&mut self, on: bool) {
        self.wheel_enabled = on;
        if !on {
            self.bank.discard();
        }
    }

    pub fn set_script(&mut self, script: Vec<Vec<Step>>) {
        self.script = script;
    }

    /// The next pass, taken. `None` when the script is exhausted, or
    /// when no fresh frame has armed one.
    pub fn take_pass(&mut self) -> Option<Vec<Step>> {
        if !self.armed {
            return None;
        }
        self.armed = false;
        let pass = self.script.get(self.pass)?.clone();
        self.pass += 1;
        Some(pass)
    }

    /// A fresh frame is on the way: arm the next pass. Cheap and
    /// idempotent, and a no-op without a script.
    pub fn arm_script(&mut self) {
        self.armed = !self.script.is_empty();
    }

    pub fn discard_scroll(&mut self) {
        self.bank.discard();
    }

    /// The pointer object arrived, or went away with the seat's
    /// capability. The shape device is per-pointer, so it goes too.
    pub fn set_pointer(&mut self, pointer: Option<WlPointer>) {
        if let Some(old) = self.device.take() {
            old.destroy();
        }
        if let Some(old) = self.pointer.take() {
            old.release();
        }
        self.pointer = pointer;
        self.serial = None;
        self.focus = None;
        self.shape = None;
    }

    /// The `wp_cursor_shape_v1` device for the pointer just taken.
    pub fn attach_shape_device(&mut self, device: WpCursorShapeDeviceV1) {
        self.device = Some(device);
    }

    /// The manager and the live pointer, which is what creating that
    /// device needs. `None` when either is missing.
    pub fn shape_source(&self) -> Option<(&CursorShapeManager, &WlPointer)> {
        Some((self.shapes.as_ref()?, self.pointer.as_ref()?))
    }

    /// The pointer crossed onto a panel.
    ///
    /// `serial` is the `enter` event's, which `set_shape` has to quote;
    /// a scripted pass has no real enter behind it and passes `None`,
    /// so it drives every hit the same way but never asks the
    /// compositor to change a cursor it was not given a serial for.
    pub fn enter(&mut self, panel: usize, pos: (f64, f64), serial: Option<u32>) {
        self.serial = serial;
        self.focus = Some(Focus { panel, pos });
        // A fresh crossing starts with no banked sub-notch: the last
        // partial flick belonged to whatever the pointer was over.
        self.bank.discard();
        self.shape = None;
    }

    pub fn leave(&mut self, panel: usize) {
        if self.focus.is_some_and(|f| f.panel == panel) {
            self.focus = None;
            self.bank.discard();
            self.shape = None;
        }
    }

    pub fn motion(&mut self, pos: (f64, f64)) {
        if let Some(focus) = self.focus.as_mut() {
            focus.pos = pos;
        }
    }

    /// A wheel frame over the panel. `None` when nothing whole came of
    /// it, or when `popup.scroll_popup` is off.
    pub fn wheel(&mut self, axis: &AxisScroll) -> Option<Interaction> {
        if !self.wheel_enabled || self.focus.is_none() {
            return None;
        }
        let notches = self.bank.take(axis);
        (notches != 0).then_some(Interaction::Scroll { notches })
    }

    /// Ask the compositor for the hand over a target and the default
    /// cursor everywhere else. A no-op without `wp_cursor_shape_v1`.
    pub fn set_shape(&mut self, over_target: bool) {
        let want = if over_target { Shape::Pointer } else { Shape::Default };
        if self.shape == Some(want) {
            return;
        }
        let (Some(device), Some(serial)) = (self.device.as_ref(), self.serial) else { return };
        device.set_shape(serial, want);
        self.shape = Some(want);
    }
}

/// One left click, resolved against the frame on screen.
///
/// Free of the Wayland half so the resolution is testable on plain
/// data: the same call serves a real `wl_pointer.button` and a scripted
/// one.
pub fn click(hits: &HitScene, at: (f64, f64)) -> Interaction {
    let local = hits.local(at);
    match hits.hit(local) {
        Some(Hit::Anki) => Interaction::Anki { local },
        Some(Hit::Action(action)) => Interaction::Click { local, hit: Some(action) },
        None => Interaction::Click { local, hit: None },
    }
}

// ---- the scripted pass ----

/// One step of `CHIBIPOP_POINTER_SCRIPT`.
///
/// Coordinates are surface-local logical units, exactly what
/// `wl_pointer` delivers, and `Wheel` is a raw `axis_value120` (which
/// counts positive downwards). The script exists so the handlers can be
/// driven on a live compositor without synthesizing any seat input -
/// the human's own pointer is never touched, never warped, and never
/// robbed of focus - and it enters through the same entry points a real
/// frame does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    Enter(f64, f64),
    Motion(f64, f64),
    Click(f64, f64),
    Wheel(i32),
    Leave,
    /// Log the frame's hit targets in logical coordinates, so the next
    /// run aims at the real scene rather than at a guess.
    Dump,
}

pub const SCRIPT_ENV: &str = "CHIBIPOP_POINTER_SCRIPT";

/// Passes, `|`-separated; steps inside one, `;`-separated.
///
/// One pass runs per fresh frame the popup is armed for, so a script
/// can answer content its own earlier pass asked for - clicking a
/// drill-down in pass one and the back affordance it produces in pass
/// two. A pass never repeats, which is what keeps a click that changes
/// the scene from driving itself in a circle.
pub fn parse_script(text: &str) -> (Vec<Vec<Step>>, Vec<String>) {
    let mut passes = Vec::new();
    let mut rejects = Vec::new();
    for chunk in text.split('|') {
        let mut steps = Vec::new();
        for raw in chunk.split(';') {
            let item = raw.trim();
            if item.is_empty() {
                continue;
            }
            let (verb, arg) = match item.split_once(':') {
                Some((verb, arg)) => (verb.trim(), arg.trim()),
                None => (item, ""),
            };
            let step = match verb {
                "enter" => point(arg).map(|(x, y)| Step::Enter(x, y)),
                "motion" => point(arg).map(|(x, y)| Step::Motion(x, y)),
                "click" => point(arg).map(|(x, y)| Step::Click(x, y)),
                "wheel" => arg.parse().ok().map(Step::Wheel),
                "leave" if arg.is_empty() => Some(Step::Leave),
                "dump" if arg.is_empty() => Some(Step::Dump),
                _ => None,
            };
            match step {
                Some(step) => steps.push(step),
                None => {
                    rejects.push(format!("pointer: script step {item:?} is not a step - ignored"));
                }
            }
        }
        if !steps.is_empty() {
            passes.push(steps);
        }
    }
    (passes, rejects)
}

fn point(arg: &str) -> Option<(f64, f64)> {
    let (x, y) = arg.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// The script from the environment. Absent env, no passes and no notes.
pub fn script_from_env() -> (Vec<Vec<Step>>, Vec<String>) {
    match std::env::var(SCRIPT_ENV) {
        Ok(text) => {
            let (passes, mut notes) = parse_script(&text);
            notes.push(format!(
                "pointer: {SCRIPT_ENV} armed with {} pass(es), {} step(s)",
                passes.len(),
                passes.iter().map(Vec::len).sum::<usize>(),
            ));
            (passes, notes)
        }
        Err(_) => (Vec::new(), Vec::new()),
    }
}

/// One `wl_pointer` frame, translated into the popup's entry points.
///
/// Frames batch related events (SCTK merges the axis pieces into one),
/// and a frame may carry a leave and an enter together when the pointer
/// crosses between two of our surfaces, so the order inside it is kept.
pub fn frame(popup: &mut Popup, events: &[PointerEvent]) -> Vec<Interaction> {
    let mut out = Vec::new();
    for event in events {
        let Some(panel) = popup.panel_of(&event.surface) else { continue };
        match &event.kind {
            PointerEventKind::Enter { serial } => {
                popup.pointer_enter(panel, event.position, Some(*serial));
            }
            PointerEventKind::Leave { .. } => popup.pointer_leave(panel),
            PointerEventKind::Motion { .. } => popup.pointer_motion(event.position),
            // Press, not release: the Windows hook fires on
            // `WM_LBUTTONDOWN`, so the popup answers on the way down
            // too.
            PointerEventKind::Press { button, .. } if *button == BTN_LEFT => {
                out.extend(popup.pointer_button(event.position));
            }
            PointerEventKind::Axis { vertical, .. } => {
                out.extend(popup.pointer_wheel(vertical));
            }
            _ => {}
        }
    }
    out
}

// ---- the dispatch plumbing ----

forward!(WlSeat, SeatData);
forward!(WlPointer, PointerData<()>);
forward!(WpCursorShapeManagerV1, GlobalData);
forward!(WpCursorShapeDeviceV1, GlobalData);

/// The seat's capabilities are how a pointer is learned about - at
/// startup and when one is plugged in later. Nothing here ever asks for
/// a keyboard: `keyboard_interactivity` is `none` and the popup has no
/// use for one (ADR-0003).
impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        self.popup_mut().seats()
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<App>, _: WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<App>,
        seat: WlSeat,
        capability: Capability,
    ) {
        if capability != Capability::Pointer {
            return;
        }
        let pointer = self.popup_mut().seats().get_pointer(qh, &seat);
        match pointer {
            Ok(pointer) => self.popup_mut().pointer_arrived(pointer),
            Err(e) => self
                .popup_mut()
                .note(format!("pointer: the seat refused a pointer: {e}")),
        }
        self.flush_popup_notes();
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<App>,
        _: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            self.popup_mut().pointer_gone();
            self.flush_popup_notes();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<App>, _: WlSeat) {}
}

/// Every `wl_pointer` frame in the daemon lands here - the popup's
/// pointer and, while a region pick is up, that pick's own - because
/// SCTK has one handler per state. `App` sorts them by surface
/// identity; this impl deliberately knows nothing about which is which.
impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<App>,
        _: &WlPointer,
        events: &[PointerEvent],
    ) {
        App::pointer_frame(self, events);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::popup::place::{Placement, Shown};
    use chibipop::geom::PhysRect;
    use chibipop::ui::layout::{AnkiSlot, HitTarget};

    fn shown_on(panel: usize) -> Visibility {
        let mut vis = Visibility::Hidden;
        vis.show(Shown {
            output: panel,
            placement: Placement {
                rect: PhysRect { x: 0, y: 0, w: 300, h: 200 },
                buffer: (300, 200),
                logical: (200, 134),
                margin: (0, 0),
            },
            scale: 1.5,
        });
        vis
    }

    fn axis(value120: i32) -> AxisScroll {
        AxisScroll { value120, ..AxisScroll::default() }
    }

    /// Fixed metrics, so the scene is core's real walk without a font
    /// stack. `paint`'s tests keep their own double because they record
    /// what was drawn; this one only ever needs measurements.
    struct FixedMetrics;

    impl chibipop::ui::layout::TextMeasure for FixedMetrics {
        fn measure(
            &mut self,
            run: chibipop::ui::layout::MeasureRun<'_>,
        ) -> Result<chibipop::ui::layout::Metrics, chibipop::ui::layout::MeasureError> {
            let w = run.text.chars().count() as f32 * run.size * 0.5;
            let lines = if run.max_w > 0.0 { (w / run.max_w).ceil().max(1.0) } else { 1.0 };
            Ok(chibipop::ui::layout::Metrics { w, h: lines * run.size * 1.4, lines: lines as u32 })
        }

        fn caret_boxes(
            &mut self,
            run: chibipop::ui::layout::MeasureRun<'_>,
            at: &[u32],
            out: &mut Vec<chibipop::ui::layout::GlyphBox>,
        ) -> Result<(), chibipop::ui::layout::MeasureError> {
            let adv = run.size * 0.5;
            out.extend(at.iter().map(|i| chibipop::ui::layout::GlyphBox {
                x: *i as f32 * adv,
                y: 0.0,
                w: adv,
                h: run.size * 1.4,
            }));
            Ok(())
        }
    }

    /// The demo presentation, laid out the way the surface lays it out:
    /// a theme scaled to physical pixels and a box scaled with it. Tall
    /// enough that nothing clamps.
    fn scene_at(scale: f64) -> PopupScene {
        scene_at_boxed(scale, 4000.0 * scale as f32)
    }

    /// The same, in a box of `max_h` physical pixels - which is what a
    /// monitor-percent cap gives, and the only way `view_h` clamps and
    /// the panel can scroll at all.
    fn scene_at_boxed(scale: f64, max_h: f32) -> PopupScene {
        let theme = crate::popup::physical_theme(&chibipop::ui::theme::Theme::dark(), scale);
        chibipop::ui::layout::scene(
            &chibipop::ui::layout::SceneRequest {
                presentation: &crate::popup::canned(),
                theme: &theme,
                max_w: 424.0 * scale as f32,
                max_h,
                show_back: true,
                side_panel: false,
                anki: Some(&chibipop::present::AnkiPopupState {
                    enabled: true,
                    connected: true,
                    ..chibipop::present::AnkiPopupState::disabled()
                }),
            },
            &mut FixedMetrics,
        )
        .expect("the fake measurer never fails")
    }

    /// The logical point at the middle of one target, given the frame
    /// it belongs to: what a pointer would have to be at to hit it.
    fn aim(hits: &HitScene, action: &HitAction) -> (f64, f64) {
        let target = hits
            .targets
            .iter()
            .find(|t| &t.action == action)
            .unwrap_or_else(|| panic!("{action:?} is not in the scene"));
        let y = f64::from(target.y + target.h / 2.0) - f64::from(hits.scroll);
        let x = match (target.x, target.w) {
            (Some(x), Some(w)) => f64::from(x + w / 2.0),
            _ => 8.0,
        };
        (x / hits.scale, y / hits.scale)
    }

    // ---- the input region ----

    #[test]
    fn a_shown_panel_takes_the_whole_surface_and_a_hidden_one_takes_nothing() {
        // The region is the whole panel, in surface-local logical
        // units - which is what the placement's `logical` size is.
        let region = InputRegion::of(shown_on(1), 1, (200, 134));
        assert_eq!(InputRegion::Panel { w: 200, h: 134 }, region);
        assert_eq!(Some((0, 0, 200, 134)), region.rect());

        // Empty: nothing is added to the `wl_region` at all, so every
        // pointer event falls through to what is underneath.
        let hidden = InputRegion::of(Visibility::Hidden, 1, (200, 134));
        assert_eq!(InputRegion::Empty, hidden);
        assert_eq!(None, hidden.rect());
    }

    /// A coalesced repaint can outlive the show that queued it. If it
    /// landed with a region, a surface the popup has moved off would
    /// keep eating clicks.
    #[test]
    fn a_frame_for_a_surface_that_is_not_showing_is_click_through() {
        assert_eq!(InputRegion::Empty, InputRegion::of(shown_on(0), 1, (200, 134)));
    }

    #[test]
    fn a_degenerate_size_still_leaves_a_one_pixel_region_rather_than_a_protocol_error() {
        assert_eq!(InputRegion::Panel { w: 1, h: 1 }, InputRegion::of(shown_on(0), 0, (0, -4)));
    }

    // ---- the wheel ----

    #[test]
    fn a_wheel_notch_down_scrolls_the_popup_down_as_it_does_on_windows() {
        let mut bank = Wheelbank::default();
        // wl_pointer counts down positive; core counts wheel-up
        // positive, and the Controller subtracts notches * 48 px from
        // the scroll offset - so one notch down must arrive as -1.
        assert_eq!(-1, bank.take(&axis(NOTCH_120)));
        assert_eq!(1, bank.take(&axis(-NOTCH_120)));
        assert_eq!(-3, bank.take(&axis(3 * NOTCH_120)));
    }

    #[test]
    fn sub_notch_wheel_deltas_bank_until_they_make_a_notch() {
        let mut bank = Wheelbank::default();
        for _ in 0..7 {
            assert_eq!(0, bank.take(&axis(15)), "an eighth of a notch is not a notch");
        }
        assert_eq!(-1, bank.take(&axis(15)), "the eighth eighth is");
        assert_eq!(0, bank.take(&axis(15)), "and the bank starts over");
    }

    #[test]
    fn a_discarded_bank_forgets_the_partial_flick() {
        let mut bank = Wheelbank::default();
        assert_eq!(0, bank.take(&axis(90)));
        bank.discard();
        assert_eq!(0, bank.take(&axis(90)), "the 90 before the discard is gone");
    }

    #[test]
    fn a_compositor_without_value120_is_read_off_discrete() {
        let mut bank = Wheelbank::default();
        let frame = AxisScroll { discrete: 2, absolute: 20.0, ..AxisScroll::default() };
        assert_eq!(-2, bank.take(&frame), "the notches, not the pixels beside them");
    }

    #[test]
    fn a_touchpad_frame_carries_pixels_only_and_still_scrolls() {
        let mut bank = Wheelbank::default();
        let half = AxisScroll { absolute: 5.0, ..AxisScroll::default() };
        assert_eq!(0, bank.take(&half));
        assert_eq!(-1, bank.take(&half), "two half-notches of finger travel make one");
    }

    #[test]
    fn an_empty_axis_frame_is_not_a_scroll() {
        let mut bank = Wheelbank::default();
        assert_eq!(0, bank.take(&AxisScroll { stop: true, ..AxisScroll::default() }));
    }

    // ---- hit resolution ----

    /// Two full-width rows and one boxed drill-down target, at the
    /// geometry a 1.0x scene would have.
    fn canned_hits(panel: usize, scroll: f32, scale: f64) -> HitScene {
        HitScene {
            panel,
            targets: vec![
                HitTarget { x: None, y: 0.0, w: None, h: 40.0, action: HitAction::Back },
                HitTarget {
                    x: Some(20.0),
                    y: 60.0,
                    w: Some(30.0),
                    h: 30.0,
                    action: HitAction::DrillDown("\u{6f22}".into()),
                },
                HitTarget {
                    x: None,
                    y: 120.0,
                    w: None,
                    h: 40.0,
                    action: HitAction::ExpandEntry(1),
                },
            ],
            anki: Some(SceneRect { x: 0.0, y: 200.0, w: 300.0, h: 28.0 }),
            view_h: 200.0,
            scroll,
            scale,
        }
    }

    #[test]
    fn logical_pointer_coordinates_become_physical_panel_pixels() {
        let unscaled = canned_hits(0, 0.0, 1.0);
        assert_eq!(PhysPoint { x: 40, y: 70 }, unscaled.local((40.0, 70.0)));

        let scaled = canned_hits(0, 0.0, 1.5);
        assert_eq!(PhysPoint { x: 60, y: 105 }, scaled.local((40.0, 70.0)));
        assert_eq!(PhysPoint { x: 1, y: 1 }, scaled.local((0.9, 0.9)), "floor, not round");
    }

    #[test]
    fn the_same_row_is_hit_at_1x_and_at_1_5x() {
        let unscaled = canned_hits(0, 0.0, 1.0);
        let scaled = canned_hits(0, 0.0, 1.5);
        // The drill-down box is scene y 60..90, x 20..50. At 1.5x the
        // pointer reaches it at two thirds of those logical numbers.
        let hit = Some(Hit::Action(HitAction::DrillDown("\u{6f22}".into())));
        assert_eq!(hit, unscaled.hit(unscaled.local((30.0, 70.0))));
        assert_eq!(hit, scaled.hit(scaled.local((20.0, 46.7))));
        // And the logical point that hit at 1.0x lands one row further
        // down the scene at 1.5x, where nothing is.
        assert_eq!(None, scaled.hit(scaled.local((30.0, 70.0))));
    }

    #[test]
    fn a_full_width_target_spans_the_panel_and_a_boxed_one_does_not() {
        let hits = canned_hits(0, 0.0, 1.0);
        assert_eq!(Some(Hit::Action(HitAction::Back)), hits.hit(PhysPoint { x: 290, y: 10 }));
        assert_eq!(None, hits.hit(PhysPoint { x: 290, y: 70 }), "beside the drill-down box");
    }

    #[test]
    fn the_first_target_in_paint_order_wins() {
        let mut hits = canned_hits(0, 0.0, 1.0);
        hits.targets.push(HitTarget {
            x: None,
            y: 0.0,
            w: None,
            h: 40.0,
            action: HitAction::ExpandEntry(9),
        });
        assert_eq!(Some(Hit::Action(HitAction::Back)), hits.hit(PhysPoint { x: 10, y: 10 }));
    }

    #[test]
    fn scrolling_moves_the_targets_under_the_pointer() {
        let top = canned_hits(0, 0.0, 1.0);
        let at = PhysPoint { x: 10, y: 10 };
        assert_eq!(Some(Hit::Action(HitAction::Back)), top.hit(at));

        // Scrolled by 120 px, the expand row (scene y 120..160) is what
        // sits at the top of the view, and back has gone off it.
        let scrolled = canned_hits(0, 120.0, 1.0);
        assert_eq!(Some(Hit::Action(HitAction::ExpandEntry(1))), scrolled.hit(at));
    }

    #[test]
    fn the_anki_strip_is_fixed_while_the_body_scrolls_under_it() {
        let hits = canned_hits(0, 0.0, 1.0);
        assert_eq!(Some(Hit::Anki), hits.hit(PhysPoint { x: 150, y: 210 }));
        // A row scrolled into the strip's band must not be reachable
        // through it: on Windows the strip was a separate window, here
        // it is the bottom of the same buffer.
        let scrolled = canned_hits(0, 90.0, 1.0);
        assert_eq!(Some(Hit::Anki), scrolled.hit(PhysPoint { x: 150, y: 210 }));
        assert_eq!(None, scrolled.hit(PhysPoint { x: 150, y: 199 }), "the gap above the strip");
    }

    #[test]
    fn a_scene_with_no_anki_slot_has_nothing_below_the_view() {
        let mut hits = canned_hits(0, 0.0, 1.0);
        hits.anki = None;
        assert_eq!(None, hits.hit(PhysPoint { x: 150, y: 210 }));
    }

    #[test]
    fn a_click_carries_the_local_point_whether_or_not_it_hit() {
        let hits = canned_hits(0, 0.0, 1.5);
        assert_eq!(
            Interaction::Click {
                local: PhysPoint { x: 30, y: 15 },
                hit: Some(HitAction::Back),
            },
            click(&hits, (20.0, 10.0))
        );
        // The strip is physical y 200..228, so at 1.5x the pointer
        // reaches it just past logical 133.
        assert_eq!(
            Interaction::Anki { local: PhysPoint { x: 150, y: 205 } },
            click(&hits, (100.0, 137.0))
        );
        assert_eq!(
            Interaction::Click { local: PhysPoint { x: 435, y: 165 }, hit: None },
            click(&hits, (290.0, 110.0))
        );
    }

    /// Core's own targets, not hand-written ones: the scene the painter
    /// draws is the scene the pointer resolves against.
    #[test]
    fn the_targets_come_from_the_scene_core_measured() {
        let mut scene = PopupScene {
            origin: 12.0,
            content_w: 176.0,
            elems: Vec::new(),
            hits: vec![HitTarget {
                x: None,
                y: 12.0,
                w: None,
                h: 24.0,
                action: HitAction::ExpandEntry(0),
            }],
            side: None,
            anki: Some(AnkiSlot {
                label: "Add to Anki".to_string(),
                color: (130, 170, 220),
                rect: SceneRect { x: 0.0, y: 100.0, w: 200.0, h: 28.0 },
            }),
            used_h: 40.0,
            content_h: 160.0,
            view_h: 100.0,
            panel_w: None,
        };
        let hits = HitScene::of(3, &scene, 24.0, 1.5);
        assert_eq!(3, hits.panel);
        assert_eq!(scene.hit_targets(), hits.targets);
        assert_eq!(Some(SceneRect { x: 0.0, y: 100.0, w: 200.0, h: 28.0 }), hits.anki);
        assert_eq!(100.0, hits.view_h);
        assert_eq!(24.0, hits.scroll);

        scene.anki = None;
        assert_eq!(None, HitScene::of(0, &scene, 0.0, 1.0).anki);
        assert_eq!(1.0, HitScene::of(0, &scene, 0.0, 0.0).scale, "never divide by nothing");
    }

    // ---- against a real scene, at both scales ----

    /// The acceptance criterion, on core's own geometry: the pointer
    /// aims at what it sees, and what it sees is the panel it can
    /// reach - at 1.0x and at the 1.5x this desktop actually runs.
    #[test]
    fn every_affordance_of_a_real_scene_is_reachable_at_1x_and_at_1_5x() {
        for scale in [1.0, 1.5] {
            let scene = scene_at(scale);
            let hits = HitScene::of(0, &scene, 0.0, scale);
            let wanted = [
                HitAction::Back,
                HitAction::DrillDown("\u{6f22}".to_string()),
                HitAction::ExpandEntry(0),
                HitAction::ExpandEntry(1),
            ];
            for action in wanted {
                let at = aim(&hits, &action);
                assert_eq!(
                    Some(Hit::Action(action.clone())),
                    hits.hit(hits.local(at)),
                    "{action:?} at logical {at:?}, scale {scale}"
                );
            }
            // And the strip core reserved, which the panel paints.
            let strip = hits.anki.expect("the demo reserves the Anki slot");
            let mid = f64::from(strip.y + strip.h / 2.0) / scale;
            assert_eq!(Some(Hit::Anki), hits.hit(hits.local((10.0, mid))));
        }
    }

    /// The transform is doing real work: feed a 1.5x frame the physical
    /// number instead of the logical one and it misses, which is what a
    /// latched or forgotten scale would look like.
    #[test]
    fn a_pointer_position_that_skipped_the_scale_transform_misses() {
        let hits = HitScene::of(0, &scene_at(1.5), 0.0, 1.5);
        let row = HitAction::ExpandEntry(0);
        let at = aim(&hits, &row);
        assert_eq!(Some(Hit::Action(row.clone())), hits.hit(hits.local(at)));
        assert_ne!(
            Some(Hit::Action(row)),
            hits.hit(hits.local((at.0 * 1.5, at.1 * 1.5))),
            "a logical point read as physical lands 1.5 rows further down"
        );
    }

    /// Scroll moves the body under a fixed pointer, and the targets
    /// move with it because the frame that carried them was repainted.
    #[test]
    fn a_scrolled_frame_resolves_the_row_that_scrolled_into_view() {
        // A box shorter than the content, as a real monitor-percent cap
        // gives: `view_h` clamps and `max_scroll` is positive.
        let scene = scene_at_boxed(1.5, 240.0);
        assert!(scene.content_h > scene.view_h, "the scene has to overflow to scroll");
        let scroll = (scene.content_h - scene.view_h).min(96.0);
        let top = HitScene::of(0, &scene, 0.0, 1.5);
        let scrolled = HitScene::of(0, &scene, scroll, 1.5);

        let row = HitAction::DrillDown("\u{5b57}".to_string());
        let at = aim(&scrolled, &row);
        assert_eq!(Some(Hit::Action(row.clone())), scrolled.hit(scrolled.local(at)));
        // The same point on the unscrolled frame reads a different part
        // of the scene, so it is not the same target.
        assert_ne!(Some(Hit::Action(row)), top.hit(top.local(at)));
    }

    // ---- the script ----

    #[test]
    fn a_script_parses_the_steps_it_recognises_and_rejects_the_rest() {
        let (passes, rejects) =
            parse_script("enter:10,20; motion:10.5,20.5 ;wheel:-360;click:11,21;leave;dump;;");
        assert_eq!(
            vec![vec![
                Step::Enter(10.0, 20.0),
                Step::Motion(10.5, 20.5),
                Step::Wheel(-360),
                Step::Click(11.0, 21.0),
                Step::Leave,
                Step::Dump,
            ]],
            passes
        );
        assert!(rejects.is_empty());

        let (passes, rejects) = parse_script("hover:1,2;click:1;wheel:x;leave:3");
        assert!(passes.is_empty(), "a pass of nothing but rejects is no pass");
        assert_eq!(4, rejects.len(), "every bad step says so and none of them run");
    }

    /// The second pass is what clicks the affordance the first pass's
    /// click produced, which is the whole reason passes exist.
    #[test]
    fn passes_are_split_on_the_pipe_and_each_runs_at_most_once() {
        let (passes, rejects) = parse_script("click:10,20|click:30,40;dump");
        assert_eq!(
            vec![
                vec![Step::Click(10.0, 20.0)],
                vec![Step::Click(30.0, 40.0), Step::Dump],
            ],
            passes
        );
        assert!(rejects.is_empty());
    }
}
