//! Rung 1: the ext-image-copy-capture pointer cursor session
//! (ARCHITECTURE.md#input-ladders). Event-driven — positions arrive on
//! the daemon's existing Wayland calloop source, so a parked cursor
//! costs zero wakeups (ARCHITECTURE.md#hover-cadence).
//!
//! Only the *cursor session* is created, never the inner capture
//! session: this module wants positions, not pixels. The session's
//! `position` events are transformed buffer pixels relative to one
//! output; `outputs::OutputGeometry` lifts them to global physical.
//!
//! Outputs present at startup get sessions; hotplug is not this
//! module's concern (the registry listener in `daemon.rs` logs it).

use super::outputs::{self, OutputGeometry};
use crate::wayland::Advertised;
use chibipop::geom::PhysPoint;
use std::collections::BTreeMap;
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::ext::image_capture_source::v1::client::ext_image_capture_source_v1::ExtImageCaptureSourceV1;
use wayland_protocols::ext::image_capture_source::v1::client::ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_cursor_session_v1::{
    self, ExtImageCopyCaptureCursorSessionV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::{self, ZxdgOutputV1};

/// The daemon-side seam: `CursorState` dispatches into whatever owns
/// it (via `delegate_dispatch!`) and hands finished positions up.
pub trait CursorHandler:
    Dispatch<WlOutput, u32>
    + Dispatch<ZxdgOutputManagerV1, ()>
    + Dispatch<ZxdgOutputV1, u32>
    + Dispatch<WlSeat, ()>
    + Dispatch<WlPointer, ()>
    + Dispatch<ExtOutputImageCaptureSourceManagerV1, ()>
    + Dispatch<ExtImageCaptureSourceV1, ()>
    + Dispatch<ExtImageCopyCaptureManagerV1, ()>
    + Dispatch<ExtImageCopyCaptureCursorSessionV1, u32>
    + 'static
{
    fn cursor(&mut self) -> &mut CursorState;
    /// One global-physical-pixel sample — the channel's output.
    fn on_cursor_position(&mut self, pos: PhysPoint);
}

struct OutputEntry {
    output: WlOutput,
    geo: OutputGeometry,
    /// zxdg_output_v1 spoke; wl_output.geometry no longer overwrites.
    xdg_position_seen: bool,
    /// Kept alive for the session's lifetime.
    _source: Option<ExtImageCaptureSourceV1>,
    session: Option<ExtImageCopyCaptureCursorSessionV1>,
}

/// Everything the cursor channel binds, keyed by registry name.
#[derive(Default)]
pub struct CursorState {
    outputs: BTreeMap<u32, OutputEntry>,
    source_manager: Option<ExtOutputImageCaptureSourceManagerV1>,
    capture_manager: Option<ExtImageCopyCaptureManagerV1>,
    pointer: Option<WlPointer>,
    /// Which output's session the cursor last entered, for trace only.
    active: Option<u32>,
    /// Hyprland <= 0.55 sends `position` in output-local logical
    /// units, not the spec's buffer pixels — see
    /// `OutputGeometry::session_to_global`.
    session_positions_logical: bool,
}

impl CursorState {
    /// Bind `wl_output` (+ xdg-output for logical geometry). Both
    /// rungs need this: it is how positions become physical pixels.
    pub fn bind_outputs<D: CursorHandler>(
        &mut self,
        registry: &WlRegistry,
        globals: &[Advertised],
        qh: &QueueHandle<D>,
    ) {
        let xdg_manager = globals
            .iter()
            .find(|g| g.interface == "zxdg_output_manager_v1")
            .map(|g| registry.bind::<ZxdgOutputManagerV1, _, D>(g.name, g.version.min(3), qh, ()));
        for g in globals.iter().filter(|g| g.interface == "wl_output") {
            let output = registry.bind::<WlOutput, _, D>(g.name, g.version.min(4), qh, g.name);
            if let Some(m) = &xdg_manager {
                m.get_xdg_output(&output, qh, g.name);
            }
            self.outputs.insert(
                g.name,
                OutputEntry {
                    output,
                    geo: OutputGeometry::default(),
                    xdg_position_seen: false,
                    _source: None,
                    session: None,
                },
            );
        }
    }

    /// Bind the rung-1 capture stack. Sessions are created once the
    /// seat advertises a pointer (see the `wl_seat` dispatch below).
    pub fn bind_capture<D: CursorHandler>(
        &mut self,
        registry: &WlRegistry,
        globals: &[Advertised],
        qh: &QueueHandle<D>,
    ) {
        // The one place the Hyprland coordinate quirk is decided;
        // conversion lives in `OutputGeometry::session_to_global`.
        self.session_positions_logical = super::hyprctl::available();
        for g in globals {
            match g.interface.as_str() {
                "ext_output_image_capture_source_manager_v1" => {
                    self.source_manager = Some(registry.bind::<ExtOutputImageCaptureSourceManagerV1, _, D>(
                        g.name,
                        1,
                        qh,
                        (),
                    ));
                }
                "ext_image_copy_capture_manager_v1" => {
                    self.capture_manager =
                        Some(registry.bind::<ExtImageCopyCaptureManagerV1, _, D>(g.name, 1, qh, ()));
                }
                "wl_seat" if self.pointer.is_none() => {
                    // Bound for get_pointer only; the capabilities
                    // event below actually creates it.
                    registry.bind::<WlSeat, _, D>(g.name, g.version.min(5), qh, ());
                }
                _ => {}
            }
        }
    }

    /// A logical layout point (the hyprctl rung's space) to global
    /// physical; `None` until output geometry has arrived.
    pub fn logical_to_global(&self, x: f64, y: f64) -> Option<PhysPoint> {
        outputs::logical_to_global(self.outputs.values().map(|e| &e.geo), x, y)
    }

    /// For trace lines: how many outputs have live cursor sessions.
    pub fn session_count(&self) -> usize {
        self.outputs.values().filter(|e| e.session.is_some()).count()
    }

    /// Every output's layout facts, in registry order.
    ///
    /// The portal capture rung anchors its monitors against these
    /// (capture ladder rung 2, ARCHITECTURE.md#capture-and-masking),
    /// and both seams must use the same numbers or a hover on the
    /// second monitor lands on the first.
    pub fn geometries(&self) -> Vec<OutputGeometry> {
        self.outputs.values().map(|e| e.geo).collect()
    }
}

/// Sessions for every sessionless output, once managers + pointer
/// exist. Proxies are cloned out first so object creation never
/// aliases the `&mut D` borrow.
fn create_sessions<D: CursorHandler>(data: &mut D, qh: &QueueHandle<D>) {
    let c = data.cursor();
    let (Some(source_manager), Some(capture_manager), Some(pointer)) =
        (c.source_manager.clone(), c.capture_manager.clone(), c.pointer.clone())
    else {
        return;
    };
    let pending: Vec<(u32, WlOutput)> = c
        .outputs
        .iter()
        .filter(|(_, e)| e.session.is_none())
        .map(|(name, e)| (*name, e.output.clone()))
        .collect();
    for (name, output) in pending {
        let source = source_manager.create_source(&output, qh, ());
        let session = capture_manager.create_pointer_cursor_session(&source, &pointer, qh, name);
        let entry = data.cursor().outputs.get_mut(&name).expect("entry vanished");
        entry._source = Some(source);
        entry.session = Some(session);
    }
}

impl<D: CursorHandler> Dispatch<WlOutput, u32, D> for CursorState {
    fn event(
        data: &mut D,
        _: &WlOutput,
        event: wl_output::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<D>,
    ) {
        let Some(entry) = data.cursor().outputs.get_mut(name) else { return };
        match event {
            wl_output::Event::Geometry { x, y, transform, .. } => {
                if !entry.xdg_position_seen {
                    entry.geo.logical_x = x;
                    entry.geo.logical_y = y;
                }
                entry.geo.transform_swaps = matches!(
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
                    entry.geo.mode_w = width;
                    entry.geo.mode_h = height;
                }
            }
            _ => {}
        }
    }
}

impl<D: CursorHandler> Dispatch<ZxdgOutputV1, u32, D> for CursorState {
    fn event(
        data: &mut D,
        _: &ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<D>,
    ) {
        let Some(entry) = data.cursor().outputs.get_mut(name) else { return };
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => {
                entry.geo.logical_x = x;
                entry.geo.logical_y = y;
                entry.xdg_position_seen = true;
            }
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                entry.geo.logical_w = width;
                entry.geo.logical_h = height;
            }
            _ => {}
        }
    }
}

impl<D: CursorHandler> Dispatch<WlSeat, (), D> for CursorState {
    fn event(
        data: &mut D,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<D>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            if caps.contains(wl_seat::Capability::Pointer) && data.cursor().pointer.is_none() {
                let pointer = seat.get_pointer(qh, ());
                data.cursor().pointer = Some(pointer);
                create_sessions(data, qh);
            }
        }
    }
}

impl<D: CursorHandler> Dispatch<ExtImageCopyCaptureCursorSessionV1, u32, D> for CursorState {
    fn event(
        data: &mut D,
        _: &ExtImageCopyCaptureCursorSessionV1,
        event: ext_image_copy_capture_cursor_session_v1::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<D>,
    ) {
        match event {
            ext_image_copy_capture_cursor_session_v1::Event::Enter => {
                data.cursor().active = Some(*name);
            }
            ext_image_copy_capture_cursor_session_v1::Event::Leave => {
                let c = data.cursor();
                if c.active == Some(*name) {
                    c.active = None;
                }
            }
            // "Relative to the main buffer's top left corner in
            // transformed buffer pixel coordinates" — physical pixels
            // on this output.
            ext_image_copy_capture_cursor_session_v1::Event::Position { x, y } => {
                let c = data.cursor();
                let logical = c.session_positions_logical;
                let pos = c.outputs.get(name).map(|e| e.geo.session_to_global(x, y, logical));
                if let Some(pos) = pos {
                    data.on_cursor_position(pos);
                }
            }
            // Hotspot is cursor-image metadata; irrelevant to hover.
            _ => {}
        }
    }
}

/// Eventless (or ignored-event) helpers the channel binds.
macro_rules! ignore_events {
    ($($iface:ty),+ $(,)?) => {
        $(
            impl<D: CursorHandler> Dispatch<$iface, (), D> for CursorState {
                fn event(
                    _: &mut D,
                    _: &$iface,
                    _: <$iface as wayland_client::Proxy>::Event,
                    _: &(),
                    _: &Connection,
                    _: &QueueHandle<D>,
                ) {
                }
            }
        )+
    };
}

ignore_events!(
    WlPointer,
    ZxdgOutputManagerV1,
    ExtOutputImageCaptureSourceManagerV1,
    ExtImageCaptureSourceV1,
    ExtImageCopyCaptureManagerV1,
);

/// A throwaway `CursorHandler` that only settles output geometry.
struct Probe(CursorState);

impl CursorHandler for Probe {
    fn cursor(&mut self) -> &mut CursorState {
        &mut self.0
    }

    /// No sessions are created, so no position can arrive.
    fn on_cursor_position(&mut self, _: PhysPoint) {}
}

wayland_client::delegate_dispatch!(Probe: [WlOutput: u32] => CursorState);
wayland_client::delegate_dispatch!(Probe: [ZxdgOutputV1: u32] => CursorState);
wayland_client::delegate_dispatch!(Probe: [ZxdgOutputManagerV1: ()] => CursorState);
wayland_client::delegate_dispatch!(Probe: [WlSeat: ()] => CursorState);
wayland_client::delegate_dispatch!(Probe: [WlPointer: ()] => CursorState);
wayland_client::delegate_dispatch!(Probe: [ExtOutputImageCaptureSourceManagerV1: ()] => CursorState);
wayland_client::delegate_dispatch!(Probe: [ExtImageCaptureSourceV1: ()] => CursorState);
wayland_client::delegate_dispatch!(Probe: [ExtImageCopyCaptureManagerV1: ()] => CursorState);
wayland_client::delegate_dispatch!(Probe: [ExtImageCopyCaptureCursorSessionV1: u32] => CursorState);

impl Dispatch<WlRegistry, ()> for Probe {
    fn event(
        _: &mut Probe,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Probe>,
    ) {
    }
}

/// Settle output geometry on a connection nobody else holds.
///
/// The portal capture rung's consent is *eager*
/// (ARCHITECTURE.md#capture-and-masking): it runs before the pump
/// exists, and the channel-status row it produces has to be right the
/// first time the tray is published - which means the monitors it
/// approves must be anchorable before `App` and its queue are built.
/// Two roundtrips on a throwaway connection is a smaller price than
/// reordering the whole startup around a dialog, and it is only paid
/// on the sessions that select that rung.
///
/// An empty answer is normal, not an error: the caller degrades to an
/// unanchored stream rather than refusing to start.
pub fn probe_geometry(globals: &[Advertised]) -> Vec<OutputGeometry> {
    let Ok(conn) = Connection::connect_to_env() else {
        return Vec::new();
    };
    let mut queue = conn.new_event_queue::<Probe>();
    let qh = queue.handle();
    let registry = conn.display().get_registry(&qh, ());
    let mut probe = Probe(CursorState::default());
    probe.0.bind_outputs(&registry, globals, &qh);
    // Geometry lands across two rounds: the binds, then the events
    // those binds provoke.
    for _ in 0..2 {
        if queue.roundtrip(&mut probe).is_err() {
            break;
        }
    }
    probe.0.geometries()
}
