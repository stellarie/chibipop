//! The capture backend's own Wayland connection: registry binds,
//! output geometry, frame bookkeeping.
//!
//! Its own connection and its own queue, on purpose. The backend lives
//! on the core Worker's thread (`Worker::spawn`'s `open` closure builds
//! it there), and the daemon's queue belongs to the daemon's calloop
//! loop on the main thread. Two threads must never dispatch one queue,
//! and a capture must never be able to stall cursor events - so this is
//! a second client as far as the compositor is concerned.
//!
//! Only what events write lives in [`State`]; the request side (proxies,
//! buffers, caches) belongs to the backend, which keeps the borrow
//! split that lets `dispatch` run against `&mut State` while the loop
//! itself is borrowed.

use super::crop::Order;
use crate::cursor::outputs::OutputGeometry;
use crate::wayland::Advertised;
use anyhow::{Context, Result};
use chibipop::geom::PhysRect;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::{self, ZxdgOutputV1};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::{
    self, ZwlrScreencopyFrameV1,
};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

/// The manager global this backend is. Selection is by advertised
/// global, never by compositor identity (ADR-0002).
pub const MANAGER_GLOBAL: &str = "zwlr_screencopy_manager_v1";

/// `copy_with_damage` arrived in version 2; without it there is no
/// damage race, only plain copies.
const DAMAGE_SINCE: u32 = 2;

/// `buffer_done` arrived in version 3; before it, the `buffer` event
/// was the whole enumeration.
const BUFFER_DONE_SINCE: u32 = 3;

/// Which frame an event belongs to: the copy a `grab` is waiting on, or
/// the damage race left in flight between reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Copy,
    Watch,
}

/// How a copy ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ready,
    Failed,
}

/// The shm buffer the compositor asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    pub format: wl_shm::Format,
    pub w: i32,
    pub h: i32,
    pub stride: i32,
}

impl Shape {
    /// The byte order of these pixels, or `None` for a format core's
    /// `Frame` cannot describe.
    ///
    /// `wl_shm` names a little-endian word, so the byte order in
    /// memory is the reverse of the name: `Xrgb8888` is B, G, R, X and
    /// `Rgb888` is B, G, R packed.
    pub fn order(&self) -> Option<Order> {
        match self.format {
            wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888 => Some(Order::Bgrx),
            wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888 => Some(Order::Rgbx),
            wl_shm::Format::Rgb888 => Some(Order::Bgr),
            wl_shm::Format::Bgr888 => Some(Order::Rgb),
            _ => None,
        }
    }
}

/// One frame's events, as they arrive.
#[derive(Debug, Default)]
pub struct FrameSlot {
    /// From the `buffer` event.
    pub shape: Option<Shape>,
    /// The `buffer_done` event, or the `buffer` event on version < 3.
    pub enumerated: bool,
    /// `y_invert` seen in the `flags` event.
    pub y_invert: bool,
    pub outcome: Option<Outcome>,
}

/// The request side: the queue every new object is created on.
///
/// Split from [`State`] on purpose - `dispatch` needs `&mut State`
/// while the loop is borrowed, so nothing the request side owns may
/// live in there.
pub struct Session {
    qh: QueueHandle<State>,
}

impl Session {
    pub fn new(qh: QueueHandle<State>) -> Session {
        Session { qh }
    }

    pub fn handle(&self) -> &QueueHandle<State> {
        &self.qh
    }

    /// One `capture_output_region` frame, output-local logical.
    ///
    /// The cursor is never composited in: OCR must read the text, not
    /// the pointer sitting on it.
    pub fn capture(
        &self,
        manager: &ZwlrScreencopyManagerV1,
        output: &WlOutput,
        logical: PhysRect,
        slot: Slot,
    ) -> ZwlrScreencopyFrameV1 {
        manager.capture_output_region(
            0,
            output,
            logical.x,
            logical.y,
            logical.w,
            logical.h,
            &self.qh,
            slot,
        )
    }
}

/// One bound output.
pub struct Output {
    /// The registry name events arrive under.
    name: u32,
    pub output: WlOutput,
    pub geom: OutputGeometry,
    /// zxdg_output_v1 spoke, so wl_output.geometry stops overwriting.
    xdg_position_seen: bool,
}

/// Everything the compositor's events write.
#[derive(Default)]
pub struct State {
    pub outputs: Vec<Output>,
    pub copy: FrameSlot,
    pub watch: FrameSlot,
}

impl State {
    pub fn slot(&self, slot: Slot) -> &FrameSlot {
        match slot {
            Slot::Copy => &self.copy,
            Slot::Watch => &self.watch,
        }
    }

    pub fn slot_mut(&mut self, slot: Slot) -> &mut FrameSlot {
        match slot {
            Slot::Copy => &mut self.copy,
            Slot::Watch => &mut self.watch,
        }
    }

    /// Geometry for `geometry::split`, in bind order.
    pub fn geometries(&self, into: &mut Vec<OutputGeometry>) {
        into.clear();
        into.extend(self.outputs.iter().map(|o| o.geom));
    }
}

/// What `open` bound, before the loop takes the queue.
pub struct Bound {
    pub conn: Connection,
    pub queue: EventQueue<State>,
    pub state: State,
    pub manager: ZwlrScreencopyManagerV1,
    pub shm: WlShm,
    /// Bound manager version: what the damage race may assume.
    pub version: u32,
}

impl Bound {
    /// True once `copy_with_damage` exists.
    pub fn damage_capable(&self) -> bool {
        self.version >= DAMAGE_SINCE
    }

    /// True once `buffer_done` exists.
    pub fn sends_buffer_done(&self) -> bool {
        self.version >= BUFFER_DONE_SINCE
    }
}

/// Advertised globals enough for this backend (ADR-0002: absence is a
/// rung that does not exist, never a crash).
pub fn available(globals: &[Advertised]) -> bool {
    let has = |i: &str| globals.iter().any(|g| g.interface == i);
    has(MANAGER_GLOBAL) && has("wl_shm") && has("wl_output")
}

/// Connect, bind, and settle output geometry.
pub fn bind(globals: &[Advertised]) -> Result<Bound> {
    anyhow::ensure!(available(globals), "{MANAGER_GLOBAL}, wl_shm or wl_output is not advertised");
    let conn = Connection::connect_to_env().context("connecting the capture backend's own display")?;
    let mut queue = conn.new_event_queue::<State>();
    let qh = queue.handle();
    let registry = conn.display().get_registry(&qh, ());

    let manager_global = globals
        .iter()
        .find(|g| g.interface == MANAGER_GLOBAL)
        .context("the screencopy manager vanished between the probe and the bind")?;
    let version = manager_global.version.min(3);
    let manager =
        registry.bind::<ZwlrScreencopyManagerV1, _, State>(manager_global.name, version, &qh, ());

    let shm_global = globals.iter().find(|g| g.interface == "wl_shm").context("wl_shm vanished")?;
    let shm = registry.bind::<WlShm, _, State>(shm_global.name, 1, &qh, ());

    let xdg = globals
        .iter()
        .find(|g| g.interface == "zxdg_output_manager_v1")
        .map(|g| registry.bind::<ZxdgOutputManagerV1, _, State>(g.name, g.version.min(3), &qh, ()));

    let mut state = State::default();
    for g in globals.iter().filter(|g| g.interface == "wl_output") {
        let output = registry.bind::<WlOutput, _, State>(g.name, g.version.min(4), &qh, g.name);
        if let Some(m) = &xdg {
            m.get_xdg_output(&output, &qh, g.name);
        }
        state.outputs.push(Output {
            name: g.name,
            output,
            geom: OutputGeometry::default(),
            xdg_position_seen: false,
        });
    }

    // Two roundtrips: the first delivers wl_output, the second the
    // xdg_output logical box created inside it.
    queue.roundtrip(&mut state).context("settling output geometry")?;
    queue.roundtrip(&mut state).context("settling output geometry")?;
    Ok(Bound { conn, queue, state, manager, shm, version })
}

/// Find the output entry an event names.
fn entry(state: &mut State, name: u32) -> Option<&mut Output> {
    state.outputs.iter_mut().find(|o| o.name == name)
}

impl Dispatch<WlOutput, u32> for State {
    fn event(
        state: &mut State,
        _: &WlOutput,
        event: wl_output::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let Some(o) = entry(state, *name) else { return };
        match event {
            wl_output::Event::Geometry { x, y, transform, .. } => {
                if !o.xdg_position_seen {
                    o.geom.logical_x = x;
                    o.geom.logical_y = y;
                }
                o.geom.transform_swaps = matches!(
                    transform,
                    WEnum::Value(
                        wl_output::Transform::_90
                            | wl_output::Transform::_270
                            | wl_output::Transform::Flipped90
                            | wl_output::Transform::Flipped270
                    )
                );
            }
            wl_output::Event::Mode { flags, width, height, .. } => {
                if matches!(flags, WEnum::Value(f) if f.contains(wl_output::Mode::Current)) {
                    o.geom.mode_w = width;
                    o.geom.mode_h = height;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZxdgOutputV1, u32> for State {
    fn event(
        state: &mut State,
        _: &ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let Some(o) = entry(state, *name) else { return };
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => {
                o.geom.logical_x = x;
                o.geom.logical_y = y;
                o.xdg_position_seen = true;
            }
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                o.geom.logical_w = width;
                o.geom.logical_h = height;
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, Slot> for State {
    fn event(
        state: &mut State,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        slot: &Slot,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let s = state.slot_mut(*slot);
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer { format, width, height, stride } => {
                if let Ok(format) = format.into_result() {
                    s.shape = Some(Shape {
                        format,
                        w: width as i32,
                        h: height as i32,
                        stride: stride as i32,
                    });
                }
            }
            zwlr_screencopy_frame_v1::Event::BufferDone => s.enumerated = true,
            zwlr_screencopy_frame_v1::Event::Flags { flags } => {
                if let Ok(f) = flags.into_result() {
                    s.y_invert = f.contains(zwlr_screencopy_frame_v1::Flags::YInvert);
                }
            }
            zwlr_screencopy_frame_v1::Event::Ready { .. } => s.outcome = Some(Outcome::Ready),
            zwlr_screencopy_frame_v1::Event::Failed => s.outcome = Some(Outcome::Failed),
            // Damage boxes are per-copy detail; the race only needs to
            // know that something moved.
            _ => {}
        }
    }
}

/// Globals with no events this backend acts on.
macro_rules! ignore_events {
    ($($proxy:ty),+ $(,)?) => {
        $(impl Dispatch<$proxy, ()> for State {
            fn event(
                _: &mut State,
                _: &$proxy,
                _: <$proxy as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<State>,
            ) {
            }
        })+
    };
}

ignore_events!(
    WlRegistry,
    WlShm,
    WlShmPool,
    WlBuffer,
    ZxdgOutputManagerV1,
    ZwlrScreencopyManagerV1,
);

#[cfg(test)]
mod tests {
    use super::*;

    fn advertised(interface: &str, version: u32) -> Advertised {
        Advertised { name: 1, interface: interface.to_string(), version }
    }

    #[test]
    fn the_backend_needs_the_manager_shm_and_outputs() {
        let full = vec![
            advertised(MANAGER_GLOBAL, 3),
            advertised("wl_shm", 1),
            advertised("wl_output", 4),
        ];
        assert!(available(&full));
        for missing in [MANAGER_GLOBAL, "wl_shm", "wl_output"] {
            let short: Vec<_> =
                full.iter().filter(|g| g.interface != missing).cloned().collect();
            assert!(!available(&short), "{missing} must make the rung unavailable");
        }
    }

    #[test]
    fn a_compositor_without_screencopy_is_simply_unavailable() {
        let globals = vec![advertised("wl_shm", 1), advertised("wl_compositor", 6)];
        assert!(!available(&globals));
        // And binding says so instead of panicking.
        assert!(bind(&globals).is_err());
    }

    /// The mapping is memory order, not the format's name - and the
    /// packed pair is what wlroots on GLES2 actually offers.
    #[test]
    fn the_supported_byte_orders_are_mapped_from_memory_order() {
        let shape = |format| Shape { format, w: 1, h: 1, stride: 4 };
        assert_eq!(shape(wl_shm::Format::Xrgb8888).order(), Some(Order::Bgrx));
        assert_eq!(shape(wl_shm::Format::Argb8888).order(), Some(Order::Bgrx));
        assert_eq!(shape(wl_shm::Format::Xbgr8888).order(), Some(Order::Rgbx));
        assert_eq!(shape(wl_shm::Format::Abgr8888).order(), Some(Order::Rgbx));
        assert_eq!(shape(wl_shm::Format::Rgb888).order(), Some(Order::Bgr));
        assert_eq!(shape(wl_shm::Format::Bgr888).order(), Some(Order::Rgb));
        assert_eq!(shape(wl_shm::Format::Rgb565).order(), None);
    }
}
