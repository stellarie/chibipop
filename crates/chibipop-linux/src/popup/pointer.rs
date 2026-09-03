//! Popup pointer input (ARCHITECTURE.md#input-ladders): the wheel,
//! entry clicks, the back affordance, and the Anki slot. All input
//! comes from the panel's own input region.
//!
//! Wayland has no machine-wide mouse hook. Therefore, `wl_pointer`
//! events on the layer surface itself replace the Windows
//! `WH_MOUSE_LL` swallow. Windows arms its hooks on each dispatch
//! tick with `SetScrollArmed` and `SetClickArmed`. Here, arming is a
//! property of the surface instead. The input region is the whole
//! panel while the popup is shown, and empty while the popup is
//! hidden ([`InputRegion`]). Therefore, a hidden popup is
//! click-through, and a shown popup needs no separate arming step.
//! `keyboard_interactivity` stays `none`. The popup takes pointer
//! input and never a key.
//!
//! Three coordinate spaces meet here, and the order matters:
//!
//! 1. `wl_pointer` speaks surface-local logical units, as f64.
//! 2. The panel, and everything that core measured, is in physical
//!    pixels at the output's fractional scale. Therefore, a click is
//!    `floor(pos * scale)`. This step is the reverse of the Windows
//!    renderer's `hit_test`, which lays out in DIPs and divides by
//!    the DPI scale.
//! 3. A [`PopupScene`]'s own y value is unscrolled. The painter
//!    subtracts the scroll offset, and core culls what falls off the
//!    panel. Therefore, a scene lookup adds the scroll back on.
//!
//! [`HitScene`] carries exactly those three facts, plus the targets.
//! Every repaint (`Popup::draw`) rebuilds [`HitScene`], so what the
//! pointer resolves against can never drift from what is on screen.

use super::place::Visibility;
use super::{forward, Popup};
use crate::daemon::App;
use chibipop::controller::{Button, HitAction};
use chibipop::geom::PhysPoint;
use chibipop::select::TextAddr;
use chibipop::ui::layout::{HitTarget, PopupScene, SceneRect};
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::globals::GlobalData;
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use smithay_client_toolkit::seat::pointer::{
    AxisScroll, PointerData, PointerEvent, PointerEventKind, PointerHandler, BTN_LEFT, BTN_RIGHT,
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

/// One wheel notch, in the units of `wl_pointer.axis_value120`. This
/// value is the same 120 that `WHEEL_DELTA` uses, so both platforms
/// bank the same arithmetic. A high-resolution wheel scrolls the
/// popup by the same amount on each platform.
const NOTCH_120: i32 = 120;

/// Logical pixels of continuous scroll worth one notch.
///
/// Only a touchpad, or a free-spinning wheel behind a compositor
/// that sends neither `axis_value120` nor `axis_discrete`, reaches
/// this rung. Those sources report finger movement and nothing else.
/// One wheel click's companion `axis` value carries ten units.
/// Therefore, this fallback uses the same scale as the notch rungs,
/// instead of a new scale of its own.
const NOTCH_PX: f64 = 10.0;

/// What a point on the panel resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hit {
    /// A scene target: expand, drill down, or back.
    Action(HitAction),
    /// The Anki slot. Core reserves the slot, and the painter fills
    /// it. A click on the slot becomes the Controller's
    /// `AddRequested`.
    Anki,
}

/// One popup-local interaction, ready to become a Controller `Event`.
#[derive(Debug, Clone, PartialEq)]
pub enum Interaction {
    /// Whole wheel notches, in core's sign: wheel-up is positive.
    Scroll { notches: i32 },
    /// A button press on the panel. `hit` is `None` when the press landed on
    /// no target, and `text` is the gloss address under the pointer.
    Down {
        local: PhysPoint,
        button: Button,
        hit: Option<HitAction>,
        text: Option<TextAddr>,
    },
    /// A pointer move while a button is held or a drag is active.
    Move { local: PhysPoint, text: Option<TextAddr> },
    /// A button release.
    Up { local: PhysPoint, button: Button },
    /// A primary press on the Anki strip.
    ///
    /// Linux paints this strip inside the panel, so it needs a separate
    /// interaction before the Controller receives `AddRequested`.
    Anki { local: PhysPoint },
}

/// The scene under the pointer, as painted.
///
/// Every repaint rebuilds this scene. This rule keeps hit targets
/// honest across a scroll, where the offset changes, and across a
/// scale change, where every rect changes too.
#[derive(Debug, Clone, PartialEq)]
pub struct HitScene {
    /// Which surface this frame belongs to.
    pub panel: usize,
    /// In paint order: the main column, then the side column. This
    /// order matters because resolution takes the first match.
    pub targets: Vec<HitTarget>,
    /// The Anki strip, when core reserved one. The code paints this
    /// strip unscrolled, and tests it unscrolled too.
    pub anki: Option<SceneRect>,
    /// Where the body view ends. Below it there is only the strip.
    pub view_h: f32,
    /// The scroll value used to paint this frame, in physical pixels.
    pub scroll: f32,
    /// The fractional scale used to raster this frame.
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

    /// Convert surface-local logical coordinates to panel-local physical coordinates.
    ///
    /// `PointerDown` and `PointerMoved` use this same physical space.
    ///
    /// This function floors the result instead of rounding it. The answer names the pixel
    /// that holds the pointer. For example, a pointer at 0.9 logical px is inside physical
    /// pixel 1 at 1.5x scale, not pixel 2.
    ///
    pub fn local(&self, pos: (f64, f64)) -> PhysPoint {
        PhysPoint {
            x: (pos.0 * self.scale).floor() as i32,
            y: (pos.1 * self.scale).floor() as i32,
        }
    }

    /// The action at a panel-local physical point, if any.
    pub fn hit(&self, local: PhysPoint) -> Option<Hit> {
        let (x, y) = (local.x as f32, local.y as f32);
        // Test the strip first, unscrolled. The code paints the
        // strip after the body, and the strip never moves. Therefore,
        // a target that scrolls under the strip must not win the
        // hit test.
        if let Some(strip) = self.anki {
            if within(strip.x, strip.w, x) && within(strip.y, strip.h, y) {
                return Some(Hit::Anki);
            }
        }
        if y >= self.view_h {
            return None;
        }
        // Scene y is unscrolled. The painter subtracted the offset,
        // so this lookup adds the offset back.
        let scene_y = y + self.scroll;
        self.targets
            .iter()
            .find(|t| {
                let across = match (t.x, t.w) {
                    (Some(tx), Some(tw)) => within(tx, tw, x),
                    // `None` spans the whole width of the panel.
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
/// This region uses surface-local logical units, because `wl_region`
/// does too. The two states cover the whole input behavior. While
/// the popup is shown, the pointer belongs to the popup, and the app
/// underneath loses hover that the popup was already covering. The
/// moment the popup is hidden, every event falls straight through to
/// what is underneath.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputRegion {
    /// The whole panel accepts pointer input.
    Panel { w: i32, h: i32 },
    /// The empty region: click-through.
    Empty,
}

impl InputRegion {
    /// What one panel's frame receives, given what is on screen.
    ///
    /// A frame for a surface that is not the one shown gets the
    /// empty region. A coalesced repaint can outlive the show that
    /// queued it, and a stale buffer must never take input.
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
/// This struct matches the Windows hook's `take_whole_notches`, with
/// one sign flip. A `wl_pointer` axis counts positive downward, so
/// the content moves up. But `WM_MOUSEWHEEL` and core's
/// `Event::Scrolled` count wheel-up as positive.
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

/// One frame's vertical scroll in 120ths, by the best rung it
/// carries.
///
/// A wheel frame carries `axis_value120` (v8+) or `axis_discrete`
/// (v5-7), beside the continuous `axis` value. The rungs are
/// exclusive on purpose. Reading both rungs would count every click
/// twice. This function deliberately ignores `relative_direction`. It
/// reports whether the compositor already inverted the delta for
/// natural scrolling. That inversion is the user's preference, and
/// the code must honor it, not undo it.
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

fn button_mask(button: Button) -> u8 {
    match button {
        Button::Primary => 1,
        Button::Secondary => 2,
    }
}

/// The popup's pointer: the seat objects, the focus, and the bank.
///
/// The button bank is separate from surface focus. Wayland keeps delivering
/// motion through its implicit grab after a press, so the popup forwards
/// that motion even after a leave event.
pub struct Pointer {
    seats: SeatState,
    /// `wp_cursor_shape_v1`, where the compositor advertises it. Where
    /// the compositor does not, the code leaves the cursor alone
    /// instead of loading XCursor themes.
    shapes: Option<CursorShapeManager>,
    pointer: Option<WlPointer>,
    device: Option<WpCursorShapeDeviceV1>,
    /// The last `enter` serial, which `set_shape` has to quote.
    serial: Option<u32>,
    focus: Option<Focus>,
    /// Buttons currently held on the popup.
    buttons: u8,
    /// Core requested pointer capture for a selection drag.
    dragging: bool,
    bank: Wheelbank,
    /// The last shape that the code asked for. A hover across one
    /// target then costs one request, instead of one for each motion
    /// event.
    shape: Option<Shape>,
    /// `popup.scroll_popup`. Windows arms its hook on each tick from
    /// this same setting. Here, the same setting gates the wheel.
    wheel_enabled: bool,
    /// `CHIBIPOP_POINTER_SCRIPT`, parsed into passes.
    script: Vec<Vec<Step>>,
    /// How many passes have run. One pass per fresh frame, and never
    /// twice.
    pass: usize,
    /// A fresh frame still needs a pass.
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
            buttons: 0,
            dragging: false,
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

    /// Record the Controller's pointer-capture state.
    pub fn set_dragging(&mut self, dragging: bool) {
        self.dragging = dragging;
    }

    /// Return whether motion must reach the Controller.
    pub fn forwards_motion(&self) -> bool {
        self.dragging || self.buttons != 0
    }

    /// Record a supported button press.
    pub fn button_down(&mut self, button: Button) {
        self.buttons |= button_mask(button);
    }

    /// Record a supported button release and return whether it was held.
    pub fn button_up(&mut self, button: Button) -> bool {
        let mask = button_mask(button);
        let held = self.buttons & mask != 0;
        self.buttons &= !mask;
        held
    }

    /// Forget button and focus state when the popup hides.
    pub fn cancel(&mut self) {
        self.buttons = 0;
        self.dragging = false;
        self.focus = None;
        self.bank.discard();
        self.shape = None;
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

    /// Call this function before a fresh frame arrives, to arm the
    /// next pass. This call is cheap and idempotent, and it is a
    /// no-op without a script.
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
        self.buttons = 0;
        self.dragging = false;
        self.shape = None;
    }

    /// The `wp_cursor_shape_v1` device for the pointer just taken.
    pub fn attach_shape_device(&mut self, device: WpCursorShapeDeviceV1) {
        self.device = Some(device);
    }

    /// The manager and the live pointer. A shape device needs both
    /// to exist. This function returns `None` when either one is
    /// missing.
    pub fn shape_source(&self) -> Option<(&CursorShapeManager, &WlPointer)> {
        Some((self.shapes.as_ref()?, self.pointer.as_ref()?))
    }

    /// The pointer crossed onto a panel.
    ///
    /// `serial` belongs to the `enter` event, and `set_shape` must
    /// quote it. A scripted pass has no real enter event behind it,
    /// so it passes `None`. This rule lets a scripted pass drive
    /// every hit the same way, but it never asks the compositor to
    /// change a cursor when the code holds no serial for it.
    pub fn enter(&mut self, panel: usize, pos: (f64, f64), serial: Option<u32>) {
        self.serial = serial;
        self.focus = Some(Focus { panel, pos });
        // A fresh crossing starts with no banked sub-notch: the last
        // partial flick belonged to whatever the pointer was over.
        self.bank.discard();
        self.shape = None;
    }

    pub fn leave(&mut self, panel: usize) {
        if self.focus.is_some_and(|f| f.panel == panel) && !self.forwards_motion() {
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

    /// A wheel frame over the panel. This function returns `None`
    /// when no whole notch came from the frame, or when
    /// `popup.scroll_popup` is off.
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

/// Resolve one button press against a painted frame.
///
/// This function stays free of Wayland state, so tests can cover button roles,
/// hit targets, and text addresses with plain data.
pub fn press(
    local: PhysPoint,
    button: Button,
    hit: Option<Hit>,
    text: Option<TextAddr>,
) -> Interaction {
    match hit {
        Some(Hit::Anki) if button == Button::Primary => Interaction::Anki { local },
        Some(Hit::Anki) => Interaction::Down { local, button, hit: None, text: None },
        Some(Hit::Action(action)) => Interaction::Down { local, button, hit: Some(action), text },
        None => Interaction::Down { local, button, hit: None, text },
    }
}

// ---- the scripted pass ----

/// One step of `CHIBIPOP_POINTER_SCRIPT`.
///
/// Coordinates are surface-local logical units, exactly what
/// `wl_pointer` delivers. `Wheel` is a raw `axis_value120` value,
/// which counts positive downward. `Press` and `Release` use the
/// primary button, while `Press2` and `Release2` use the secondary
/// button. `Drag` is a motion while a button is held. The script
/// exists so a developer can drive the handlers on a live compositor
/// without synthesizing any seat input. The script never touches,
/// warps, or steals focus from the human's own pointer. It enters
/// through the same entry points that a real frame uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    Enter(f64, f64),
    Motion(f64, f64),
    Click(f64, f64),
    Press(f64, f64, Button),
    Release(f64, f64, Button),
    Drag(f64, f64),
    Wheel(i32),
    Leave,
    /// Log the frame's hit targets in logical coordinates, so the next
    /// run aims at the real scene rather than at a guess.
    Dump,
}

pub const SCRIPT_ENV: &str = "CHIBIPOP_POINTER_SCRIPT";

/// Passes are `|`-separated. Steps inside one pass are `;`-separated.
///
/// One pass runs for each fresh frame that the popup is armed for.
/// Therefore, a script can answer content that its own earlier pass
/// requested. For example, pass one can click a drill-down, and pass
/// two can click the back affordance that pass one produced. A pass
/// never repeats. Therefore, a click that changes the scene cannot
/// trigger itself in a loop.
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
                "press" => point(arg).map(|(x, y)| Step::Press(x, y, Button::Primary)),
                "release" => point(arg).map(|(x, y)| Step::Release(x, y, Button::Primary)),
                "press2" => point(arg).map(|(x, y)| Step::Press(x, y, Button::Secondary)),
                "release2" => point(arg).map(|(x, y)| Step::Release(x, y, Button::Secondary)),
                "drag" => point(arg).map(|(x, y)| Step::Drag(x, y)),
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
/// A frame batches related events. For example, SCTK merges the axis
/// pieces into one event. A frame may also carry a leave and an
/// enter together, when the pointer crosses between two of the
/// popup's surfaces. Therefore, this function keeps the order inside
/// the frame.
pub fn frame(popup: &mut Popup, events: &[PointerEvent]) -> Vec<Interaction> {
    let mut out = Vec::new();
    for event in events {
        let panel = popup.panel_of(&event.surface);
        match &event.kind {
            PointerEventKind::Enter { serial } => {
                if let Some(panel) = panel {
                    popup.pointer_enter(panel, event.position, Some(*serial));
                }
            }
            PointerEventKind::Leave { .. } => {
                if let Some(panel) = panel {
                    popup.pointer_leave(panel);
                }
            }
            PointerEventKind::Motion { .. } => {
                if panel.is_some() || popup.pointer_forwards_motion() {
                    if let Some(interaction) = popup.pointer_move(event.position) {
                        out.push(interaction);
                    }
                }
            }
            PointerEventKind::Press { button, .. } => {
                let Some(button) = button_role(*button) else { continue };
                if panel.is_some() {
                    if let Some(interaction) = popup.pointer_press(event.position, button) {
                        out.push(interaction);
                    }
                }
            }
            PointerEventKind::Release { button, .. } => {
                let Some(button) = button_role(*button) else { continue };
                if panel.is_some() || popup.pointer_forwards_motion() {
                    if let Some(interaction) = popup.pointer_release(event.position, button) {
                        out.push(interaction);
                    }
                }
            }
            PointerEventKind::Axis { vertical, .. } => {
                if panel.is_some() {
                    out.extend(popup.pointer_wheel(vertical));
                }
            }
        }
    }
    out
}

fn button_role(button: u32) -> Option<Button> {
    match button {
        BTN_LEFT => Some(Button::Primary),
        BTN_RIGHT => Some(Button::Secondary),
        _ => None,
    }
}

// ---- the dispatch plumbing ----

forward!(WlSeat, SeatData);
forward!(WlPointer, PointerData<()>);
forward!(WpCursorShapeManagerV1, GlobalData);
forward!(WpCursorShapeDeviceV1, GlobalData);

/// The seat's capabilities report a pointer, at startup and again
/// when a user plugs one in later. This code never asks for a
/// keyboard. `keyboard_interactivity` is `none`, and the popup has no
/// use for a keyboard (ARCHITECTURE.md#input-ladders).
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

/// Every `wl_pointer` frame in the daemon lands here: the popup's
/// pointer, and, while a region pick is shown, that pick's own
/// pointer. SCTK has one handler for each state. `App` sorts these
/// frames by surface identity. This impl deliberately does not know
/// which surface is which.
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
    use chibipop::dict::gloss::{DocAddr, NodePath};
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

    /// Fixed metrics, so the scene follows core's real logic without
    /// a font stack. `paint`'s tests keep their own double, because
    /// those tests record what the code drew. This test only ever
    /// needs measurements.
    struct FixedMetrics;

    impl chibipop::ui::layout::TextMeasure for FixedMetrics {
        fn measure(
            &mut self,
            run: chibipop::ui::layout::MeasureRun<'_>,
            out: &mut chibipop::ui::layout::Measured,
        ) -> Result<(), chibipop::ui::layout::MeasureError> {
            use chibipop::ui::layout::{LineBox, Metrics, SpanBox};
            out.clear();
            // Half an em for each character, with spans laid end to
            // end. This test only ever needs the aggregate, but it
            // fills in the detail too, so the test honors the seam's
            // whole contract.
            let mut x = 0.0f32;
            let mut h = 0.0f32;
            for (i, span) in run.spans.iter().enumerate() {
                let w = span.text.chars().count() as f32 * span.size * 0.5;
                out.spans.push(SpanBox { span: i as u32, line: 0, x, w, h: span.size * 1.4 });
                x += w;
                h = h.max(span.size * 1.4);
            }
            let lines = if run.max_w > 0.0 { (x / run.max_w).ceil().max(1.0) } else { 1.0 };
            for line in 0..lines as u32 {
                out.lines.push(LineBox {
                    y: line as f32 * h,
                    w: x.min(run.max_w.max(1.0)),
                    h,
                    baseline: h,
                });
            }
            out.metrics = Metrics { w: x, h: lines * h, lines: lines as u32 };
            Ok(())
        }

        fn caret_boxes(
            &mut self,
            run: chibipop::ui::layout::MeasureRun<'_>,
            at: &[u32],
            out: &mut Vec<chibipop::ui::layout::GlyphBox>,
        ) -> Result<(), chibipop::ui::layout::MeasureError> {
            let size = run.spans.first().map_or(0.0, |s| s.size);
            let adv = size * 0.5;
            out.extend(at.iter().map(|i| chibipop::ui::layout::GlyphBox {
                x: *i as f32 * adv,
                y: 0.0,
                w: adv,
                h: size * 1.4,
            }));
            Ok(())
        }

        fn hit_offset(
            &mut self,
            run: chibipop::ui::layout::MeasureRun<'_>,
            x: f32,
            _y: f32,
        ) -> Result<u32, chibipop::ui::layout::MeasureError> {
            // The inverse of `caret_boxes`: half an em per character, one line.
            let size = run.spans.first().map_or(0.0, |s| s.size);
            let adv = size * 0.5;
            let len: usize = run.spans.iter().map(|s| s.text.encode_utf16().count()).sum();
            if adv <= 0.0 {
                return Ok(0);
            }
            Ok(((x / adv).round().max(0.0) as usize).min(len) as u32)
        }
    }

    /// The demo presentation, laid out the same way the surface lays
    /// it out: a theme scaled to physical pixels, and a box scaled
    /// with it. The box is tall enough that nothing clamps.
    fn scene_at(scale: f64) -> PopupScene {
        scene_at_boxed(scale, 4000.0 * scale as f32)
    }

    /// The same scene, in a box of `max_h` physical pixels. A
    /// monitor-percent cap gives this kind of box. It is the only way
    /// that `view_h` clamps, and the only way that the panel can
    /// scroll at all.
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
                render: Default::default(),
                anki: Some(&chibipop::present::AnkiPopupState {
                    enabled: true,
                    connected: true,
                    ..chibipop::present::AnkiPopupState::disabled()
                }),
                selection: None,
            },
            &mut FixedMetrics,
        )
        .expect("the fake measurer never fails")
    }

    /// The logical point at the middle of one target, given the frame
    /// that the target belongs to. This is the point where a pointer
    /// must be to hit the target.
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
        // units. The placement's `logical` size uses the same units.
        let region = InputRegion::of(shown_on(1), 1, (200, 134));
        assert_eq!(InputRegion::Panel { w: 200, h: 134 }, region);
        assert_eq!(Some((0, 0, 200, 134)), region.rect());

        // Empty: the code adds nothing to the `wl_region`, so every
        // pointer event falls through to what is underneath.
        let hidden = InputRegion::of(Visibility::Hidden, 1, (200, 134));
        assert_eq!(InputRegion::Empty, hidden);
        assert_eq!(None, hidden.rect());
    }

    /// A drag over the canned gloss must paint. The text hit on the frame
    /// and the highlight boxes of the next frame use one source table.
    #[test]
    fn a_drag_range_over_the_canned_gloss_produces_highlights() {
        use chibipop::select::{SelRange, Selections};
        let scale = 1.0;
        let scene = scene_at(scale);
        let font = chibipop::ui::theme::Theme::dark().font_name;
        let gloss: Vec<&chibipop::ui::layout::SceneElem> =
            scene.elems.iter().filter(|e| !e.sources.is_empty()).collect();
        assert!(!gloss.is_empty(), "the canned popup has gloss text");
        let first = gloss[0];
        let y = first.pen.1 + 2.0;
        let start = scene
            .text_hit((first.pen.0 + 1.0, y), 0.0, &font, &mut FixedMetrics)
            .unwrap()
            .expect("a gloss address");
        let end = scene
            .text_hit((first.pen.0 + 40.0, y), 0.0, &font, &mut FixedMetrics)
            .unwrap()
            .expect("a gloss address");
        assert!(start < end, "{start:?} < {end:?}");
        let mut all = Selections::default();
        all.card_mut(0).replace(SelRange { start, end });
        let theme = crate::popup::physical_theme(&chibipop::ui::theme::Theme::dark(), scale);
        let selected = chibipop::ui::layout::scene(
            &chibipop::ui::layout::SceneRequest {
                presentation: &crate::popup::canned(),
                theme: &theme,
                max_w: 424.0,
                max_h: 4000.0,
                show_back: true,
                side_panel: false,
                render: Default::default(),
                anki: None,
                selection: Some(&all),
            },
            &mut FixedMetrics,
        )
        .unwrap();
        assert!(!selected.highlights.is_empty(), "items {:?}", all.card(0));
        assert!(selected.elems.iter().any(|e| e.kind == chibipop::ui::layout::ElemKind::Check));
    }

    /// A coalesced repaint can outlive the show that queued it. If
    /// that repaint landed with a region, a surface that the popup
    /// has moved off would still take clicks.
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
        // wl_pointer counts down as positive. Core counts wheel-up as
        // positive, and the Controller subtracts notches * 48 px from
        // the scroll offset. Therefore, one notch down must arrive as
        // -1.
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
        // The logical point that hit at 1.0x lands one row further
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

        // Scrolled by 120 px, the expand row (scene y 120..160) now
        // sits at the top of the view, and the back row has scrolled
        // off it.
        let scrolled = canned_hits(0, 120.0, 1.0);
        assert_eq!(Some(Hit::Action(HitAction::ExpandEntry(1))), scrolled.hit(at));
    }

    #[test]
    fn the_anki_strip_is_fixed_while_the_body_scrolls_under_it() {
        let hits = canned_hits(0, 0.0, 1.0);
        assert_eq!(Some(Hit::Anki), hits.hit(PhysPoint { x: 150, y: 210 }));
        // A row that scrolls into the strip's band must stay
        // unreachable through the strip. On Windows the strip was a
        // separate window. Here it is the bottom of the same buffer.
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
    fn a_press_carries_its_button_hit_and_text() {
        assert_eq!(
            Interaction::Down {
                local: PhysPoint { x: 30, y: 15 },
                button: Button::Primary,
                hit: Some(HitAction::Back),
                text: None,
            },
            press(
                PhysPoint { x: 30, y: 15 },
                Button::Primary,
                Some(Hit::Action(HitAction::Back)),
                None,
            )
        );
        // The strip is physical y 200..228, so at 1.5x the pointer
        // reaches it just past logical 133.
        assert_eq!(
            Interaction::Anki { local: PhysPoint { x: 150, y: 205 } },
            press(PhysPoint { x: 150, y: 205 }, Button::Primary, Some(Hit::Anki), None)
        );
        assert_eq!(
            Interaction::Down {
                local: PhysPoint { x: 435, y: 165 },
                button: Button::Primary,
                hit: None,
                text: None,
            },
            press(PhysPoint { x: 435, y: 165 }, Button::Primary, None, None)
        );

        let text = TextAddr {
            entry: 0,
            addr: DocAddr { path: NodePath::ROOT, byte: 0 },
        };
        let right = press(
            PhysPoint { x: 30, y: 15 },
            Button::Secondary,
            Some(Hit::Action(HitAction::Back)),
            None,
        );
        assert_eq!(
            Interaction::Down {
                local: PhysPoint { x: 30, y: 15 },
                button: Button::Secondary,
                hit: Some(HitAction::Back),
                text: None,
            },
            right
        );
        assert!(!matches!(right, Interaction::Anki { .. }), "secondary presses stay in the popup");
        assert_eq!(
            Interaction::Down {
                local: PhysPoint { x: 30, y: 15 },
                button: Button::Primary,
                hit: None,
                text: Some(text),
            },
            press(PhysPoint { x: 30, y: 15 }, Button::Primary, None, Some(text))
        );
    }

    /// Core's own targets, not hand-written ones. The scene that the
    /// painter draws is the same scene that the pointer resolves
    /// against.
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
            highlights: Vec::new(),
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

    /// The acceptance criterion, on core's own geometry. The pointer
    /// aims at what it sees, and what it sees is the panel that it
    /// can reach, at 1.0x and at the 1.5x that this desktop runs.
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
            // The strip that core reserved, which the panel paints.
            let strip = hits.anki.expect("the demo reserves the Anki slot");
            let mid = f64::from(strip.y + strip.h / 2.0) / scale;
            assert_eq!(Some(Hit::Anki), hits.hit(hits.local((10.0, mid))));
        }
    }

    /// This test proves that the transform does real work. Feed a
    /// 1.5x frame the physical number instead of the logical one, and
    /// the hit misses. A latched or forgotten scale would look
    /// exactly like this miss.
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

    /// A scroll moves the body under a fixed pointer, and the targets
    /// move with it, because the code repainted the frame that
    /// carried them.
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
            parse_script("enter:10,20; motion:10.5,20.5 ;wheel:-360;click:11,21;press:12,22;release:12,22;press2:13,23;release2:13,23;drag:14,24;leave;dump;;");
        assert_eq!(
            vec![vec![
                Step::Enter(10.0, 20.0),
                Step::Motion(10.5, 20.5),
                Step::Wheel(-360),
                Step::Click(11.0, 21.0),
                Step::Press(12.0, 22.0, Button::Primary),
                Step::Release(12.0, 22.0, Button::Primary),
                Step::Press(13.0, 23.0, Button::Secondary),
                Step::Release(13.0, 23.0, Button::Secondary),
                Step::Drag(14.0, 24.0),
                Step::Leave,
                Step::Dump,
            ]],
            passes
        );
        assert!(rejects.is_empty());

        let (passes, rejects) = parse_script("hover:1,2;click:1;wheel:x;leave:3;press:");
        assert!(passes.is_empty(), "a pass of nothing but rejects is no pass");
        assert_eq!(5, rejects.len(), "every bad step says so and none of them run");
    }


    /// The second pass clicks the affordance that the first pass's
    /// click produced. This sequence is the whole reason that passes
    /// exist.
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
