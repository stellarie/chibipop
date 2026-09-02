//! Rung 1 uses the ext-image-copy-capture cursor session
//! (ARCHITECTURE.md#input-ladders). The session is event-driven.
//! Positions arrive on the daemon's Wayland calloop source.
//! A parked cursor causes zero wakeups (ARCHITECTURE.md#hover-cadence).
//!
//! Create only the *cursor session*. Do not create the inner capture
//! session. This module needs positions, not pixels. A `position` event
//! normally gives transformed buffer pixels relative to one output.
//! Affected Hyprland versions can send output-local logical units instead.
//! `outputs::OutputGeometry` converts both forms to global physical pixels.
//!
//! Create sessions for outputs present at startup. Hotplug does not
//! belong to this module. The registry listener in `daemon.rs` logs it.

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

/// This daemon-side seam lets `CursorState` dispatch events to its owner
/// through `delegate_dispatch!`. The owner receives completed positions.
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
    /// Send one sample in global physical pixels. This is the channel output.
    fn on_cursor_position(&mut self, pos: PhysPoint);
}

struct OutputEntry {
    output: WlOutput,
    geo: OutputGeometry,
    /// `xdg_position_seen` is true after `zxdg_output_v1` supplies a position.
    /// This prevents later `wl_output.geometry` data from replacing that position.
    xdg_position_seen: bool,
    /// The source stays alive for the session lifetime.
    _source: Option<ExtImageCaptureSourceV1>,
    session: Option<ExtImageCopyCaptureCursorSessionV1>,
}

/// This state covers every object that the cursor channel binds.
/// The output map uses the registry name as its key.
#[derive(Default)]
pub struct CursorState {
    outputs: BTreeMap<u32, OutputEntry>,
    source_manager: Option<ExtOutputImageCaptureSourceManagerV1>,
    capture_manager: Option<ExtImageCopyCaptureManagerV1>,
    pointer: Option<WlPointer>,
    /// Store the registry name of the output that the cursor last entered.
    /// The trace uses this value only.
    active: Option<u32>,
    /// Affected Hyprland versions can send `position` in output-local logical
    /// units. The protocol defines buffer pixels, not logical units. See
    /// `OutputGeometry::session_to_global`.
    session_positions_logical: bool,
}

impl CursorState {
    /// Bind `wl_output` and `zxdg_output_v1` for logical geometry. Both
    /// cursor rungs need this data to convert positions to physical pixels.
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

    /// Bind the rung-1 capture stack. Create sessions after the seat
    /// advertises a pointer. See the `wl_seat` dispatch below.
    pub fn bind_capture<D: CursorHandler>(
        &mut self,
        registry: &WlRegistry,
        globals: &[Advertised],
        qh: &QueueHandle<D>,
    ) {
        // Decide the Hyprland coordinate quirk in this one place.
        // `OutputGeometry::session_to_global` does the conversion.
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
                    // Bind this object for get_pointer only. The capabilities
                    // event below creates the pointer.
                    registry.bind::<WlSeat, _, D>(g.name, g.version.min(5), qh, ());
                }
                _ => {}
            }
        }
    }

    /// Convert a logical layout point from the hyprctl rung to global physical pixels.
    /// Use default or fallback geometry when complete data is unavailable.
    /// Return `None` only when no output exists.
    pub fn logical_to_global(&self, x: f64, y: f64) -> Option<PhysPoint> {
        outputs::logical_to_global(self.outputs.values().map(|e| &e.geo), x, y)
    }

    /// Return the number of outputs with live cursor sessions.
    /// The daemon logs this diagnostic unconditionally.
    pub fn session_count(&self) -> usize {
        self.outputs.values().filter(|e| e.session.is_some()).count()
    }

    /// Return each output's layout facts in registry order.
    ///
    /// The portal capture rung anchors its monitors with these facts.
    /// This is capture ladder rung 2 (ARCHITECTURE.md#capture-and-masking).
    /// Both seams must use the same values. Otherwise, a hover on the
    /// second monitor can land on the first.
    pub fn geometries(&self) -> Vec<OutputGeometry> {
        self.outputs.values().map(|e| e.geo).collect()
    }
}

/// Create a session for each output without a session after all managers
/// and the pointer exist. Clone the proxies before object creation.
/// This avoids aliasing the `&mut D` borrow.
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
            // The protocol defines this position as
            // "Relative to the main buffer's top left corner in
            // transformed buffer pixel coordinates".
            // wlroots sends physical pixels, but affected Hyprland versions can
            // send output-local logical units.
            ext_image_copy_capture_cursor_session_v1::Event::Position { x, y } => {
                let c = data.cursor();
                let logical = c.session_positions_logical;
                let pos = c.outputs.get(name).map(|e| e.geo.session_to_global(x, y, logical));
                if let Some(pos) = pos {
                    data.on_cursor_position(pos);
                }
            }
            // The hotspot is cursor-image metadata. Hover does not use it.
            _ => {}
        }
    }
}

/// Bind helpers for events that the cursor channel ignores.
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

/// Use a temporary `CursorHandler` to settle output geometry.
struct Probe(CursorState);

impl CursorHandler for Probe {
    fn cursor(&mut self) -> &mut CursorState {
        &mut self.0
    }

    /// This probe creates no sessions. Therefore, no position can arrive.
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

/// Settle output geometry on a connection that no other component holds.
///
/// The portal capture rung requests consent *eagerly*
/// (ARCHITECTURE.md#capture-and-masking). It runs before the pump
/// exists. The tray must show the correct channel-status row on its first
/// publish. The approved monitors therefore need anchors before `App`
/// and its queue exist.
///
/// Two roundtrips on a temporary connection cost less than a startup
/// reorder around a dialog. This cost applies only to the portal capture rung.
///
/// An empty result is normal, not an error. The caller uses an
/// unanchored stream instead of refusing to start.
pub fn probe_geometry(globals: &[Advertised]) -> Vec<OutputGeometry> {
    let Ok(conn) = Connection::connect_to_env() else {
        return Vec::new();
    };
    let mut queue = conn.new_event_queue::<Probe>();
    let qh = queue.handle();
    let registry = conn.display().get_registry(&qh, ());
    let mut probe = Probe(CursorState::default());
    probe.0.bind_outputs(&registry, globals, &qh);
    // Geometry arrives in two rounds. The first round processes the binds.
    // The second round processes the events that the binds cause.
    for _ in 0..2 {
        if queue.roundtrip(&mut probe).is_err() {
            break;
        }
    }
    probe.0.geometries()
}
