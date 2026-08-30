//! The layer surfaces (ADR-0004): one per output, all mapped at
//! startup, hidden by a transparent buffer and never unmapped.
//!
//! Why one per output: a layer surface's output is fixed at creation
//! and its margins are relative to that output, so `output = NULL`
//! would hand us an origin we did not choose while the cursor roams.
//! Why never unmapped: Hyprland animates layer surfaces by default, so
//! a map per lookup would visibly fly the popup in on every hover - a
//! regression against Windows, where show and hide are instant. Making
//! that the default beats telling users to add a `layerrule no_anim`.
//!
//! Pointer input is the sibling module ([`super::pointer`]): the input
//! region committed here is what makes the panel reachable at all, and
//! the hit targets a click resolves against are taken from every frame
//! this file paints.

use super::place::{self, Placement, Screen, Shown, Visibility};
use super::pointer::{self, HitScene, InputRegion, Interaction, Pointer, Step};
use super::{paint, physical_theme, text::TextEngine};
use crate::cursor::outputs::OutputGeometry;
use crate::daemon::App;
use anyhow::{anyhow, Context, Result};
use chibipop::config::{Config, PopupLayer};
use chibipop::geom::PhysRect;
use chibipop::present::{AnkiPopupState, Presentation};
use chibipop::ui::layout::{self, PopupScene, SceneRequest};
use chibipop::ui::theme::Theme;
use smithay_client_toolkit::compositor::{
    CompositorHandler, CompositorState, FrameCallbackData, Region, SurfaceData,
};
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::output::{OutputData, OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use smithay_client_toolkit::seat::pointer::AxisScroll;
use smithay_client_toolkit::seat::SeatState;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure, LayerSurfaceData,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{globals::GlobalData, registry_handlers};
use std::sync::Arc;
use std::time::{Duration, Instant};
use wayland_client::globals::{GlobalList, GlobalListContents};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_callback::WlCallback;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_shm::{Format, WlShm};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::ZxdgOutputV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{
    self, WpFractionalScaleV1,
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::ZwlrLayerSurfaceV1;

/// The layer surface's namespace, which is what `hyprctl layers` and
/// `layerrule` see.
const NAMESPACE: &str = "chibipop";

/// How long a commit waits for the frame callback that should have
/// paced it before painting anyway.
///
/// Frame gating bounds work by the refresh rate instead of the cursor
/// event rate (ADR-0004), but callbacks stop arriving entirely for an
/// occluded surface - a fullscreen client above an `overlay` popup that
/// is currently hidden - and a gate that waits forever would wedge the
/// popup shut. Six frames at 60 Hz: long enough that a real cadence is
/// never broken, short enough that no user sees the popup stall.
const FRAME_GRACE: Duration = Duration::from_millis(100);

/// One popup to show.
///
/// The bin measures, so the Controller hands over the view model and
/// gets a rect back (`Event::PopupPlaced`); everything measured lives
/// on this side of the seam.
#[derive(Debug, Clone)]
pub struct ShowRequest {
    pub presentation: Presentation,
    pub anchor: PhysRect,
    /// Physical pixels, as the Controller counts them.
    pub scroll: i32,
    pub show_back: bool,
    /// `Some` reserves the Anki slot; Linux paints it in the panel.
    pub anki: Option<AnkiPopupState>,
}

/// What one show landed on: exactly `Event::PopupPlaced`'s payload,
/// plus what the log wants.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    pub rect: PhysRect,
    pub content_h: i32,
    pub view_h: i32,
    /// The scale it was rastered at.
    pub scale: f64,
    pub output: usize,
}

/// A frame that is waiting for the compositor.
///
/// Commits coalesce to one pending state (ADR-0004): work is bounded by
/// the refresh rate rather than the cursor-event rate, at the cost of
/// up to one frame of positional latency. Show and hide are *not*
/// gated - a hidden or occluded surface receives no frame callbacks, so
/// gating them would deadlock the popup shut.
struct Pending {
    placement: Placement,
    scale: f64,
    scene: PopupScene,
    theme: Theme,
    scroll: f32,
}

/// One output's surface.
struct Panel {
    /// Stable across removals, unlike a position in `panels`: the
    /// `wp_fractional_scale_v1` object created with a surface carries
    /// this in its user data for the surface's whole life, and an
    /// unplugged monitor must not silently re-point it at a neighbour.
    id: usize,
    output: WlOutput,
    /// The output's layout facts, in the cursor channel's convention.
    geo: OutputGeometry,
    layer: LayerSurface,
    /// `wp_viewport`: buffer pixels in, logical size out.
    viewport: Option<WpViewport>,
    /// Kept alive so `preferred_scale` keeps arriving.
    _scale: Option<WpFractionalScaleV1>,
    /// Last `preferred_scale`, in 120ths. Never latched: the value is
    /// re-read on every render.
    preferred: Option<u32>,
    /// The logical size the compositor last configured.
    configured: Option<(i32, i32)>,
    /// When the outstanding `wl_surface.frame` callback was asked for,
    /// or `None` when none is.
    awaiting_frame: Option<Instant>,
    /// The one coalesced pending frame.
    pending: Option<Pending>,
}

/// Everything the popup owns.
pub struct Popup {
    /// The queue every popup object was created on. Stored so the
    /// popup's own API needs no `QueueHandle` argument: the daemon
    /// reaches it from a control-socket callback and from a cursor
    /// sample, neither of which is a Wayland dispatch.
    qh: QueueHandle<App>,
    compositor: CompositorState,
    outputs: OutputState,
    registry: RegistryState,
    shm: Shm,
    /// `None` on a compositor that advertises no `zwlr_layer_shell_v1`
    /// (stock GNOME): the popup then maps no surface and every show is
    /// a named error, while the rest of it keeps answering SCTK.
    shell: Option<LayerShell>,
    fractional: Option<WpFractionalScaleManagerV1>,
    viewporter: Option<WpViewporter>,
    pool: SlotPool,
    panels: Vec<Panel>,
    /// Hands out `Panel::id`; never reused.
    next_id: usize,
    text: TextEngine,
    /// The painter's decoded-asset cache, or `None` when this build cannot
    /// open the dictionary database.
    ///
    /// Its own `MediaStore` connection, on this thread, because the worker
    /// owns the dictionary on another and a `rusqlite::Connection` is not
    /// `Sync`. `None` is a real state and not a failure: the popup still
    /// paints, with every image on the `alt`-text rung of its ladder.
    media: Option<chibipop_linux::media::MediaSurfaces>,
    /// The logical theme; every render scales a copy of it.
    theme: Theme,
    layer: Layer,
    /// Width and height caps, percent of the output.
    caps: (u8, u8),
    side_panel: bool,
    /// How much of an entry the panel draws, re-read on every reload
    /// (`reconfigure`) so a settings change lands on the popup already
    /// on screen.
    render: chibipop::ui::layout::RenderSettings,
    vis: Visibility,
    /// What is on screen, kept for a re-render at a new scale.
    current: Option<ShowRequest>,
    /// The seat's pointer and where it is (ADR-0003): the popup's whole
    /// input story, since Wayland has no machine-wide mouse hook.
    pointer: Pointer,
    /// The frame on screen, as the pointer resolves against it. Taken
    /// from every repaint and dropped by every hide, so a hit can never
    /// answer with geometry that is no longer painted.
    hits: Option<HitScene>,
    notes: Vec<String>,
}

impl Popup {
    /// Bind the popup's globals. Surfaces come later, once the output
    /// roundtrip has geometry to place them against.
    ///
    /// The layer shell is the one global whose absence is a *state*
    /// rather than an error (ticket 49): stock GNOME advertises none,
    /// and the popup then exists with no shell to draw on - it maps
    /// nothing, shows nothing, and says so - while the seat, the
    /// outputs and the shm pool it also owns keep serving the handlers
    /// SCTK dispatches into `App`. Returning `Err` here instead would
    /// leave those live proxies behind with nothing to answer their
    /// events, which is exactly how a layer-shell-less session used to
    /// panic on its first `wl_shm::format`.
    ///
    /// An `Err` from this function therefore means something no session
    /// can work without (no `wl_compositor`, no `wl_shm`, no shared
    /// memory), which the capability report has already named as fatal.
    pub fn bind(
        globals: &GlobalList,
        qh: &QueueHandle<App>,
        config: &Config,
        db: &std::path::Path,
    ) -> Result<Popup> {
        let compositor = CompositorState::bind(globals, qh)
            .map_err(|e| anyhow!("binding wl_compositor: {e}"))?;
        let shm = Shm::bind(globals, qh).map_err(|e| anyhow!("binding wl_shm: {e}"))?;
        // A screenful at 4K is 33 MB; the pool grows on demand, so
        // start at one modest popup and let it stretch.
        let pool = SlotPool::new(640 * 480 * 4, &shm).context("creating the popup's shm pool")?;

        let mut notes = Vec::new();
        let shell = match LayerShell::bind(globals, qh) {
            Ok(shell) => Some(shell),
            Err(e) => {
                notes.push(format!(
                    "popup: no layer surface - {e}; the hover loop is unsupported on this \
                     compositor and every other channel keeps running"
                ));
                None
            }
        };
        let fractional = globals.bind::<WpFractionalScaleManagerV1, App, _>(qh, 1..=1, ()).ok();
        let viewporter = globals.bind::<WpViewporter, App, _>(qh, 1..=1, ()).ok();
        if fractional.is_none() || viewporter.is_none() {
            notes.push(
                "popup: wp_fractional_scale_v1/wp_viewporter missing - falling back to the \
                 output's integer scale; a fractional-scale desktop will look soft"
                    .to_string(),
            );
        }

        // The font: resolved against the engine's own db, so a config
        // carried over from Windows (`Yu Gothic UI`) degrades to the
        // Linux default *visibly* instead of silently drawing tofu.
        let mut theme = theme_from_config(config);
        let mut text = TextEngine::new(&theme.font_name);
        let choice = chibipop::config::resolve_font(
            &config.popup.font,
            chibipop::config::Platform::Linux,
            |f| text.resolvable(f),
        );
        if let chibipop::config::FontChoice::Fallback { requested, family } = &choice {
            notes.push(format!("font: {requested:?} is not installed - falling back to {family}"));
        }
        theme.font_name = choice.family().to_string();
        text.set_family(&theme.font_name);
        notes.push(format!("font: painting with {} (cosmic-text locale ja)", text.family()));
        if let Some(warning) = super::text::warning(&text.probe()) {
            notes.push(warning);
        }

        // The pointer (ADR-0003). The seat is bound now and the pointer
        // itself arrives with the seat's `capabilities` event, which is
        // also where a pointer plugged in later shows up. No
        // `wp_cursor_shape_v1` means the cursor is simply left as it
        // was over the panel: loading XCursor themes ourselves is
        // exactly what ADR-0004 refused.
        let shapes = CursorShapeManager::bind(globals, qh).ok();
        if shapes.is_none() {
            notes.push(
                "popup: wp_cursor_shape_v1 missing - the cursor keeps its shape over the panel"
                    .to_string(),
            );
        }
        let mut pointer = Pointer::new(SeatState::new(globals, qh), shapes, config.popup.scroll_popup);
        let (script, script_notes) = pointer::script_from_env();
        pointer.set_script(script);
        notes.extend(script_notes);

        // The painter's own media store. A database this build cannot
        // open is one diagnostic and no images, never a popup that will
        // not draw: an image node then renders its `alt` text, which is
        // the character it stood for.
        let media = match chibipop_linux::media::MediaSurfaces::open(db) {
            Ok(cache) => Some(cache),
            Err(e) => {
                notes.push(format!(
                    "popup: no dictionary media - {e:#}; image nodes will render their \
                     alt text"
                ));
                None
            }
        };

        Ok(Popup {
            qh: qh.clone(),
            compositor,
            outputs: OutputState::new(globals, qh),
            registry: RegistryState::new(globals),
            shm,
            shell,
            fractional,
            viewporter,
            pool,
            panels: Vec::new(),
            next_id: 0,
            text,
            media,
            theme,
            layer: layer_of(config.popup_layer()),
            caps: (config.popup.max_width_percent, config.popup.max_height_percent),
            side_panel: config.popup.side_panel,
            render: config.popup.render_settings(),
            vis: Visibility::Hidden,
            current: None,
            pointer,
            hits: None,
            notes,
        })
    }

    /// Can this popup ever draw? False on a compositor with no layer
    /// shell, which is a supported state and not an error: the daemon
    /// reports the Popup channel down and keeps every other channel.
    pub fn available(&self) -> bool {
        self.shell.is_some()
    }

    /// Diagnostics accumulated since the last drain. The popup owns no
    /// log: the daemon thread does.
    pub fn drain_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// One diagnostic, for the handlers that live in the sibling
    /// module. The popup owns no log; the daemon drains these.
    pub fn note(&mut self, line: String) {
        self.notes.push(line);
    }

    pub fn surface_count(&self) -> usize {
        self.panels.len()
    }

    /// The outputs, as the surfaces beside the popup need them.
    ///
    /// One `OutputState` serves the whole daemon (`registry_handlers!`
    /// names exactly one), so the selector and the outline read the
    /// monitor list here instead of binding a second one. They get the
    /// popup's own surface ids with it, which is what makes "surface 1"
    /// mean one monitor across every diagnostic this daemon writes.
    pub fn screens(&self) -> Vec<Screen> {
        self.panels
            .iter()
            .map(|p| Screen {
                id: p.id,
                output: p.output.clone(),
                rect: place::output_physical(&p.geo),
                scale: place::fractional(p.preferred, &p.geo),
                name: self
                    .outputs
                    .info(&p.output)
                    .and_then(|i| i.name)
                    .unwrap_or_else(|| format!("output-{}", p.id)),
            })
            .collect()
    }

    /// The one `wl_shm` and the one `wl_compositor` this process bound.
    ///
    /// Shared with the selector and the outline so neither binds a
    /// second: a `wl_shm`'s format list is per-process knowledge, and a
    /// `wl_compositor` is a factory handle worth having exactly one of.
    /// A `SlotPool` is *not* shared - it is an mmap, and one per
    /// surface set is the point.
    pub fn shm(&self) -> &Shm {
        &self.shm
    }

    pub fn compositor(&self) -> &CompositorState {
        &self.compositor
    }

    /// The one `wp_viewporter`, and no fractional-scale object beside
    /// it: the popup already has a surface on every output, so its
    /// `preferred_scale` is this daemon's single source of scale truth
    /// and the other two surfaces read it back through
    /// [`Popup::screens`]. Nothing latches it there either.
    pub fn viewporter(&self) -> Option<&WpViewporter> {
        self.viewporter.as_ref()
    }

    /// The logical theme this popup resolved from the config, and
    /// re-resolves on every reload.
    ///
    /// The scan outline reads its four `scan_*` colours off it rather
    /// than resolving a second theme of its own: the outline is drawn
    /// beside the popup, for the same lookup, and there is exactly one
    /// answer to "what does this session look like". Lengths here are
    /// logical - the outline's are physical constants, so it never
    /// wants the scaled copy [`physical_theme`] makes for a render.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Does one `wl_surface` belong to the popup?
    ///
    /// The routing question every shared SCTK handler now asks: three
    /// kinds of layer surface answer into one `App`, so a `configure` or
    /// a `frame` has to be sorted by identity rather than assumed to be
    /// the popup's.
    pub fn owns(&self, surface: &WlSurface) -> bool {
        self.panels.iter().any(|p| p.layer.wl_surface() == surface)
    }

    /// The same question for a `closed`/`configure`, which name a layer
    /// surface rather than a `wl_surface`.
    pub fn owns_layer(&self, layer: &LayerSurface) -> bool {
        self.panels.iter().any(|p| &p.layer == layer)
    }

    pub fn shown(&self) -> Option<Shown> {
        self.vis.shown()
    }

    /// The request on screen, for a repaint at new settings.
    pub fn request(&self) -> Option<&ShowRequest> {
        self.current.as_ref()
    }

    // ---- pointer input (ADR-0003) ----

    /// The seat, for SCTK's own `wl_seat` dispatch.
    pub fn seats(&mut self) -> &mut SeatState {
        self.pointer.seats()
    }

    /// The seat grew a pointer: take it, and a cursor-shape device to
    /// go with it. Both are dropped again by [`Popup::pointer_gone`].
    pub fn pointer_arrived(&mut self, pointer: WlPointer) {
        self.pointer.set_pointer(Some(pointer));
        let device = self
            .pointer
            .shape_source()
            .map(|(shapes, pointer)| shapes.get_shape_device(pointer, &self.qh));
        if let Some(device) = device {
            self.pointer.attach_shape_device(device);
        }
        self.notes.push(format!(
            "pointer: seat pointer bound (cursor shape {})",
            if self.pointer.shapes_advertised() { "wp_cursor_shape_v1" } else { "left alone" }
        ));
    }

    /// The seat lost its pointer capability.
    pub fn pointer_gone(&mut self) {
        self.pointer.set_pointer(None);
        self.notes.push("pointer: the seat no longer has a pointer".to_string());
    }

    /// Which surface a pointer event names, if it is one of ours.
    pub fn panel_of(&self, surface: &WlSurface) -> Option<usize> {
        self.panels.iter().find(|p| p.layer.wl_surface() == surface).map(|p| p.id)
    }

    /// The pointer crossed onto a panel.
    ///
    /// Reachable only while the popup is shown: a hidden surface's
    /// input region is empty, so the compositor never sends this - one
    /// crossing logged is the region working. `serial` is `None` for a
    /// scripted pass, which has no enter event to quote.
    pub fn pointer_enter(&mut self, panel: usize, pos: (f64, f64), serial: Option<u32>) {
        self.pointer.enter(panel, pos, serial);
        self.notes.push(format!(
            "pointer: entered surface {panel} at {:.1},{:.1} logical -> {}",
            pos.0,
            pos.1,
            self.hit_at(pos)
        ));
        self.hover();
    }

    pub fn pointer_leave(&mut self, panel: usize) {
        if self.pointer.focus().is_some_and(|f| f.panel == panel) {
            self.notes.push(format!("pointer: left surface {panel}"));
        }
        self.pointer.leave(panel);
    }

    pub fn pointer_motion(&mut self, pos: (f64, f64)) {
        self.pointer.motion(pos);
        self.hover();
    }

    /// A left press on the panel, resolved against the frame on screen.
    pub fn pointer_button(&mut self, pos: (f64, f64)) -> Option<Interaction> {
        self.pointer.motion(pos);
        let hits = self.hits.as_ref()?;
        if self.pointer.focus()?.panel != hits.panel {
            return None;
        }
        Some(pointer::click(hits, pos))
    }

    /// A wheel frame over the panel.
    pub fn pointer_wheel(&mut self, axis: &AxisScroll) -> Option<Interaction> {
        self.pointer.wheel(axis)
    }

    /// The same wheel path, from a raw `axis_value120`: what a scripted
    /// pass has, since it holds no `wl_pointer` frame.
    pub fn pointer_wheel_120(&mut self, value120: i32) -> Option<Interaction> {
        self.pointer_wheel(&AxisScroll { value120, ..AxisScroll::default() })
    }

    /// Drop the banked sub-notch delta: `Command::DiscardScroll`, which
    /// the Controller sends when a fresh popup replaces the old one.
    pub fn discard_scroll(&mut self) {
        self.pointer.discard_scroll();
    }

    /// The hand over anything clickable, the default cursor elsewhere.
    fn hover(&mut self) {
        let over = match (self.hits.as_ref(), self.pointer.focus()) {
            (Some(hits), Some(focus)) if focus.panel == hits.panel => {
                hits.hit(hits.local(focus.pos)).is_some()
            }
            _ => false,
        };
        self.pointer.set_shape(over);
    }

    /// A fresh frame is coming: arm the next scripted pass.
    pub fn arm_script(&mut self) {
        self.pointer.arm_script();
    }

    /// The next scripted pass (`CHIBIPOP_POINTER_SCRIPT`), if one is
    /// armed and a frame is actually up.
    ///
    /// The daemon drives the steps, not this module: each step's effect
    /// has to reach the Controller and come back as a repaint before
    /// the next step resolves, or a scripted scroll would be followed
    /// by a click against the frame it just replaced.
    pub fn take_pass(&mut self) -> Option<Vec<Step>> {
        if self.hits.is_none() || self.vis.shown().is_none() {
            return None;
        }
        self.pointer.take_pass()
    }

    /// What sits at one surface-local logical point, for the log.
    pub fn hit_at(&self, pos: (f64, f64)) -> String {
        match self.hits.as_ref() {
            Some(hits) => match hits.hit(hits.local(pos)) {
                Some(hit) => format!("{hit:?} at panel px {:?}", hits.local(pos)),
                None => format!("nothing at panel px {:?}", hits.local(pos)),
            },
            None => "nothing is shown".to_string(),
        }
    }

    /// The frame's hit targets, in the logical coordinates a pointer
    /// arrives in: what a scripted pass needs to aim at anything.
    pub fn dump_hits(&mut self) {
        let Some(hits) = self.hits.clone() else {
            self.notes.push("pointer: no frame to dump".to_string());
            return;
        };
        self.notes.push(format!(
            "pointer: frame on surface {} at {:.3}x, scroll {} px, view {} px, {} target(s)",
            hits.panel,
            hits.scale,
            hits.scroll,
            hits.view_h,
            hits.targets.len(),
        ));
        for target in &hits.targets {
            let top = (f64::from(target.y - hits.scroll) / hits.scale).round();
            let bottom = (f64::from(target.y + target.h - hits.scroll) / hits.scale).round();
            let across = match (target.x, target.w) {
                (Some(x), Some(w)) => format!(
                    "x {:.0}..{:.0}",
                    f64::from(x) / hits.scale,
                    f64::from(x + w) / hits.scale
                ),
                _ => "x full".to_string(),
            };
            self.notes.push(format!(
                "pointer:   {:?} logical y {top:.0}..{bottom:.0}, {across}",
                target.action
            ));
        }
        if let Some(strip) = hits.anki {
            self.notes.push(format!(
                "pointer:   Anki logical y {:.0}..{:.0}, x full",
                f64::from(strip.y) / hits.scale,
                f64::from(strip.y + strip.h) / hits.scale,
            ));
        }
    }

    /// Map one hidden surface per known output. Every surface is mapped
    /// now and stays mapped; only the active one ever holds a
    /// popup-sized buffer.
    pub fn map_all(&mut self) {
        let outputs: Vec<WlOutput> = self.outputs.outputs().collect();
        for output in outputs {
            self.map_one(&output);
        }
    }

    /// Map the surface for one output, if it has none yet.
    ///
    /// Idempotent on purpose: SCTK announces an output twice over a
    /// startup roundtrip - once from the initial global list, once when
    /// `new_output` fires as its xdg-output info completes - and two
    /// surfaces on one output would show two popups.
    pub fn map_one(&mut self, output: &WlOutput) {
        if self.panels.iter().any(|p| &p.output == output) {
            return;
        }
        let qh = self.qh.clone();
        let qh = &qh;
        let Some(info) = self.outputs.info(output) else { return };
        let geo = place::geometry_of(&info);
        let id = self.next_id;

        // No shell, no surface: nothing is created and nothing is
        // spent. The daemon has already said so once at startup, so
        // this is silent rather than one note per output.
        let Some(shell) = self.shell.as_ref() else { return };
        let surface = self.compositor.create_surface(qh);
        let viewport = self.viewporter.as_ref().map(|v| v.get_viewport(&surface, qh, ()));
        let scale = self.fractional.as_ref().map(|f| f.get_fractional_scale(&surface, qh, id));
        let layer = shell.create_layer_surface(qh, surface, self.layer, Some(NAMESPACE), Some(output));
        self.next_id += 1;
        // Inviolable (ADR-0004): the popup must never reserve space,
        // never take keyboard focus, and be positioned by margins from
        // the top-left corner of *this* output.
        layer.set_anchor(Anchor::TOP | Anchor::LEFT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_margin(0, 0, 0, 0);
        layer.set_size(1, 1);
        // The initial commit carries no buffer: the compositor answers
        // with a configure, and the hidden 1x1 buffer follows.
        layer.commit();

        let name = info.name.clone().unwrap_or_else(|| format!("output-{}", info.id));
        self.notes.push(format!(
            "popup: layer surface {id} on {name} ({}x{} at {:.2}x, {} layer, keyboard none)",
            geo.physical_w(),
            place::output_physical(&geo).h,
            geo.scale(),
            layer_name(self.layer),
        ));
        self.panels.push(Panel {
            id,
            output: output.clone(),
            geo,
            layer,
            viewport,
            _scale: scale,
            preferred: None,
            configured: None,
            awaiting_frame: None,
            pending: None,
        });
    }

    /// An output went away, or its surface was closed by the
    /// compositor: routine recreation, not an error (ADR-0004).
    pub fn drop_output(&mut self, output: &WlOutput) {
        let Some(slot) = self.panels.iter().position(|p| &p.output == output) else { return };
        let id = self.panels[slot].id;
        if self.vis.shown().is_some_and(|s| s.output == id) {
            self.vis = Visibility::Hidden;
            self.current = None;
        }
        self.panels.remove(slot);
        self.notes.push(format!("popup: layer surface {id} closed; {} left", self.panels.len()));
    }

    /// A `closed` event names a surface, not an output.
    pub fn drop_layer(&mut self, layer: &LayerSurface) {
        let Some(slot) = self.panels.iter().position(|p| &p.layer == layer) else { return };
        let output = self.panels[slot].output.clone();
        self.drop_output(&output);
        // The output is still there, so the surface comes back: this is
        // how a compositor asks for recreation.
        if self.outputs.info(&output).is_some() {
            self.map_one(&output);
        }
    }

    /// Re-read the settings a reload can change. `set_layer` needs no
    /// recreation, which is exactly why `popup.layer` is a runtime
    /// toggle and not a restart.
    pub fn reconfigure(&mut self, config: &Config) {
        let layer = layer_of(config.popup_layer());
        if layer != self.layer {
            self.layer = layer;
            for panel in &self.panels {
                panel.layer.set_layer(layer);
                panel.layer.commit();
            }
            self.notes.push(format!("popup: layer -> {}", layer_name(layer)));
        }
        self.caps = (config.popup.max_width_percent, config.popup.max_height_percent);
        self.side_panel = config.popup.side_panel;
        self.render = config.popup.render_settings();
        // Windows arms its wheel hook per dispatch tick from the same
        // setting; the popup's own region has nothing to arm, so the
        // gate lives on the pointer.
        self.pointer.set_wheel_enabled(config.popup.scroll_popup);

        let theme = theme_from_config(config);
        let refont = theme.font_name != self.theme.font_name;
        self.theme = theme;
        if refont {
            let choice = chibipop::config::resolve_font(
                &config.popup.font,
                chibipop::config::Platform::Linux,
                |f| self.text.resolvable(f),
            );
            if let chibipop::config::FontChoice::Fallback { requested, family } = &choice {
                self.notes
                    .push(format!("font: {requested:?} is not installed - falling back to {family}"));
            }
            self.theme.font_name = choice.family().to_string();
            self.text.set_family(&self.theme.font_name);
            if let Some(warning) = super::text::warning(&self.text.probe()) {
                self.notes.push(warning);
            }
        }
        // A shown popup is re-measured at the new settings.
        if self.vis.shown().is_some() {
            if let Some(req) = self.current.clone() {
                let _ = self.show(&req);
            }
        }
    }

    /// Measure, place, raster, commit. The returned [`Placed`] is what
    /// the daemon feeds back as `Event::PopupPlaced`.
    pub fn show(&mut self, req: &ShowRequest) -> Result<Placed> {
        if self.panels.is_empty() {
            return Err(anyhow!("no layer surface exists yet"));
        }
        let id = place::output_at(self.panels.iter().map(|p| (p.id, &p.geo)), req.anchor)
            .ok_or_else(|| anyhow!("no output geometry to place against"))?;
        let slot = self.slot(id).ok_or_else(|| anyhow!("output {id} has no surface"))?;

        let scale = place::fractional(self.panels[slot].preferred, &self.panels[slot].geo);
        let monitor = place::output_physical(&self.panels[slot].geo);
        let theme = physical_theme(&self.theme, scale);
        let max_w = (i64::from(monitor.w) * i64::from(self.caps.0) / 100).max(1) as i32;
        let max_h = (i64::from(monitor.h) * i64::from(self.caps.1) / 100).max(1) as i32;

        // Whether the Anki slot is reserved is the caller's call: the
        // Controller knows whether AnkiConnect answered, the surface
        // does not.
        let anki = req.anki.as_ref();
        let scene = layout::scene(
            &SceneRequest {
                presentation: &req.presentation,
                theme: &theme,
                max_w: max_w as f32,
                max_h: max_h as f32,
                show_back: req.show_back,
                side_panel: self.side_panel,
                render: self.render,
                anki,
            },
            &mut self.text,
        )
        .with_context(|| "laying out the popup".to_string())?;

        // Width: the panel's own when a side column widened it,
        // otherwise exactly the box it was offered - the same rule the
        // Windows bin follows, so the two platforms wrap identically.
        let w = scene.panel_w.map_or(max_w, |w| w.ceil() as i32).max(1);
        let h = paint::surface_height(&scene).ceil().max(1.0) as i32;
        let placement = place::place(req.anchor, (w, h), monitor, scale);

        let content_h = scene.content_h.ceil() as i32;
        let view_h = scene.view_h.ceil() as i32;

        if let Some(stale) = self.vis.show(Shown { output: id, placement, scale }) {
            if let Some(stale) = self.slot(stale) {
                self.clear(stale);
            }
        }
        self.current = Some(req.clone());

        let pending = Pending {
            placement,
            scale,
            scene,
            theme,
            scroll: req.scroll as f32,
        };
        self.commit_show(slot, pending)?;

        Ok(Placed { rect: placement.rect, content_h, view_h, scale, output: id })
    }

    /// Re-render what is on screen. `Ok(None)` means nothing was up.
    pub fn reshow(&mut self) -> Result<Option<Placed>> {
        let Some(req) = self.current.clone() else { return Ok(None) };
        if self.vis.shown().is_none() {
            return Ok(None);
        }
        self.show(&req).map(Some)
    }

    /// Hide: a transparent buffer and an empty input region, never an
    /// unmap. Cheap and instant, and a hide while hidden costs nothing.
    ///
    /// The pointer loses its frame here too - an empty input region
    /// means no `leave` may ever arrive, so the popup forgets what was
    /// under the cursor rather than waiting to be told.
    pub fn hide(&mut self) {
        if let Some(slot) = self.vis.hide().and_then(|id| self.slot(id)) {
            self.clear(slot);
        }
        self.current = None;
    }

    /// A `preferred_scale`: record it, and re-render when it is the
    /// surface currently showing (the scale is never latched, so this
    /// is the only place a 1.0-then-1.5 compositor is corrected).
    pub fn preferred_scale(&mut self, id: usize, scale_120ths: u32) -> bool {
        let Some(slot) = self.slot(id) else { return false };
        let panel = &mut self.panels[slot];
        panel.preferred = Some(scale_120ths);
        let scale = place::fractional(Some(scale_120ths), &panel.geo);
        self.vis.rescale(id, scale)
    }

    /// Where the surface with this stable id sits in `panels`.
    fn slot(&self, id: usize) -> Option<usize> {
        self.panels.iter().position(|p| p.id == id)
    }

    /// A configure landed: draw the frame it was asked for.
    pub fn configured(&mut self, layer: &LayerSurface, size: (u32, u32)) {
        let Some(idx) = self.panels.iter().position(|p| &p.layer == layer) else { return };
        let logical = (size.0 as i32, size.1 as i32);
        self.panels[idx].configured = Some(logical);
        if let Some(pending) = self.panels[idx].pending.take() {
            if let Err(e) = self.draw(idx, pending) {
                self.notes.push(format!("popup: painting after configure failed: {e:#}"));
            }
        } else if self.panels[idx].configured == Some((1, 1)) {
            // The startup configure: map the surface hidden.
            self.hidden_frame(idx, (1, 1));
        }
    }

    /// The compositor consumed our last frame: commit the coalesced one.
    pub fn frame_done(&mut self, surface: &WlSurface) {
        let Some(idx) = self.panels.iter().position(|p| p.layer.wl_surface() == surface) else {
            return;
        };
        self.panels[idx].awaiting_frame = None;
        if let Some(pending) = self.panels[idx].pending.take() {
            if let Err(e) = self.draw(idx, pending) {
                self.notes.push(format!("popup: painting a coalesced frame failed: {e:#}"));
            }
        }
    }

    /// A show either resizes - which costs a configure round-trip, once
    /// per new entry and never per cursor move - or draws straight away.
    /// The intermediate commit attaches no buffer, so there is no
    /// visible jump.
    fn commit_show(&mut self, idx: usize, pending: Pending) -> Result<()> {
        let panel = &mut self.panels[idx];
        let logical = pending.placement.logical;
        let (top, left) = pending.placement.margin;
        panel.layer.set_margin(top, 0, 0, left);

        if panel.configured == Some(logical) {
            return self.draw(idx, pending);
        }
        panel.layer.set_size(logical.0.max(1) as u32, logical.1.max(1) as u32);
        panel.layer.commit();
        panel.pending = Some(pending);
        Ok(())
    }

    /// Raster one frame and commit it.
    fn draw(&mut self, idx: usize, pending: Pending) -> Result<()> {
        let panel = &mut self.panels[idx];
        if panel.awaiting_frame.is_some_and(|since| since.elapsed() < FRAME_GRACE) {
            // Coalesce: one pending state, whatever arrived last.
            panel.pending = Some(pending);
            return Ok(());
        }
        let (bw, bh) = pending.placement.buffer;
        let surface = panel.layer.wl_surface().clone();
        let (buffer, canvas) = self
            .pool
            .create_buffer(bw, bh, bw * 4, Format::Argb8888)
            .context("allocating the popup's buffer")?;
        {
            let mut pixmap = tiny_skia::PixmapMut::from_bytes(canvas, bw as u32, bh as u32)
                .ok_or_else(|| anyhow!("a {bw}x{bh} buffer is not a pixmap"))?;
            paint::panel(
                &paint::Panel {
                    scene: &pending.scene,
                    theme: &pending.theme,
                    scroll: pending.scroll,
                    scale: pending.scale as f32,
                },
                &mut self.text,
                self.media.as_mut(),
                &mut pixmap,
            );
            to_argb(pixmap.data_mut());
        }

        // The targets a click resolves against come from the frame
        // being painted, never from a remembered one: that is what
        // keeps a hit honest across a scroll (the offset moved) and a
        // scale change (every rect did).
        let panel_id = self.panels[idx].id;
        let region = InputRegion::of(self.vis, panel_id, pending.placement.logical);
        self.hits = match region {
            InputRegion::Panel { .. } => Some(HitScene::of(
                panel_id,
                &pending.scene,
                pending.scroll,
                pending.scale,
            )),
            InputRegion::Empty => None,
        };

        let panel = &mut self.panels[idx];
        if let Some(viewport) = &panel.viewport {
            viewport.set_destination(pending.placement.logical.0.max(1), pending.placement.logical.1.max(1));
        }
        // The whole panel accepts input while it is shown: ADR-0003
        // moved wheel and click onto the popup's own region, and
        // Wayland has no machine-wide mouse hook to synthesize them
        // from. Region coordinates are surface-local, i.e. logical.
        if let Ok(wl) = Region::new(&self.compositor) {
            if let Some((x, y, w, h)) = region.rect() {
                wl.add(x, y, w, h);
            }
            surface.set_input_region(Some(wl.wl_region()));
        }
        surface.damage_buffer(0, 0, bw, bh);
        surface.frame(&self.qh, FrameCallbackData(surface.clone()));
        panel.awaiting_frame = Some(Instant::now());
        buffer.attach_to(&surface).context("attaching the popup's buffer")?;
        panel.layer.commit();
        Ok(())
    }

    /// Hand one surface a fully transparent buffer at its current size
    /// and take its input region away.
    ///
    /// A coalesced frame queued before the hide is dropped with it:
    /// letting a frame callback repaint it afterwards would put a
    /// clickable panel back on a screen the Controller believes is
    /// clear. The pointer's frame goes too - it is the same fact.
    fn clear(&mut self, idx: usize) {
        let logical = self.panels[idx].configured.unwrap_or((1, 1));
        self.panels[idx].pending = None;
        if self.hits.as_ref().is_some_and(|h| h.panel == self.panels[idx].id) {
            self.hits = None;
        }
        self.pointer.leave(self.panels[idx].id);
        self.hidden_frame(idx, logical);
    }

    /// The hidden frame. Not frame-gated: a hidden surface receives no
    /// frame callbacks, so waiting for one would never let it hide.
    fn hidden_frame(&mut self, idx: usize, logical: (i32, i32)) {
        let panel = &mut self.panels[idx];
        let surface = panel.layer.wl_surface().clone();
        let scale = place::fractional(panel.preferred, &panel.geo);
        let bw = ((f64::from(logical.0) * scale).round() as i32).max(1);
        let bh = ((f64::from(logical.1) * scale).round() as i32).max(1);
        let buffer = match self.pool.create_buffer(bw, bh, bw * 4, Format::Argb8888) {
            Ok((buffer, canvas)) => {
                if let Some(mut pixmap) = tiny_skia::PixmapMut::from_bytes(canvas, bw as u32, bh as u32)
                {
                    paint::transparent(&mut pixmap);
                }
                buffer
            }
            Err(e) => {
                self.notes.push(format!("popup: no buffer to hide with: {e}"));
                return;
            }
        };
        let panel = &mut self.panels[idx];
        if let Some(viewport) = &panel.viewport {
            viewport.set_destination(logical.0.max(1), logical.1.max(1));
        }
        if let Ok(region) = Region::new(&self.compositor) {
            // Empty region: the pointer falls through to whatever is
            // underneath, which is the whole point of hiding.
            surface.set_input_region(Some(region.wl_region()));
        }
        surface.damage_buffer(0, 0, bw, bh);
        let _ = buffer.attach_to(&surface);
        panel.layer.commit();
    }
}

/// Premultiplied RGBA (tiny-skia's byte order) to `Argb8888` (wl_shm's:
/// a little-endian 0xAARRGGBB word, i.e. B,G,R,A in memory). One pass
/// over the buffer we just painted, in place - the alternative is
/// painting into a second pixmap and copying.
fn to_argb(data: &mut [u8]) {
    let (pixels, _) = data.as_chunks_mut::<4>();
    for px in pixels {
        px.swap(0, 2);
    }
}

/// The theme a config asks for. Same shape as the Windows bin's
/// `theme_from_config`, so the two platforms disagree about nothing but
/// the default family.
fn theme_from_config(config: &Config) -> Theme {
    let mut theme = match config.popup.theme.as_str() {
        "light" => Theme::light(),
        _ => Theme::dark(),
    };
    theme.font_name = config.popup.font.clone();
    theme
}

fn layer_of(layer: PopupLayer) -> Layer {
    match layer {
        PopupLayer::Overlay => Layer::Overlay,
        PopupLayer::Top => Layer::Top,
    }
}

fn layer_name(layer: Layer) -> &'static str {
    match layer {
        Layer::Overlay => "overlay",
        Layer::Top => "top",
        Layer::Bottom => "bottom",
        Layer::Background => "background",
        _ => "unknown",
    }
}

// ---- the dispatch plumbing ----

/// One `Dispatch` per interface and user-data pair, forwarding to the
/// `Dispatch2` impl SCTK ships for that pair. See the module doc for
/// why this is not `delegate_dispatch2!`.
macro_rules! forward {
    ($iface:ty, $udata:ty) => {
        impl Dispatch<$iface, $udata> for App {
            fn event(
                state: &mut App,
                proxy: &$iface,
                event: <$iface as Proxy>::Event,
                data: &$udata,
                conn: &Connection,
                qh: &QueueHandle<App>,
            ) {
                <$udata as Dispatch2<$iface, App>>::event(data, state, proxy, event, conn, qh);
            }

            fn event_created_child(
                opcode: u16,
                qh: &QueueHandle<App>,
            ) -> Arc<dyn wayland_client::backend::ObjectData> {
                <$udata as Dispatch2<$iface, App>>::event_created_child(opcode, qh)
            }
        }
    };
}

forward!(WlCompositor, GlobalData);
forward!(WlSurface, SurfaceData<()>);
forward!(WlCallback, FrameCallbackData);
forward!(WlShm, GlobalData);
forward!(ZwlrLayerShellV1, GlobalData);
forward!(ZwlrLayerSurfaceV1, LayerSurfaceData);
forward!(WlOutput, OutputData);
forward!(ZxdgOutputManagerV1, GlobalData);
forward!(ZxdgOutputV1, OutputData);

wayland_client::delegate_dispatch!(App: [WlRegistry: GlobalListContents] => RegistryState);

/// `wl_buffer` events (only `release`) are handled by the slot pool's
/// own object data, but the pool's *fallback* path still needs the
/// impl to exist.
impl Dispatch<WlBuffer, GlobalData> for App {
    fn event(
        _: &mut App,
        _: &WlBuffer,
        _: <WlBuffer as Proxy>::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

/// The fractional-scale manager and the viewporter have no events;
/// `wp_fractional_scale_v1` has exactly one, and it is the reason the
/// popup never latches a scale.
impl Dispatch<WpFractionalScaleManagerV1, ()> for App {
    fn event(
        _: &mut App,
        _: &WpFractionalScaleManagerV1,
        _: <WpFractionalScaleManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch<WpViewporter, ()> for App {
    fn event(
        _: &mut App,
        _: &WpViewporter,
        _: <WpViewporter as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch<WpViewport, ()> for App {
    fn event(
        _: &mut App,
        _: &WpViewport,
        _: <WpViewport as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch<WpFractionalScaleV1, usize> for App {
    fn event(
        app: &mut App,
        _: &WpFractionalScaleV1,
        event: <WpFractionalScaleV1 as Proxy>::Event,
        idx: &usize,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            app.popup_rescaled(*idx, scale);
        }
    }
}

impl CompositorHandler for App {
    /// The integer `wl_surface` scale. Irrelevant here: the popup keeps
    /// `buffer_scale` at 1 and sizes its buffer from the *fractional*
    /// scale instead (ADR-0004).
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<App>, _: &WlSurface, _: i32) {}

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<App>,
        _: &WlSurface,
        _: wayland_client::protocol::wl_output::Transform,
    ) {
    }

    /// Only the popup asks for frame callbacks, and `App` checks that
    /// before acting: three kinds of layer surface now dispatch into one
    /// state, so nothing here may assume a surface is a panel.
    fn frame(&mut self, _: &Connection, _: &QueueHandle<App>, surface: &WlSurface, _: u32) {
        self.surface_frame(surface);
    }

    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<App>, _: &WlSurface, _: &WlOutput) {}

    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<App>, _: &WlSurface, _: &WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.popup_mut().outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<App>, output: WlOutput) {
        self.popup_mut().map_one(&output);
        self.flush_popup_notes();
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<App>, output: WlOutput) {
        let popup = self.popup_mut();
        if let Some(info) = popup.outputs.info(&output) {
            let geo = place::geometry_of(&info);
            if let Some(panel) = popup.panels.iter_mut().find(|p| p.output == output) {
                panel.geo = geo;
            }
        }
    }

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<App>, output: WlOutput) {
        self.popup_mut().drop_output(&output);
        self.flush_popup_notes();
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.popup_mut().shm
    }
}

/// Configures and closes for every layer surface in the daemon arrive
/// here - the popup's panels, the selector's full-output dim, the
/// outline's frames - because SCTK has one handler per state. `App`
/// sorts them by surface identity; this impl deliberately knows nothing
/// about which is which.
impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<App>, layer: &LayerSurface) {
        self.layer_closed(layer);
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<App>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        self.layer_configured(layer, configure.new_size);
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.popup_mut().registry
    }

    registry_handlers![OutputState, SeatState];
}
