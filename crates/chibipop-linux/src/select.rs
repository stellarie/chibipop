//! This module defines the Linux region selector.
//!
//! The user drags a box on a dimmed screen, and the selector returns a `PhysRect`.
//!
//! The Windows bin has `action/selection.rs` for the same function.
//! The selector dims the live screen to 40 % black.
//! It clears the drag rectangle and draws a 2 px white frame around it.
//! It shows a crosshair cursor.
//! `Esc` or a right-click cancels the pick.
//! It discards a drag under [`MIN_DRAG_PX`] as an accidental click.
//! It creates one `zwlr_layer_shell_v1` surface per output on the `Overlay` layer.
//! Each surface anchors to all four edges, so the compositor sizes it to the whole output.
//!
//! **The live screen, not a frozen grab.**
//! The selector captures no data before the drag.
//! The dim is a translucent surface over the real screen content.
//! The user sees that content while the user drags.
//! The code grabs the region after it destroys this surface.
//! This order keeps the selector out of the grab.
//! [`Selector::pick`] therefore destroys its surfaces and completes a round trip before it returns.
//!
//! **Why this surface can take keyboard focus and the popup cannot.**
//! `keyboard_interactivity = none` is a strict rule for the popup.
//! A surface that appears on every hover must not take focus.
//! The selector has the opposite role.
//! The user requests it with a key press.
//! It stays modal until it closes.
//! Its contract accepts the next drag or `Esc`.
//! Only a successful drag can end a picker that cannot hear `Esc`.
//! Therefore, only this surface sets `Exclusive`.
//! It returns focus when it destroys itself.
//!
//! **How the selector pumps events.**
//! [`Selector::pick`] runs a nested dispatch loop that blocks the daemon thread.
//! It is the analog of the nested `GetMessageW` pump in the Windows bin.
//! The daemon queue lives inside the calloop `WaylandSource`.
//! No source callback can dispatch that queue.
//! A pick therefore creates a second `EventQueue` on the same `Connection` for its objects.
//! It runs that queue in a temporary `calloop::EventLoop` with a `Timer` of [`PICK_TIMEOUT`] length.
//! This design has two effects.
//! A compositor that sends a press without a release cannot block the daemon.
//! The timer cancels the pick.
//! Any callback can start another pick, even a pointer click on the popup.
//! The nested pick holds no borrow of the outer loop.
//!
//! Events for the daemon queue that arrive while a pick runs survive.
//! `wayland-client` sends each read to every queue.
//! Calloop dispatches those events when it next sees the socket as readable.
//! A pick therefore ends with a `wl_display.sync` on the daemon queue ([`Wake`]).
//! That sync makes those events dispatch at once.
//!
//! **The code binds the keyboard protocol directly, not through the SCTK `seat::keyboard` module.**
//! That module needs the SCTK `xkbcommon` feature.
//! The feature adds a build-time `pkg-config` and `libxkbcommon` dependency to the workspace.
//! Cargo unifies features, so every Linux build includes these dependencies for one key.
//! `Esc` is a physical key with a fixed evdev code ([`KEY_ESC`]).
//! `wl_keyboard.key` carries exactly that code, so the selector needs no keymap.
//! The physical Esc key cancels in every keyboard layout.

use crate::daemon::App;
use crate::overlay::{self, Px};
use crate::popup::Screen;
use anyhow::{Context, Result};
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, LoopSignal};
use calloop_wayland_source::WaylandSource;
use chibipop::geom::{PhysPoint, PhysRect};
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, BTN_LEFT};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerSurface,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use std::time::Duration;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_callback::WlCallback;
use wayland_client::protocol::wl_keyboard::{self, KeyState, WlKeyboard};
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_shm::Format;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

/// `hyprctl layers` and `layerrule` show this namespace.
const NAMESPACE: &str = "chibipop-select";

/// The shortest drag that counts, in physical px.
/// This constant matches the Windows bin's `MIN_DRAG_PX`.
/// A stray click while the selector is open cancels the pick.
/// It does not create a one-pixel grab.
pub const MIN_DRAG_PX: i32 = 5;

/// The selection frame thickness, in physical px.
/// This constant matches the Windows bin's `BORDER_PX`.
pub const BORDER_PX: i32 = 2;

/// Premultiplied black with the Windows bin's `DIM_ALPHA` value of 102/255.
/// Premultiplied black leaves the color channels at zero.
/// This value therefore stores alpha only.
pub const DIM: Px = [0, 0, 0, 102];

/// The selection frame: opaque white, as on Windows.
pub const FRAME: Px = [0xFF, 0xFF, 0xFF, 0xFF];

/// `KEY_ESC` from `linux/input-event-codes.h`.
/// `wl_keyboard.key` reports this evdev code, not an xkb keysym.
pub const KEY_ESC: u32 = 1;

/// `BTN_RIGHT` from `linux/input-event-codes.h`.
/// SCTK exports [`BTN_LEFT`] but not this code.
pub const BTN_RIGHT: u32 = 0x111;

/// The time before a pick cancels itself.
///
/// The nested pump needs this guard.
/// A compositor that sends a press without a release would otherwise hold the daemon thread forever.
/// The daemon would then have no cursor samples, control socket, or popup.
/// Twenty seconds exceeds any real drag and still lets a stuck session recover.
/// The caller supplies the timeout. It does not read this constant.
/// A diagnostic can therefore request a pick that must expire.
pub const PICK_TIMEOUT: Duration = Duration::from_secs(20);

/// Return a rectangle from two drag corners in either order.
///
/// This function matches the Windows bin's `normalized_rect`.
/// A drag up and left returns the same box as a drag down and right.
/// The rectangle uses the smaller coordinate as its origin.
pub fn normalized(a: PhysPoint, b: PhysPoint) -> PhysRect {
    PhysRect {
        x: a.x.min(b.x),
        y: a.y.min(b.y),
        w: (a.x - b.x).abs(),
        h: (a.y - b.y).abs(),
    }
}

/// Return whether this drag meets the threshold.
///
/// This function matches the Windows bin's `meets_drag_threshold` with `||`.
/// A thin horizontal strip of text is a valid selection.
/// One axis at or above the threshold is enough.
pub fn meets_threshold(r: PhysRect) -> bool {
    r.w >= MIN_DRAG_PX || r.h >= MIN_DRAG_PX
}

/// The result of a pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The drag continues, or the pick waits for its first press.
    Live,
    /// The user selected a box that passes the threshold.
    Picked(PhysRect),
    /// The pick ends when `Esc`, a right-click, an under-threshold drag, a closed surface, or the timeout occurs.
    /// Each case returns no region and no error.
    Cancelled,
}

/// The action that one input event requests.
///
/// This enum stores the selector input contract as data.
/// A test can check event routes without a compositor.
/// `Esc` and a right-click both request the same cancel result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// A left press starts the drag.
    Start,
    /// The pointer moved.
    Move,
    /// A left release ends the drag.
    Finish,
    /// `Esc`, a right press, a closed surface, or the deadline cancels the pick.
    Cancel,
    /// The selector ignores every other event.
    Ignore,
}

/// Return the action for one pointer button.
///
/// A right-click cancels the pick, as on Windows.
/// This function checks the right button before the left button.
/// A right-click while a drag runs therefore still cancels.
/// A right release requests no action because the press already decided.
pub fn ask_of_button(button: u32, pressed: bool) -> Ask {
    match (button, pressed) {
        (BTN_RIGHT, true) => Ask::Cancel,
        (BTN_LEFT, true) => Ask::Start,
        (BTN_LEFT, false) => Ask::Finish,
        _ => Ask::Ignore,
    }
}

/// Return the action for one key.
/// The codes are raw evdev codes.
/// The module documentation explains why this file needs no keymap.
pub fn ask_of_key(code: u32, pressed: bool) -> Ask {
    if pressed && code == KEY_ESC {
        Ask::Cancel
    } else {
        Ask::Ignore
    }
}

/// The drag state machine uses plain data.
///
/// The struct holds no Wayland types, so a test can check the arithmetic without a compositor.
/// The same [`Drag::apply`] serves a real `wl_pointer` frame and a test.
/// Coordinates that enter or leave this state are **global physical pixels**, which is core's only coordinate space.
/// [`Surface::global`] converts the surface-local logical units of `wl_pointer` once at entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Drag {
    /// The press position while a drag runs.
    anchor: Option<PhysPoint>,
    /// The newest position used to paint the drag.
    at: Option<PhysPoint>,
    done: Option<Outcome>,
}

impl Drag {
    /// Return the box to paint, if any.
    pub fn rect(&self) -> Option<PhysRect> {
        Some(normalized(self.anchor?, self.at?))
    }

    /// Return whether this pick finished and how it finished.
    pub fn outcome(&self) -> Outcome {
        self.done.unwrap_or(Outcome::Live)
    }

    pub fn finished(&self) -> bool {
        self.done.is_some()
    }

    /// Apply one input event at a point in global physical pixels.
    ///
    /// The state does not change after a decision.
    /// The first [`Ask::Finish`] or [`Ask::Cancel`] decides the result.
    /// A later release cannot revive a canceled pick.
    pub fn apply(&mut self, ask: Ask, at: PhysPoint) {
        if self.done.is_some() {
            return;
        }
        match ask {
            Ask::Start => {
                self.anchor = Some(at);
                self.at = Some(at);
            }
            // Ignore motion before the press.
            // This keeps the dim surface unchanged until the user starts a drag.
            Ask::Move => {
                if self.anchor.is_some() {
                    self.at = Some(at);
                }
            }
            Ask::Finish => {
                // A release without our press decides no result.
                // The user pressed the button before this surface appeared.
                let Some(anchor) = self.anchor else { return };
                let rect = normalized(anchor, at);
                self.done = Some(if meets_threshold(rect) {
                    Outcome::Picked(rect)
                } else {
                    Outcome::Cancelled
                });
            }
            Ask::Cancel => self.done = Some(Outcome::Cancelled),
            Ask::Ignore => {}
        }
    }

    /// Apply [`Ask::Cancel`] when the source has no pointer position.
    /// A closed surface and the deadline use this path.
    pub fn cancel(&mut self) {
        self.apply(Ask::Cancel, PhysPoint { x: 0, y: 0 });
    }
}

/// Paint one output's selector frame.
///
/// The function dims the output, clears the selection, and draws a frame outside it.
/// It combines the Windows bin's `punch_through` and `paint_border` in one pass over premultiplied `Argb8888`.
/// `sel` contains surface-local physical pixels and can extend beyond an edge.
/// Every fill clips to the output.
pub fn paint(px: &mut [Px], w: i32, h: i32, sel: Option<PhysRect>) {
    px.fill(DIM);
    let Some(sel) = sel.filter(|s| s.w > 0 && s.h > 0) else { return };
    // Clear first, then draw the frame.
    // The frame sits outside the selection, as on Windows.
    // The grab therefore captures the clear box without a border row.
    overlay::fill(px, w, h, sel, overlay::CLEAR);
    for strip in overlay::strips(sel.inflated(BORDER_PX, BORDER_PX), BORDER_PX) {
        overlay::fill(px, w, h, strip, FRAME);
    }
}

/// One output's selector surface.
struct Surface {
    /// The selector surface id for this output.
    /// Diagnostics use this id to name a monitor, for example, "surface 1".
    id: usize,
    /// This output's box in global physical space.
    /// It converts a surface-local pointer position to core's coordinate space.
    monitor: PhysRect,
    /// The raster scale.
    /// The code reads it once per pick.
    /// A scale change cannot move the box under the pointer.
    scale: f64,
    layer: LayerSurface,
    viewport: Option<WpViewport>,
    /// The logical size from the compositor.
    /// It stays `None` until the compositor replies.
    /// The surface uses four anchors and `set_size(0, 0)`, so the compositor chooses the whole-output size.
    /// The selector does not calculate this size.
    configured: Option<(i32, i32)>,
}

impl Surface {
    /// Convert a surface-local logical position to a global physical point.
    ///
    /// The function floors instead of rounds for the `HitScene::local` rule.
    /// The result identifies the physical pixel that contains the pointer.
    /// It adds the output origin because core uses one global space.
    /// A layer surface measures positions from its own corner.
    fn global(&self, pos: (f64, f64)) -> PhysPoint {
        PhysPoint {
            x: self.monitor.x + (pos.0 * self.scale).floor() as i32,
            y: self.monitor.y + (pos.1 * self.scale).floor() as i32,
        }
    }

    /// Return the buffer size and logical size after the compositor configures the surface.
    fn frame(&self) -> Option<((i32, i32), (i32, i32))> {
        let logical = self.configured?;
        let buffer = (
            ((f64::from(logical.0) * self.scale).round() as i32).max(1),
            ((f64::from(logical.1) * self.scale).round() as i32).max(1),
        );
        Some((buffer, logical))
    }
}

/// The active state for one pick: its surfaces, drag, and exit.
///
/// This struct lives on [`App`] instead of inside [`Selector`] for one concrete reason.
/// The SCTK handlers that drive the drag are methods on `App`.
/// Every field they change must be reachable from `&mut App`.
/// The nested pump dispatches `&mut App`, so it cannot borrow a `&mut Selector` at the same time.
pub struct Pick {
    surfaces: Vec<Surface>,
    drag: Drag,
    /// Stop the nested loop when the drag decides.
    /// This value is `None` between surface creation and loop start.
    signal: Option<LoopSignal>,
    /// The pointer and keyboard for this pick on its own queue.
    /// The pick releases both when it ends, which returns focus.
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,
    shape: Option<WpCursorShapeDeviceV1>,
    /// The last `enter` serial. `set_shape` must use this serial.
    serial: Option<u32>,
    /// A drag change needs a new frame.
    dirty: bool,
    notes: Vec<String>,
}

impl Pick {
    /// Does one `wl_surface` belong to this pick?
    ///
    /// The shared SCTK handlers ask this before they treat a surface as the popup's.
    pub fn owns(&self, surface: &WlSurface) -> bool {
        self.surfaces.iter().any(|s| s.layer.wl_surface() == surface)
    }

    /// Does a `configure` or `closed` event belong to this pick?
    /// Those events name a layer surface rather than a `wl_surface`.
    pub fn owns_layer(&self, layer: &LayerSurface) -> bool {
        self.surfaces.iter().any(|s| &s.layer == layer)
    }

    pub fn drain_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    pub fn outcome(&self) -> Outcome {
        self.drag.outcome()
    }

    fn slot_of(&self, surface: &WlSurface) -> Option<usize> {
        self.surfaces.iter().position(|s| s.layer.wl_surface() == surface)
    }

    /// Apply one `wl_pointer` frame to the drag.
    ///
    /// A frame groups related events.
    /// It can carry a leave and an enter when the pointer crosses between two of our surfaces.
    /// This loop keeps the event order inside the frame.
    fn pointer_frame(&mut self, events: &[PointerEvent]) {
        for event in events {
            if self.slot_of(&event.surface).is_none() {
                continue;
            }
            match &event.kind {
                PointerEventKind::Enter { serial } => {
                    self.serial = Some(*serial);
                    self.crosshair();
                }
                PointerEventKind::Motion { .. } => {
                    self.ask(Ask::Move, &event.surface, event.position);
                }
                PointerEventKind::Press { button, .. } => {
                    self.ask(ask_of_button(*button, true), &event.surface, event.position);
                }
                PointerEventKind::Release { button, .. } => {
                    self.ask(ask_of_button(*button, false), &event.surface, event.position);
                }
                _ => {}
            }
        }
    }

    /// Apply one input event to one selector surface.
    fn ask(&mut self, ask: Ask, surface: &WlSurface, pos: (f64, f64)) {
        let Some(slot) = self.slot_of(surface) else { return };
        let before = self.drag.rect();
        self.drag.apply(ask, self.surfaces[slot].global(pos));
        self.dirty |= self.drag.rect() != before;
        if self.drag.finished() {
            self.finish();
        }
    }

    /// Apply one `wl_keyboard.key` event, which has no surface.
    /// `Esc` cancels the drag in every state.
    pub fn key(&mut self, code: u32, pressed: bool) {
        if ask_of_key(code, pressed) != Ask::Cancel {
            return;
        }
        self.notes.push("select: cancelled by Esc".to_string());
        self.drag.cancel();
        self.finish();
    }

    /// Cancel the drag when the compositor closes one selector surface.
    /// No surface remains for the drag, so this is a cancel and not an error.
    pub fn closed(&mut self, layer: &LayerSurface) {
        if !self.owns_layer(layer) {
            return;
        }
        self.notes.push("select: cancelled - the compositor closed the selector".to_string());
        self.drag.cancel();
        self.finish();
    }

    /// Cancel the drag when the deadline fires.
    /// This guard stops a compositor that never sends a release.
    /// Without it, that compositor keeps the daemon thread occupied.
    pub fn expired(&mut self) {
        self.notes.push("select: cancelled - the deadline passed with no decision".to_string());
        self.drag.cancel();
        self.finish();
    }

    /// Record a configure size and request a frame.
    ///
    /// The compositor's size is its answer for "the whole output".
    /// That answer is why the selector asks for `0x0` with four anchors.
    /// The selector does not calculate a size. Keep this reason with the code.
    pub fn configured(&mut self, layer: &LayerSurface, size: (u32, u32)) {
        let Some(slot) = self.surfaces.iter().position(|s| &s.layer == layer) else { return };
        self.surfaces[slot].configured = Some((size.0 as i32, size.1 as i32));
        self.dirty = true;
        self.notes.push(format!(
            "select: surface {} configured {}x{} logical at {:.3}x",
            self.surfaces[slot].id, size.0, size.1, self.surfaces[slot].scale
        ));
    }

    /// Set the loop exit signal.
    /// The caller calls this once after the map and before the pump.
    pub fn arm(&mut self, signal: LoopSignal) {
        self.signal = Some(signal);
    }

    /// Stop the nested loop.
    ///
    /// This method is idempotent because several routes share this exit.
    /// A shutdown cancel must not panic.
    ///
    /// It calls `wakeup` as well as `stop` because `stop` only sets a flag.
    /// Without `wakeup`, the loop would stay in `poll` until its next tick.
    /// That delay adds one frame of latency to each release.
    fn finish(&mut self) {
        if let Some(signal) = self.signal.as_ref() {
            signal.stop();
            signal.wakeup();
        }
    }

    /// The equivalent of `IDC_CROSS` when the compositor offers it.
    ///
    /// Without `wp_cursor_shape_v1`, the pointer keeps its current shape.
    /// The popup does not load XCursor themes, and the picker does not change that rule.
    /// The code reports the absent global once at bind.
    fn crosshair(&mut self) {
        let (Some(device), Some(serial)) = (self.shape.as_ref(), self.serial) else { return };
        device.set_shape(serial, Shape::Crosshair);
    }

    /// Run one pump iteration.
    /// Paint changes from the drag, then stop when the drag decides.
    ///
    /// Check the decision here as well as in the handlers.
    /// `calloop::EventLoop::run` clears the stop flag at start.
    /// A pick canceled before the loop starts would otherwise wait until its deadline.
    /// Two causes exist: no pointer on the seat, or a surface closed while the map roundtrip runs.
    pub fn tick(&mut self, pool: &mut SlotPool) {
        self.repaint(pool);
        if self.drag.finished() {
            self.finish();
        }
    }

    /// Paint each surface that needs a frame.
    /// Run this once per iteration, so a burst of motion events costs one commit.
    fn repaint(&mut self, pool: &mut SlotPool) {
        if !std::mem::take(&mut self.dirty) {
            return;
        }
        let sel = self.drag.rect();
        for slot in 0..self.surfaces.len() {
            let Some((buffer, logical)) = self.surfaces[slot].frame() else { continue };
            let local = sel.map(|r| PhysRect {
                x: r.x - self.surfaces[slot].monitor.x,
                y: r.y - self.surfaces[slot].monitor.y,
                w: r.w,
                h: r.h,
            });
            if let Err(e) = draw(&mut self.surfaces[slot], pool, buffer, logical, local) {
                self.notes.push(format!("select: painting surface {} failed: {e:#}", self.surfaces[slot].id));
            }
        }
    }
}

impl Pick {
    /// Remove the selector from the screen and return focus.
    ///
    /// This method destroys every object that the pick created.
    /// Bare proxies have no `Drop` implementation for this work.
    /// It releases the pointer and keyboard, which returns focus.
    /// It destroys the viewports.
    /// It then drops the layer surfaces.
    /// SCTK destroys the role object and the `wl_surface` with them, in the protocol order.
    /// The caller runs a roundtrip afterward, so the compositor sees this work before any grab.
    pub fn destroy(mut self) -> usize {
        if let Some(device) = self.shape.take() {
            device.destroy();
        }
        if let Some(pointer) = self.pointer.take() {
            pointer.release();
        }
        if let Some(keyboard) = self.keyboard.take() {
            keyboard.release();
        }
        let count = self.surfaces.len();
        for surface in self.surfaces.drain(..) {
            if let Some(viewport) = surface.viewport {
                viewport.destroy();
            }
        }
        count
    }
}

/// Raster and commit one selector surface.
fn draw(
    surface: &mut Surface,
    pool: &mut SlotPool,
    buffer: (i32, i32),
    logical: (i32, i32),
    sel: Option<PhysRect>,
) -> Result<()> {
    let (bw, bh) = buffer;
    let wl = surface.layer.wl_surface().clone();
    let (buf, canvas) = pool
        .create_buffer(bw, bh, bw * 4, Format::Argb8888)
        .context("allocating the selector's buffer")?;
    let (px, _) = canvas.as_chunks_mut::<4>();
    paint(px, bw, bh, sel);
    if let Some(viewport) = &surface.viewport {
        viewport.set_destination(logical.0.max(1), logical.1.max(1));
    }
    // The default input region for a layer surface is the whole surface.
    // Do not call `set_input_region`. A picker needs pointer events across this output.
    // Every pointer event on this output belongs to the drag while the selector is visible.
    wl.damage_buffer(0, 0, bw, bh);
    buf.attach_to(&wl).context("attaching the selector's buffer")?;
    surface.layer.commit();
    Ok(())
}

/// The region selector.
pub struct Selector {
    /// The connection on which each pick creates its own queue.
    conn: Connection,
    /// The daemon queue handle.
    /// A pick sends one [`Wake`] sync on this queue to restart the outer pump.
    daemon: QueueHandle<App>,
    /// The popup's `wl_compositor`, cloned once per process.
    compositor: CompositorState,
    shell: LayerShell,
    /// The popup's `wp_viewporter`, cloned for selector use.
    /// The selector creates no fractional-scale object.
    /// The popup's `preferred_scale` is the only scale source.
    /// [`Screen`] passes that scale to a pick.
    viewporter: Option<WpViewporter>,
    shapes: Option<CursorShapeManager>,
    pool: SlotPool,
    notes: Vec<String>,
}

impl Selector {
    /// Bind the selector globals.
    ///
    /// Each pick creates its own surfaces.
    /// A permanent full-output dim would need a full buffer for each monitor.
    /// The selector runs for seconds, so that permanent surface would waste memory.
    /// The popup already rejected the same permanent maximum-size surface.
    ///
    /// `None` means that this compositor advertises no `zwlr_layer_shell_v1`.
    /// This follows the *state, not error* rule that `Popup::bind` follows.
    /// The caller reports the selector as unavailable through its current channel.
    /// Every other channel stays active.
    pub fn bind(
        conn: &Connection,
        globals: &GlobalList,
        qh: &QueueHandle<App>,
        popup: &crate::popup::Popup,
    ) -> Option<Selector> {
        let shell = LayerShell::bind(globals, qh).ok()?;
        // Reserve space for one 1080p screen.
        // The pool grows for larger monitors and keeps this capacity for the daemon lifetime.
        let pool = SlotPool::new(1920 * 1080 * 4, popup.shm()).ok()?;
        let shapes = CursorShapeManager::bind(globals, qh).ok();
        let mut notes = Vec::new();
        if shapes.is_none() {
            notes.push(
                "select: wp_cursor_shape_v1 missing - the pointer keeps its shape over the \
                 selector instead of showing a crosshair"
                    .to_string(),
            );
        }
        Some(Selector {
            conn: conn.clone(),
            daemon: qh.clone(),
            compositor: popup.compositor().clone(),
            shell,
            viewporter: popup.viewporter().cloned(),
            shapes,
            pool,
            notes,
        })
    }

    /// Return diagnostics collected since the last drain.
    /// The selector has no log. The daemon thread owns the log.
    pub fn drain_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// Add one diagnostic from the daemon side of the seam.
    pub fn note(&mut self, line: String) {
        self.notes.push(line);
    }

    /// Return the connection for a pick queue and the daemon queue handle for [`Wake`].
    /// The pick sends [`Wake`] when it exits.
    pub fn handles(&self) -> (Connection, QueueHandle<App>) {
        (self.conn.clone(), self.daemon.clone())
    }

    /// Return the pool for a pick to raster into.
    /// The selector keeps the pool, so a second pick reuses the mmap instead of a new full-screen buffer.
    pub fn pool(&mut self) -> &mut SlotPool {
        &mut self.pool
    }

    /// Let the user drag a region. This call blocks until the user decides.
    ///
    /// This is an associated function, not a `&mut self` method.
    /// The drag state must stay reachable from `&mut App` while the nested pump dispatches into it.
    /// See [`Pick`].
    /// The code therefore reads the selector from `app` with short borrows.
    /// It never holds that borrow across the loop.
    ///
    /// Pass `deadline` as `None` for the product's own [`PICK_TIMEOUT`].
    /// A diagnostic that must expire passes its own value.
    ///
    /// `None` covers every answer without a region:
    /// a cancel, a drag under the threshold, no output, no layer shell, or the timeout.
    /// None is not an error for the caller.
    /// Diagnostics leave through the notes.
    ///
    /// On return, the surfaces are gone and one roundtrip is complete.
    /// A caller that grabs the returned rectangle therefore cannot capture the selector.
    pub fn pick(
        app: &mut App,
        screens: &[Screen],
        deadline: Option<Duration>,
    ) -> Option<PhysRect> {
        match Selector::run(app, screens, deadline.unwrap_or(PICK_TIMEOUT)) {
            Ok(rect) => rect,
            Err(e) => {
                app.selector_note(format!("select: {e:#}"));
                None
            }
        }
    }

    /// Run the fallible half.
    /// Every early exit destroys the surfaces.
    fn run(app: &mut App, screens: &[Screen], deadline: Duration) -> Result<Option<PhysRect>> {
        anyhow::ensure!(!screens.is_empty(), "no output geometry to drag on");
        let (conn, daemon) = app.selector_handles()?;

        // Create a queue for this pick.
        // The daemon queue lives inside the calloop `WaylandSource`.
        // No source callback can dispatch it.
        // A fresh queue per pick leaves no listener between picks.
        let mut queue = conn.new_event_queue::<App>();
        Selector::map(app, &queue.handle(), screens)?;
        // The surfaces exist without configure data.
        // This roundtrip maps them and paints the first dim.
        let mapped = queue.roundtrip(app);

        let outcome = mapped
            .context("mapping the selector's surfaces")
            .and_then(|_| Selector::pump(app, conn.clone(), queue, deadline));

        // The selector is gone, and the compositor has seen its removal.
        // The caller next grabs the returned rectangle.
        // A selector that remains visible would cover those pixels.
        let count = app.pick_finish();
        conn.roundtrip().context("taking the selector down")?;
        // The daemon queue collected events while this loop held the thread.
        // Calloop dispatches them when the socket becomes readable.
        // Sync the daemon queue now so the socket becomes readable.
        conn.display().sync(&daemon, Wake);
        let _ = conn.flush();

        let outcome = outcome?;
        app.selector_note(format!(
            "select: {count} surface(s) down, outcome {}",
            match outcome {
                Outcome::Picked(r) => format!("{}x{} at {},{}", r.w, r.h, r.x, r.y),
                Outcome::Cancelled => "cancelled".to_string(),
                Outcome::Live => "undecided".to_string(),
            }
        ));
        Ok(match outcome {
            Outcome::Picked(rect) => Some(rect),
            _ => None,
        })
    }

    /// Map one `Exclusive`, full-output surface per screen.
    /// Also create this pick's pointer and keyboard.
    fn map(app: &mut App, qh: &QueueHandle<App>, screens: &[Screen]) -> Result<()> {
        let mut pick = {
            let selector = app.selector_mut().context("this compositor has no selector")?;
            let mut surfaces = Vec::with_capacity(screens.len());
            for screen in screens {
                let wl = selector.compositor.create_surface(qh);
                let viewport = selector.viewporter.as_ref().map(|v| v.get_viewport(&wl, qh, ()));
                let layer = selector.shell.create_layer_surface(
                    qh,
                    wl,
                    Layer::Overlay,
                    Some(NAMESPACE),
                    Some(&screen.output),
                );
                // All four edges plus `set_size(0, 0)` let the compositor choose the whole output.
                // This avoids our own logical-size arithmetic and covers every pixel the user can drag over.
                layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
                layer.set_exclusive_zone(-1);
                layer.set_size(0, 0);
                layer.set_margin(0, 0, 0, 0);
                // This is the only surface in the daemon that can take focus.
                // The module documentation explains why the popup cannot.
                layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
                layer.commit();
                surfaces.push(Surface {
                    id: screen.id,
                    monitor: screen.rect,
                    scale: screen.scale,
                    layer,
                    viewport,
                    configured: None,
                });
            }
            selector.notes.push(format!(
                "select: {} full-output surface(s) mapped (overlay layer, keyboard exclusive)",
                surfaces.len()
            ));
            Pick {
                surfaces,
                drag: Drag::default(),
                signal: None,
                pointer: None,
                keyboard: None,
                shape: None,
                serial: None,
                dirty: false,
                notes: Vec::new(),
            }
        };

        // The seat belongs to the popup because one `SeatState` serves the daemon.
        // These pointer and keyboard objects belong to this pick.
        // The code creates them on this pick's queue and releases them with the pick.
        // The keyboard release returns focus.
        //
        // Check both capabilities before any request.
        // `wl_seat.get_keyboard` is a *protocol error* when the seat has no keyboard.
        // That error kills the connection and the daemon's popup, cursor, and control channels.
        // A session without devices must therefore add a note and send no request.
        // A headless compositor is one example.
        // A seat whose keyboard the user unplugged is another.
        match app.seat() {
            Some(seat) => {
                match app.popup_mut().seats().get_pointer(qh, &seat) {
                    Ok(pointer) => pick.pointer = Some(pointer),
                    Err(e) => pick.notes.push(format!(
                        "select: the seat refused a pointer - {e}; there is nothing to drag with"
                    )),
                }
                if app.popup_mut().seats().info(&seat).is_some_and(|i| i.has_keyboard) {
                    pick.keyboard = Some(seat.get_keyboard(qh, SelectKeyboard));
                } else {
                    pick.notes.push(
                        "select: the seat has no keyboard - Esc cannot cancel, so right-click \
                         and the deadline are the only ways out"
                            .to_string(),
                    );
                }
            }
            None => pick
                .notes
                .push("select: no seat - nothing can drag or cancel this selector".to_string()),
        }
        match pick.pointer.clone() {
            Some(pointer) => {
                if let Some(selector) = app.selector_mut() {
                    pick.shape = selector.shapes.as_ref().map(|m| m.get_shape_device(&pointer, qh));
                }
            }
            // No pointer means no drag.
            // Do not hold the daemon thread until the deadline for a result that cannot occur.
            None => pick.drag.cancel(),
        }
        app.pick_start(pick);
        Ok(())
    }

    /// Run the nested pump for this pick's queue and timeout.
    fn pump(
        app: &mut App,
        conn: Connection,
        queue: EventQueue<App>,
        deadline: Duration,
    ) -> Result<Outcome> {
        let mut events: EventLoop<App> =
            EventLoop::try_new().context("creating the selector's event loop")?;
        WaylandSource::new(conn, queue)
            .insert(events.handle())
            .map_err(|e| anyhow::anyhow!("registering the selector's Wayland source: {e}"))?;
        events
            .handle()
            .insert_source(Timer::from_duration(deadline), |_, _, app: &mut App| {
                app.pick_expired();
                TimeoutAction::Drop
            })
            .map_err(|e| anyhow::anyhow!("registering the selector's timeout: {e}"))?;

        app.pick_arm(events.get_signal());
        // Paint once per iteration instead of once per event, with a 16 ms wait cap.
        // A drag sends one motion per compositor frame.
        // One commit per frame matches popup updates from its frame callbacks.
        events
            .run(Some(Duration::from_millis(16)), app, |app| app.pick_tick())
            .context("running the selector's event loop")?;
        Ok(app.pick_outcome())
    }
}

// ---- dispatch ----

/// The selector keyboard, bound from the seat without `seat::keyboard`.
///
/// The SCTK `seat::keyboard` module would add `libxkbcommon` to every Linux build.
/// The module documentation explains that cost.
/// This keyboard needs no keymap, so it carries no state.
#[derive(Debug)]
pub struct SelectKeyboard;

impl Dispatch<WlKeyboard, SelectKeyboard> for App {
    fn event(
        app: &mut App,
        _: &WlKeyboard,
        event: <WlKeyboard as Proxy>::Event,
        _: &SelectKeyboard,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        match event {
            // The keymap arrives as a file descriptor even when the selector does not use it.
            // The event carries an `OwnedFd`.
            // The descriptor closes when the event drops, so no other action is required.
            wl_keyboard::Event::Keymap { .. } => {}
            wl_keyboard::Event::Key { key, state, .. } => {
                app.pick_key(key, state == WEnum::Value(KeyState::Pressed));
            }
            _ => {}
        }
    }
}

/// The `wl_display.sync` callback that a pick sends on the daemon queue at exit.
/// Calloop then wakes and dispatches events that arrived while the nested loop held the thread.
/// This type carries no state and needs no handler.
/// Its arrival marks the end of the pick.
#[derive(Debug)]
pub struct Wake;

impl Dispatch<WlCallback, Wake> for App {
    fn event(
        _: &mut App,
        _: &WlCallback,
        _: <WlCallback as Proxy>::Event,
        _: &Wake,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

/// Handle one `wl_pointer` frame for a pick.
/// Return `false` when no event belongs to the pick.
///
/// This function is the event route.
/// `PointerHandler for App` serves the popup pointer and this pick pointer.
/// It checks this pick first and routes to the popup only when no event belongs here.
pub fn pointer_frame(pick: &mut Pick, events: &[PointerEvent]) -> bool {
    if !events.iter().any(|e| pick.owns(&e.surface)) {
        return false;
    }
    pick.pointer_frame(events);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: i32, y: i32) -> PhysPoint {
        PhysPoint { x, y }
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> PhysRect {
        PhysRect { x, y, w, h }
    }

    #[test]
    fn a_press_a_motion_and_a_release_answer_with_the_box_in_physical_pixels() {
        let mut drag = Drag::default();
        drag.apply(Ask::Start, at(100, 200));
        drag.apply(Ask::Move, at(180, 260));
        assert_eq!(drag.rect(), Some(rect(100, 200, 80, 60)), "the box paints while dragging");
        assert_eq!(drag.outcome(), Outcome::Live, "a drag in flight has not decided");
        drag.apply(Ask::Finish, at(180, 260));
        assert_eq!(drag.outcome(), Outcome::Picked(rect(100, 200, 80, 60)));
        assert!(drag.finished());
    }

    #[test]
    fn a_drag_up_and_to_the_left_is_the_same_box_as_the_same_drag_the_other_way() {
        let mut backwards = Drag::default();
        backwards.apply(Ask::Start, at(180, 260));
        backwards.apply(Ask::Finish, at(100, 200));
        let mut forwards = Drag::default();
        forwards.apply(Ask::Start, at(100, 200));
        forwards.apply(Ask::Finish, at(180, 260));
        assert_eq!(backwards.outcome(), forwards.outcome());
        assert_eq!(backwards.outcome(), Outcome::Picked(rect(100, 200, 80, 60)));
    }

    #[test]
    fn a_drag_under_the_floor_is_discarded_instead_of_returning_a_sliver() {
        // Four px on both axes means a click that moved, not a selection.
        let mut drag = Drag::default();
        drag.apply(Ask::Start, at(10, 10));
        drag.apply(Ask::Finish, at(14, 14));
        assert_eq!(drag.outcome(), Outcome::Cancelled);

        // One axis above the threshold is enough.
        // A thin strip of text is a valid box.
        let mut strip = Drag::default();
        strip.apply(Ask::Start, at(10, 10));
        strip.apply(Ask::Finish, at(60, 12));
        assert_eq!(strip.outcome(), Outcome::Picked(rect(10, 10, 50, 2)));
    }

    #[test]
    fn esc_cancels_a_drag_that_is_already_in_flight_and_nothing_revives_it() {
        let mut drag = Drag::default();
        drag.apply(Ask::Start, at(0, 0));
        drag.apply(Ask::Move, at(400, 400));
        drag.apply(ask_of_key(KEY_ESC, true), at(400, 400));
        assert_eq!(drag.outcome(), Outcome::Cancelled, "a big box does not survive Esc");
        drag.apply(Ask::Finish, at(400, 400));
        assert_eq!(drag.outcome(), Outcome::Cancelled, "the release after the cancel is ignored");
    }

    #[test]
    fn a_right_click_cancels_a_drag_in_flight_and_the_release_that_follows_is_inert() {
        let mut drag = Drag::default();
        drag.apply(Ask::Start, at(0, 0));
        drag.apply(Ask::Move, at(400, 400));
        drag.apply(ask_of_button(BTN_RIGHT, true), at(400, 400));
        assert_eq!(drag.outcome(), Outcome::Cancelled, "right-click gets out mid-drag");
        // A right release requests no action.
        // A later left release cannot reopen the drag.
        assert_eq!(ask_of_button(BTN_RIGHT, false), Ask::Ignore);
        drag.apply(Ask::Finish, at(400, 400));
        assert_eq!(drag.outcome(), Outcome::Cancelled);
    }

    #[test]
    fn only_esc_and_the_right_button_cancel_and_only_the_left_button_drags() {
        assert_eq!(ask_of_key(KEY_ESC, true), Ask::Cancel);
        assert_eq!(ask_of_key(KEY_ESC, false), Ask::Ignore, "the Esc release decides nothing");
        // Space, Enter, and the other keys do not affect the selector.
        for code in [28u32, 57, 15, 103] {
            assert_eq!(ask_of_key(code, true), Ask::Ignore, "key {code} must not cancel");
        }
        assert_eq!(ask_of_button(BTN_LEFT, true), Ask::Start);
        assert_eq!(ask_of_button(BTN_LEFT, false), Ask::Finish);
        // The middle button, `BTN_MIDDLE`, neither starts nor cancels the drag.
        assert_eq!(ask_of_button(0x112, true), Ask::Ignore);
    }

    #[test]
    fn a_release_with_no_press_behind_it_decides_nothing() {
        let mut drag = Drag::default();
        drag.apply(Ask::Move, at(50, 50));
        assert_eq!(drag.rect(), None, "a motion before the press paints no box");
        drag.apply(Ask::Finish, at(50, 50));
        assert_eq!(drag.outcome(), Outcome::Live, "the pick is still waiting for a real drag");
    }

    #[test]
    fn the_deadline_and_a_closed_surface_cancel_without_a_pointer_position() {
        let mut drag = Drag::default();
        drag.apply(Ask::Start, at(10, 10));
        drag.apply(Ask::Move, at(200, 200));
        drag.cancel();
        assert_eq!(drag.outcome(), Outcome::Cancelled, "the wedge guard answers like every cancel");
    }

    #[test]
    fn a_surface_local_logical_position_becomes_a_global_physical_point() {
        // This matches `Surface::global` without a compositor.
        // It uses the second monitor at x 1920 and scale 1.5.
        let convert = |pos: (f64, f64), monitor: PhysRect, scale: f64| PhysPoint {
            x: monitor.x + (pos.0 * scale).floor() as i32,
            y: monitor.y + (pos.1 * scale).floor() as i32,
        };
        let monitor = rect(1920, 0, 2560, 1440);
        assert_eq!(convert((0.0, 0.0), monitor, 1.5), at(1920, 0));
        assert_eq!(convert((100.0, 50.0), monitor, 1.5), at(2070, 75));
        // Use floor, not round.
        // At 1.5x, 0.9 logical px maps inside physical pixel 1.
        assert_eq!(convert((0.9, 0.9), monitor, 1.5), at(1921, 1));
    }

    #[test]
    fn a_drag_across_two_outputs_answers_one_box_in_the_global_space() {
        // The press starts on the left monitor and the release ends on the right.
        // Each surface uses its own origin and scale.
        // The result is their union in core's global space.
        let left = |pos: (f64, f64)| PhysPoint { x: (pos.0 * 1.0) as i32, y: (pos.1 * 1.0) as i32 };
        let right = |pos: (f64, f64)| PhysPoint {
            x: 1920 + (pos.0 * 2.0) as i32,
            y: (pos.1 * 2.0) as i32,
        };
        let mut drag = Drag::default();
        drag.apply(Ask::Start, left((1800.0, 500.0)));
        drag.apply(Ask::Finish, right((100.0, 300.0)));
        assert_eq!(drag.outcome(), Outcome::Picked(rect(1800, 500, 320, 100)));
    }

    #[test]
    fn the_selection_is_punched_clear_of_the_dim_and_framed_just_outside_it() {
        let (w, h) = (20, 20);
        let mut px = vec![DIM; (w * h) as usize];
        paint(&mut px, w, h, Some(rect(6, 6, 8, 8)));
        let at = |x: i32, y: i32| px[(y * w + x) as usize];

        assert_eq!(at(0, 0), DIM, "outside the frame stays dimmed");
        assert_eq!(at(10, 10), overlay::CLEAR, "the selection itself is punched clear");
        assert_eq!(at(6, 6), overlay::CLEAR, "the selection's own corner is clear");
        assert_eq!(at(13, 13), overlay::CLEAR, "and its far corner too");
        // The frame is two px outside the selection on every side.
        for (x, y) in [(4, 10), (5, 10), (10, 4), (10, 5), (14, 10), (15, 10), (10, 14), (10, 15)] {
            assert_eq!(at(x, y), FRAME, "the frame pixel at {x},{y}");
        }
        assert_eq!(at(3, 10), DIM, "and nothing thicker than 2 px");
    }

    #[test]
    fn a_selector_frame_with_no_drag_yet_is_dim_all_over() {
        let (w, h) = (8, 8);
        let mut px = vec![overlay::CLEAR; (w * h) as usize];
        paint(&mut px, w, h, None);
        assert!(px.iter().all(|p| *p == DIM), "before the press there is only the dim");
    }

    #[test]
    fn a_selection_hanging_off_the_output_is_clipped_rather_than_written_past_the_buffer() {
        let (w, h) = (10, 10);
        let mut px = vec![overlay::CLEAR; (w * h) as usize];
        // The drag starts on the monitor to the left, so this surface sees negative coordinates.
        paint(&mut px, w, h, Some(rect(-40, -40, 45, 45)));
        assert_eq!(px.len(), 100, "the buffer must not have been resized");
        assert_eq!(px[0], overlay::CLEAR, "the part of the selection that reaches here is clear");
        assert_eq!(px[(9 * 10 + 9) as usize], DIM, "the far corner is still dim");
    }

    #[test]
    fn the_dim_is_a_translucent_black_rather_than_an_opaque_one() {
        // The selector shows the live screen through a dim layer.
        // Alpha must be partial and premultiplied for `wl_shm`.
        assert_eq!(DIM, [0, 0, 0, 102], "40% black, premultiplied");
        assert!(DIM[3] < 255, "an opaque dim would hide the screen being selected from");
    }
}
