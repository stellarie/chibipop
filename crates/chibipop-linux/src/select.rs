//! The region selector: drag a box on a dimmed screen, get a
//! `PhysRect` back.
//!
//! The Linux answer to the Windows bin's `action/selection.rs`, and it
//! looks the same on purpose: the live screen dimmed to 40 % black with
//! the drag rectangle punched clear and a 2 px white frame round it, a
//! crosshair cursor, `Esc` or right-click to cancel, and drags under
//! [`MIN_DRAG_PX`] discarded as accidental clicks. One
//! `zwlr_layer_shell_v1` surface per output on the `Overlay` layer,
//! anchored to all four edges so the compositor sizes it to the whole
//! output.
//!
//! **The live screen, not a frozen grab** (spec D5). Nothing is captured
//! before the drag: the dim is a translucent surface over whatever is
//! there, so the user drags against what they are actually looking at
//! and the region is grabbed *after* this surface is down - which is
//! also the only way the grab cannot capture the selector itself.
//! [`Selector::pick`] therefore destroys its surfaces and completes a
//! round trip before it returns.
//!
//! **Why this may take keyboard focus and the popup may not.** ADR-0004
//! makes `keyboard_interactivity = none` inviolable for the popup:
//! focus-stealing has to be impossible by construction for a surface
//! that appears on every hover, unasked. The selector is the exact
//! opposite - it exists only because the user pressed a key to ask for
//! it, it is modal for as long as it is up, and its whole contract is
//! "the next drag or `Esc` decides". A picker that could not hear `Esc`
//! would have no way out but a successful drag. So this surface, and
//! only this surface, sets `Exclusive` - and gives the focus back by
//! destroying itself.
//!
//! **How it pumps.** [`Selector::pick`] is a blocking nested dispatch
//! loop on the daemon thread, the analogue of Windows' nested
//! `GetMessageW` pump. The daemon's own queue lives inside calloop's
//! `WaylandSource` and cannot be dispatched from within a source
//! callback, so a pick makes a *second* `EventQueue` on the same
//! `Connection` for its own objects and runs it in a throwaway
//! `calloop::EventLoop` with a [`PICK_TIMEOUT`]-length `Timer`. Two
//! consequences, both wanted: a compositor that delivers a press and
//! then no release cannot wedge the daemon (the timer cancels the pick),
//! and this is re-entrant from any callback - including a pointer click
//! on the popup - because nothing borrows the outer loop.
//!
//! Events for the daemon's queue that arrive while a pick has the thread
//! are not lost: `wayland-client` distributes each read to every queue.
//! They are, however, only *dispatched* when calloop next sees the
//! socket readable, so a pick ends with a `wl_display.sync` on the
//! daemon's queue ([`Wake`]) to guarantee that happens at once.
//!
//! **The keyboard is bound raw, not through SCTK's `seat::keyboard`.**
//! That module needs SCTK's `xkbcommon` feature, which is a build-time
//! `pkg-config` + `libxkbcommon` dependency for the whole workspace
//! (cargo unifies features), and would be paid by every Linux build for
//! one key. `Esc` is a physical key with a fixed evdev code
//! ([`KEY_ESC`]), which is exactly what `wl_keyboard.key` carries, so no
//! keymap is needed to recognise it - and "the physical Esc key cancels"
//! is then layout-independent by construction rather than by lookup.

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

/// `hyprctl layers` and `layerrule` see this.
const NAMESPACE: &str = "chibipop-select";

/// Shortest drag that counts, physical px. The Windows bin's
/// `MIN_DRAG_PX`: a stray click while the selector is up is a cancel,
/// not a one-pixel screenshot.
pub const MIN_DRAG_PX: i32 = 5;

/// Selection frame thickness, physical px. The Windows bin's
/// `BORDER_PX`.
pub const BORDER_PX: i32 = 2;

/// The dim, premultiplied: black at the Windows bin's `DIM_ALPHA` of
/// 102/255. Premultiplying black leaves the colour channels at zero, so
/// this value is alpha and nothing else.
pub const DIM: Px = [0, 0, 0, 102];

/// The selection frame: opaque white, as on Windows.
pub const FRAME: Px = [0xFF, 0xFF, 0xFF, 0xFF];

/// `KEY_ESC` from `linux/input-event-codes.h`, which is what
/// `wl_keyboard.key` reports - the evdev code, not an xkb keysym.
pub const KEY_ESC: u32 = 1;

/// `BTN_RIGHT` from `linux/input-event-codes.h`. SCTK exports
/// [`BTN_LEFT`] and not this one.
pub const BTN_RIGHT: u32 = 0x111;

/// How long a pick waits before cancelling itself.
///
/// The guard the nested pump needs: a compositor that delivers a press
/// and then no release at all would otherwise hold the daemon's only
/// thread forever - no cursor samples, no control socket, no popup.
/// Twenty seconds is far longer than any real drag and short enough that
/// a wedged session recovers on its own. Passed in rather than read here
/// so a diagnostic can ask for a pick it knows will expire.
pub const PICK_TIMEOUT: Duration = Duration::from_secs(20);

/// Two drag corners as a rect, in whichever order they came.
///
/// The Windows bin's `normalized_rect`: a drag up-and-left is the same
/// box as the same drag down-and-right, because a user does not think in
/// signs.
pub fn normalized(a: PhysPoint, b: PhysPoint) -> PhysRect {
    PhysRect {
        x: a.x.min(b.x),
        y: a.y.min(b.y),
        w: (a.x - b.x).abs(),
        h: (a.y - b.y).abs(),
    }
}

/// Is this drag a drag, or a click that moved a little?
///
/// The Windows bin's `meets_drag_threshold`, `||` included: a thin
/// horizontal strip of text is a legitimate selection, so one axis
/// clearing the floor is enough.
pub fn meets_threshold(r: PhysRect) -> bool {
    r.w >= MIN_DRAG_PX || r.h >= MIN_DRAG_PX
}

/// What a pick ended as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Still dragging, or still waiting for the first press.
    Live,
    /// A box past the threshold.
    Picked(PhysRect),
    /// `Esc`, a right-click, an under-threshold drag, a closed surface or
    /// the timeout. Every one of them is the same answer to the caller:
    /// no region, and nothing it has to phrase as an error.
    Cancelled,
}

/// What one input event asks of a live pick.
///
/// The selector's whole input contract as data, so the routing is
/// testable without a compositor and the two cancels - `Esc` and
/// right-click - are visibly the same answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// A left press: the drag starts here.
    Start,
    /// The pointer moved.
    Move,
    /// A left release: the drag ends here.
    Finish,
    /// `Esc`, a right press, a closed surface, or the deadline.
    Cancel,
    /// Anything else the selector sees and does not act on.
    Ignore,
}

/// What one pointer button asks.
///
/// Right-click is a cancel exactly as on Windows, and it is decided
/// before the left button so a right-click *during* a drag still gets
/// out. A right *release* asks nothing: the press already decided.
pub fn ask_of_button(button: u32, pressed: bool) -> Ask {
    match (button, pressed) {
        (BTN_RIGHT, true) => Ask::Cancel,
        (BTN_LEFT, true) => Ask::Start,
        (BTN_LEFT, false) => Ask::Finish,
        _ => Ask::Ignore,
    }
}

/// What one key asks. Raw evdev codes: see the module doc for why there
/// is no keymap here.
pub fn ask_of_key(code: u32, pressed: bool) -> Ask {
    if pressed && code == KEY_ESC {
        Ask::Cancel
    } else {
        Ask::Ignore
    }
}

/// The drag, as a state machine over plain data.
///
/// Free of Wayland so the arithmetic is testable without a compositor:
/// the same [`Drag::apply`] serves a real `wl_pointer` frame and a test.
/// Coordinates in and out are **global physical pixels** - core's only
/// coordinate space - so the surface-local logical units `wl_pointer`
/// speaks are converted once, on the way in ([`Surface::global`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Drag {
    /// Where the press landed, while a drag is in flight.
    anchor: Option<PhysPoint>,
    /// The newest position, so a motion has something to draw against.
    at: Option<PhysPoint>,
    done: Option<Outcome>,
}

impl Drag {
    /// The box to paint right now, if any.
    pub fn rect(&self) -> Option<PhysRect> {
        Some(normalized(self.anchor?, self.at?))
    }

    /// Has this pick finished, and how?
    pub fn outcome(&self) -> Outcome {
        self.done.unwrap_or(Outcome::Live)
    }

    pub fn finished(&self) -> bool {
        self.done.is_some()
    }

    /// One input event applied, at a point in global physical pixels.
    ///
    /// Nothing moves after the decision: the first [`Ask::Finish`] or
    /// [`Ask::Cancel`] is the answer, and a trailing release cannot
    /// revive a cancelled pick.
    pub fn apply(&mut self, ask: Ask, at: PhysPoint) {
        if self.done.is_some() {
            return;
        }
        match ask {
            Ask::Start => {
                self.anchor = Some(at);
                self.at = Some(at);
            }
            // Ignored before the press, which is what keeps the dim
            // surface inert until the user commits to a drag.
            Ask::Move => {
                if self.anchor.is_some() {
                    self.at = Some(at);
                }
            }
            Ask::Finish => {
                // A release with no press of ours behind it - the button
                // went down before the surface was up - decides nothing.
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

    /// [`Ask::Cancel`] from somewhere with no pointer position behind it:
    /// a closed surface, or the deadline.
    pub fn cancel(&mut self) {
        self.apply(Ask::Cancel, PhysPoint { x: 0, y: 0 });
    }
}

/// Paint one output's selector frame.
///
/// Dim everywhere, the selection punched clear, a frame just outside it -
/// the Windows bin's `punch_through` + `paint_border`, in one pass over
/// premultiplied `Argb8888`. `sel` is surface-local physical pixels and
/// may hang off any edge; every fill clips.
pub fn paint(px: &mut [Px], w: i32, h: i32, sel: Option<PhysRect>) {
    px.fill(DIM);
    let Some(sel) = sel.filter(|s| s.w > 0 && s.h > 0) else { return };
    // Clear first, then frame: the frame sits *outside* the selection,
    // exactly as on Windows, so the punched box is the box that will be
    // grabbed and the border never eats a row of it.
    overlay::fill(px, w, h, sel, overlay::CLEAR);
    for strip in overlay::strips(sel.inflated(BORDER_PX, BORDER_PX), BORDER_PX) {
        overlay::fill(px, w, h, strip, FRAME);
    }
}

/// One output's selector surface.
struct Surface {
    /// The popup's stable surface id for this output, so every
    /// diagnostic this daemon writes means one monitor by "surface 1".
    id: usize,
    /// This output's box in the global physical space: what turns a
    /// surface-local pointer position into core's coordinate space.
    monitor: PhysRect,
    /// The scale to raster at. Read once per pick - a pick is seconds
    /// long, and a scale change mid-drag would move the box under the
    /// user's hand.
    scale: f64,
    layer: LayerSurface,
    viewport: Option<WpViewport>,
    /// The logical size the compositor configured. `None` until it has:
    /// the surface is anchored to all four edges with `set_size(0, 0)`,
    /// so the compositor - not our own arithmetic - decides what "the
    /// whole output" is.
    configured: Option<(i32, i32)>,
}

impl Surface {
    /// Surface-local logical -> global physical.
    ///
    /// Floor, not round, for the popup's `HitScene::local` reason: the
    /// answer names the pixel the pointer is inside. The output's own
    /// origin is added because core counts in one global space while a
    /// layer surface counts from its own corner.
    fn global(&self, pos: (f64, f64)) -> PhysPoint {
        PhysPoint {
            x: self.monitor.x + (pos.0 * self.scale).floor() as i32,
            y: self.monitor.y + (pos.1 * self.scale).floor() as i32,
        }
    }

    /// The buffer to raster and the logical size to advertise, once the
    /// compositor has configured one.
    fn frame(&self) -> Option<((i32, i32), (i32, i32))> {
        let logical = self.configured?;
        let buffer = (
            ((f64::from(logical.0) * self.scale).round() as i32).max(1),
            ((f64::from(logical.1) * self.scale).round() as i32).max(1),
        );
        Some((buffer, logical))
    }
}

/// The live half of one pick: the surfaces, the drag, and the way out.
///
/// This lives on [`App`] rather than inside [`Selector`] for one
/// concrete reason: the SCTK handlers that drive the drag are written on
/// `App`, so everything they must mutate has to be reachable from
/// `&mut App` - and the nested pump dispatches `&mut App`, which cannot
/// simultaneously be borrowing a `&mut Selector` out of it.
pub struct Pick {
    surfaces: Vec<Surface>,
    drag: Drag,
    /// Stops the nested loop the moment the drag decides. `None` only
    /// between building the surfaces and starting the loop.
    signal: Option<LoopSignal>,
    /// The pointer and keyboard this pick owns, on its own queue. Both
    /// are released with it, which is what hands the focus back.
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,
    shape: Option<WpCursorShapeDeviceV1>,
    /// The last `enter` serial, which `set_shape` has to quote.
    serial: Option<u32>,
    /// A frame is owed because the drag moved.
    dirty: bool,
    notes: Vec<String>,
}

impl Pick {
    /// Does one `wl_surface` belong to this pick? The routing question
    /// the shared SCTK handlers ask before assuming a surface is the
    /// popup's.
    pub fn owns(&self, surface: &WlSurface) -> bool {
        self.surfaces.iter().any(|s| s.layer.wl_surface() == surface)
    }

    /// The same question for a `configure`/`closed`, which name a layer
    /// surface rather than a `wl_surface`.
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

    /// One `wl_pointer` frame, sorted into the drag.
    ///
    /// A frame batches related events and may carry a leave and an enter
    /// together when the pointer crosses between two of our surfaces, so
    /// the order inside it is kept.
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

    /// One input event on one of our surfaces, applied.
    fn ask(&mut self, ask: Ask, surface: &WlSurface, pos: (f64, f64)) {
        let Some(slot) = self.slot_of(surface) else { return };
        let before = self.drag.rect();
        self.drag.apply(ask, self.surfaces[slot].global(pos));
        self.dirty |= self.drag.rect() != before;
        if self.drag.finished() {
            self.finish();
        }
    }

    /// One `wl_keyboard.key`, which names no surface: `Esc` cancels
    /// whatever the drag was doing.
    pub fn key(&mut self, code: u32, pressed: bool) {
        if ask_of_key(code, pressed) != Ask::Cancel {
            return;
        }
        self.notes.push("select: cancelled by Esc".to_string());
        self.drag.cancel();
        self.finish();
    }

    /// The compositor closed one of our surfaces mid-pick. There is
    /// nothing left to drag on, so this is a cancel and not an error.
    pub fn closed(&mut self, layer: &LayerSurface) {
        if !self.owns_layer(layer) {
            return;
        }
        self.notes.push("select: cancelled - the compositor closed the selector".to_string());
        self.drag.cancel();
        self.finish();
    }

    /// The deadline fired: the guard that keeps a compositor which
    /// never delivers a release from holding the daemon's only thread.
    pub fn expired(&mut self) {
        self.notes.push("select: cancelled - the deadline passed with no decision".to_string());
        self.drag.cancel();
        self.finish();
    }

    /// A configure landed: record the size and owe a frame.
    ///
    /// The size is the compositor's own answer to "the whole output",
    /// which is why the selector asks for `0x0` on four anchors rather
    /// than computing it - so it is worth a line.
    pub fn configured(&mut self, layer: &LayerSurface, size: (u32, u32)) {
        let Some(slot) = self.surfaces.iter().position(|s| &s.layer == layer) else { return };
        self.surfaces[slot].configured = Some((size.0 as i32, size.1 as i32));
        self.dirty = true;
        self.notes.push(format!(
            "select: surface {} configured {}x{} logical at {:.3}x",
            self.surfaces[slot].id, size.0, size.1, self.surfaces[slot].scale
        ));
    }

    /// Arm the way out. Called once, between mapping and pumping.
    pub fn arm(&mut self, signal: LoopSignal) {
        self.signal = Some(signal);
    }

    /// Leave the nested loop. Idempotent: several routes share this
    /// exit, and a cancel during teardown must not panic.
    ///
    /// `wakeup` as well as `stop`, because `stop` only sets a flag:
    /// without it the loop would sit in `poll` until its next tick
    /// before noticing, which is a frame of latency on every release.
    fn finish(&mut self) {
        if let Some(signal) = self.signal.as_ref() {
            signal.stop();
            signal.wakeup();
        }
    }

    /// `IDC_CROSS`'s equivalent, where the compositor offers one.
    ///
    /// Without `wp_cursor_shape_v1` the cursor is left exactly as it
    /// was: loading XCursor themes ourselves is what ADR-0004 refused
    /// for the popup, and a picker is not a reason to change that. The
    /// absence is said once, at bind.
    fn crosshair(&mut self) {
        let (Some(device), Some(serial)) = (self.shape.as_ref(), self.serial) else { return };
        device.set_shape(serial, Shape::Crosshair);
    }

    /// One pump iteration: draw what the drag changed, then leave if it
    /// has decided.
    ///
    /// The decision is re-checked here and not only in the handlers
    /// because `calloop::EventLoop::run` clears the stop flag as it
    /// starts - a pick that was already cancelled before the loop began
    /// (no pointer on the seat, a surface closed during the mapping
    /// round trip) would otherwise sit until its deadline.
    pub fn tick(&mut self, pool: &mut SlotPool) {
        self.repaint(pool);
        if self.drag.finished() {
            self.finish();
        }
    }

    /// Every surface that owes a frame, drawn. Once per iteration, so a
    /// burst of motion events costs one commit.
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
    /// Take the selector off the screen and give the focus back.
    ///
    /// Every object this pick created is destroyed here rather than left
    /// to a `Drop` that does not exist for bare proxies: the pointer and
    /// keyboard are released (which is what returns focus), the
    /// viewports destroyed, and the layer surfaces dropped - SCTK
    /// destroys the role object and the `wl_surface` with them, in that
    /// order, as the protocol requires. The caller round-trips
    /// afterwards, so this is *seen* to have happened before any grab.
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
    // No `set_input_region`: a layer surface's default region is the
    // whole surface, which is exactly what a picker wants - every
    // pointer event on this output belongs to the drag while it is up.
    wl.damage_buffer(0, 0, bw, bh);
    buf.attach_to(&wl).context("attaching the selector's buffer")?;
    surface.layer.commit();
    Ok(())
}

/// The region selector.
pub struct Selector {
    /// The connection every pick makes its own queue on.
    conn: Connection,
    /// The daemon's queue handle, for the one request a pick sends on
    /// it: the [`Wake`] sync that gets the outer pump moving again.
    daemon: QueueHandle<App>,
    /// The popup's `wl_compositor`, cloned: one per process.
    compositor: CompositorState,
    shell: LayerShell,
    /// The popup's `wp_viewporter`, cloned. No fractional-scale object:
    /// the popup's `preferred_scale` is the one source of scale truth
    /// and reaches a pick through [`Screen`].
    viewporter: Option<WpViewporter>,
    shapes: Option<CursorShapeManager>,
    pool: SlotPool,
    notes: Vec<String>,
}

impl Selector {
    /// Bind the selector's globals. Surfaces are per pick: a full-output
    /// dim held permanently would be a screenful of buffer per monitor
    /// for a thing used seconds at a time, which is the permanent
    /// maximum-size surface ADR-0004 already rejected once.
    ///
    /// `None` means this compositor advertises no `zwlr_layer_shell_v1` -
    /// the same *state, not error* rule `Popup::bind` follows (ADR-0004,
    /// ticket 49): the caller reports the selector unavailable through
    /// the channel it already has, and every other channel keeps
    /// running.
    pub fn bind(
        conn: &Connection,
        globals: &GlobalList,
        qh: &QueueHandle<App>,
        popup: &crate::popup::Popup,
    ) -> Option<Selector> {
        let shell = LayerShell::bind(globals, qh).ok()?;
        // One 1080p screenful; the pool grows on demand for bigger
        // monitors and keeps the room, which is right for a struct that
        // lives as long as the daemon.
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

    /// Diagnostics accumulated since the last drain. The selector owns no
    /// log; the daemon thread does.
    pub fn drain_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// One diagnostic, from the daemon side of the seam.
    pub fn note(&mut self, line: String) {
        self.notes.push(line);
    }

    /// The connection a pick makes its own queue on, and the daemon's
    /// queue handle for the [`Wake`] it fires on the way out.
    pub fn handles(&self) -> (Connection, QueueHandle<App>) {
        (self.conn.clone(), self.daemon.clone())
    }

    /// The pool a pick rasters into. Lives on the selector rather than
    /// the pick so a second pick reuses the mmap instead of paying for
    /// a screenful again.
    pub fn pool(&mut self) -> &mut SlotPool {
        &mut self.pool
    }

    /// Drag a region, blocking until the user decides.
    ///
    /// An associated function rather than a `&mut self` method on
    /// purpose: the drag state has to be reachable from `&mut App` while
    /// the nested pump dispatches into it (see [`Pick`]), so the selector
    /// is read out of `app` in short borrows instead of held across the
    /// loop.
    ///
    /// `deadline` is `None` for the product's own [`PICK_TIMEOUT`]; a
    /// diagnostic that wants a pick it knows will expire passes its own.
    ///
    /// `None` covers every way this can fail to answer with a region -
    /// cancelled, under the threshold, no output to drag on, no layer
    /// shell, or the timeout - and none of them is an error the caller
    /// has to phrase. Diagnostics come out through the notes.
    ///
    /// On return the surfaces are destroyed and a round trip has
    /// completed, so a caller that grabs the returned rect cannot
    /// capture the selector (spec D5).
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

    /// The fallible half, so every early exit still tears the surfaces
    /// down on the way out.
    fn run(app: &mut App, screens: &[Screen], deadline: Duration) -> Result<Option<PhysRect>> {
        anyhow::ensure!(!screens.is_empty(), "no output geometry to drag on");
        let (conn, daemon) = app.selector_handles()?;

        // A queue of this pick's own: the daemon's lives inside
        // calloop's `WaylandSource` and cannot be dispatched from a
        // source callback, and a fresh one per pick leaves nothing
        // listening between picks.
        let mut queue = conn.new_event_queue::<App>();
        Selector::map(app, &queue.handle(), screens)?;
        // The surfaces exist but are not configured; this round trip is
        // what maps them and paints the first dim.
        let mapped = queue.roundtrip(app);

        let outcome = mapped
            .context("mapping the selector's surfaces")
            .and_then(|_| Selector::pump(app, conn.clone(), queue, deadline));

        // Down, and *seen* to be down: the caller's next act is a grab
        // of the rect this returns, and a selector still on screen would
        // be in those pixels.
        let count = app.pick_finish();
        conn.roundtrip().context("taking the selector down")?;
        // The daemon's queue collected events while this loop had the
        // thread; calloop dispatches them the next time the socket is
        // readable, so make it readable now.
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

    /// One `Exclusive`, full-output surface per screen, plus this pick's
    /// own pointer and keyboard.
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
                // All four edges plus `set_size(0, 0)`: the compositor
                // decides what the whole output is, which beats trusting
                // our own logical-size arithmetic for a surface that
                // must cover every pixel the user can drag over.
                layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
                layer.set_exclusive_zone(-1);
                layer.set_size(0, 0);
                layer.set_margin(0, 0, 0, 0);
                // The one surface in this daemon that may take focus.
                // See the module doc for why the popup may not.
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

        // The seat is the popup's - one `SeatState` serves the daemon -
        // but these two objects are this pick's own, created on this
        // pick's queue and released with it. Releasing the keyboard is
        // what hands the focus back.
        //
        // Both capabilities are checked first. `wl_seat.get_keyboard` on
        // a seat with no keyboard is a *protocol error*, which kills the
        // whole connection and with it the daemon's popup, cursor and
        // control channels - so a device-less session (a headless
        // compositor, a seat whose keyboard was unplugged) must be a
        // note here and never a request.
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
            // No pointer is no drag, and waiting out the deadline to say
            // so would hold the daemon's thread for nothing.
            None => pick.drag.cancel(),
        }
        app.pick_start(pick);
        Ok(())
    }

    /// The nested pump: this pick's queue plus a timeout, in a loop of
    /// their own.
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
        // One repaint per iteration rather than per event, capped at a
        // 16 ms wait: a drag delivers a motion per compositor frame, and
        // one commit per frame is the pacing the popup gets from its
        // frame callbacks.
        events
            .run(Some(Duration::from_millis(16)), app, |app| app.pick_tick())
            .context("running the selector's event loop")?;
        Ok(app.pick_outcome())
    }
}

// ---- the dispatch plumbing ----

/// The selector's keyboard, bound raw off the seat.
///
/// SCTK's `seat::keyboard` would pull `libxkbcommon` into every Linux
/// build of the workspace (see the module doc); this needs no keymap, so
/// it carries no state at all.
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
            // The keymap arrives as a file descriptor whether anyone
            // wants it or not; the `OwnedFd` the event carries closes
            // when it drops here, which is the whole handling needed.
            wl_keyboard::Event::Keymap { .. } => {}
            wl_keyboard::Event::Key { key, state, .. } => {
                app.pick_key(key, state == WEnum::Value(KeyState::Pressed));
            }
            _ => {}
        }
    }
}

/// The `wl_display.sync` a pick fires on the *daemon's* queue on its way
/// out, so calloop wakes and dispatches whatever piled up there while
/// the nested loop had the thread. It carries no state and needs no
/// handling: arriving is the whole point.
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

/// One `wl_pointer` frame for a pick, or `false` when none of it was
/// ours.
///
/// The routing seam: `PointerHandler for App` serves the popup's pointer
/// and this pick's at once, so it asks here first and only falls through
/// to the popup when the answer is no.
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
        // Four px on both axes: a click that moved, not a selection.
        let mut drag = Drag::default();
        drag.apply(Ask::Start, at(10, 10));
        drag.apply(Ask::Finish, at(14, 14));
        assert_eq!(drag.outcome(), Outcome::Cancelled);

        // One axis clearing the floor is enough: a thin strip of text is
        // a legitimate box.
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
        // The right *release* asks nothing, and a left release after the
        // decision cannot re-open it.
        assert_eq!(ask_of_button(BTN_RIGHT, false), Ask::Ignore);
        drag.apply(Ask::Finish, at(400, 400));
        assert_eq!(drag.outcome(), Outcome::Cancelled);
    }

    #[test]
    fn only_esc_and_the_right_button_cancel_and_only_the_left_button_drags() {
        assert_eq!(ask_of_key(KEY_ESC, true), Ask::Cancel);
        assert_eq!(ask_of_key(KEY_ESC, false), Ask::Ignore, "the Esc release decides nothing");
        // Space, Enter and the rest are not the selector's business.
        for code in [28u32, 57, 15, 103] {
            assert_eq!(ask_of_key(code, true), Ask::Ignore, "key {code} must not cancel");
        }
        assert_eq!(ask_of_button(BTN_LEFT, true), Ask::Start);
        assert_eq!(ask_of_button(BTN_LEFT, false), Ask::Finish);
        // The middle button (BTN_MIDDLE) is neither.
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
        // The conversion `Surface::global` performs, without a
        // compositor to make one: second monitor at x 1920, 1.5x.
        let convert = |pos: (f64, f64), monitor: PhysRect, scale: f64| PhysPoint {
            x: monitor.x + (pos.0 * scale).floor() as i32,
            y: monitor.y + (pos.1 * scale).floor() as i32,
        };
        let monitor = rect(1920, 0, 2560, 1440);
        assert_eq!(convert((0.0, 0.0), monitor, 1.5), at(1920, 0));
        assert_eq!(convert((100.0, 50.0), monitor, 1.5), at(2070, 75));
        // Floor, not round: 0.9 logical px at 1.5x is inside physical 1.
        assert_eq!(convert((0.9, 0.9), monitor, 1.5), at(1921, 1));
    }

    #[test]
    fn a_drag_across_two_outputs_answers_one_box_in_the_global_space() {
        // Press on the left monitor, release on the right one: each
        // surface converts through its own origin and scale, and the
        // box is the union in the one global space core counts in.
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
        // The frame is the two px just outside, on every side.
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
        // The drag started on the monitor to the left, so this surface
        // sees negative coordinates.
        paint(&mut px, w, h, Some(rect(-40, -40, 45, 45)));
        assert_eq!(px.len(), 100, "the buffer must not have been resized");
        assert_eq!(px[0], overlay::CLEAR, "the part of the selection that reaches here is clear");
        assert_eq!(px[(9 * 10 + 9) as usize], DIM, "the far corner is still dim");
    }

    #[test]
    fn the_dim_is_a_translucent_black_rather_than_an_opaque_one() {
        // Spec D5: the selector shows the live screen dimmed, so the
        // alpha must be partial and premultiplied to match `wl_shm`.
        assert_eq!(DIM, [0, 0, 0, 102], "40% black, premultiplied");
        assert!(DIM[3] < 255, "an opaque dim would hide the screen being selected from");
    }
}
