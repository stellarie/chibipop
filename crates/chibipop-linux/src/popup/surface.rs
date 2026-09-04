//! This module owns one layer surface for each output. The code maps
//! every surface at startup. A transparent buffer hides each surface.
//! The code never unmaps a surface.
//!
//! One surface must exist for each output because a layer surface's
//! output is fixed at creation, and its margins are relative to that
//! output. `output = NULL` would give an origin that the code does
//! not choose. The cursor moves across many outputs while the popup
//! runs.
//!
//! The code never unmaps a surface to hide it, because Hyprland
//! animates layer surfaces by default. A map on each lookup would
//! animate the popup visibly on every hover. That behavior is a
//! regression against Windows, where show and hide are instant. This
//! default is better than a rule that tells the user to add
//! `layerrule no_anim`.
//!
//! The sibling module [`super::pointer`] handles pointer input. The
//! input region that this file commits makes the panel reachable.
//! Every frame that this file paints supplies the hit targets that a
//! click resolves against.

use super::place::{self, Placement, Screen, Shown, Visibility};
use super::pointer::{self, Hit, HitScene, InputRegion, Interaction, Pointer, Step};
use super::{paint, physical_theme, text::TextEngine};
use crate::cursor::outputs::OutputGeometry;
use crate::daemon::App;
use anyhow::{anyhow, Context, Result};
use chibipop::config::{Config, PopupLayer};
use chibipop::geom::PhysPoint;
use chibipop::geom::PhysRect;
use chibipop::controller::Button;
use chibipop::present::{AnkiPopupState, Presentation};
use chibipop::select::{Selections, TextAddr};
use chibipop::ui::theme::Theme;
use chibipop::ui::layout::{self, PopupScene, SceneRequest};
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

/// The namespace of the layer surface.
/// `hyprctl layers` and `layerrule` read this namespace.
const NAMESPACE: &str = "chibipop";

/// How long a commit waits for the frame callback before it paints
/// anyway.
///
/// The frame gate must not wait forever. The frame gate bounds paint
/// work by the refresh rate instead of the cursor-event rate. But
/// callbacks stop completely when a surface is occluded. For example,
/// a fullscreen client can cover a hidden `overlay` popup. A gate
/// that waits forever would leave the popup stuck and unable to
/// redraw. Six frames at 60 Hz is long enough that a real cadence
/// never breaks. It is short enough that no user sees the popup
/// stall.
const FRAME_GRACE: Duration = Duration::from_millis(100);

/// One popup to show.
///
/// The platform bin measures the popup. The Controller sends the view
/// model to the bin. The bin returns a rect as `Event::PopupPlaced`.
/// Every measured value lives on this side of the seam.
#[derive(Debug, Clone)]
pub struct ShowRequest {
    pub presentation: Presentation,
    pub anchor: PhysRect,
    /// Physical pixels, as the Controller counts them.
    pub scroll: i32,
    pub show_back: bool,
    /// `Some` reserves the Anki slot. Linux paints the slot in the
    /// panel.
    pub anki: Option<AnkiPopupState>,
    /// The Controller's selection state for the shown surface.
    ///
    /// The request owns a copy because the popup can repaint after the
    /// Controller returns. The current request also survives a reshow.
    pub selection: Option<Selections>,
}

/// What one show produced.
///
/// This struct holds the exact payload of `Event::PopupPlaced`. It
/// also holds the fields that the log needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    pub rect: PhysRect,
    pub content_h: i32,
    pub view_h: i32,
    /// The scale used to raster this frame.
    pub scale: f64,
    pub output: usize,
}

/// A frame that is waiting for the compositor.
///
/// Every commit is frame-gated. The frame gate bounds paint work by
/// the refresh rate, not by the cursor-event rate, at the cost of up
/// to one frame of positional lag. When several commits arrive before
/// the callback fires, they coalesce into one pending state, so only
/// the latest commit wins.
///
/// Show and hide are the two exceptions. The code must never
/// frame-gate them. A hidden or occluded surface receives no frame
/// callbacks, so a gate on show or hide would leave the popup unable
/// to hide or show again.
struct Pending {
    placement: Placement,
    scale: f64,
    scene: PopupScene,
    theme: Theme,
    scroll: f32,
}

/// One output's surface.
struct Panel {
    /// A stable value across removals, unlike a position in `panels`.
    /// The `wp_fractional_scale_v1` object that the code creates with
    /// a surface carries this id in its user data for the whole life
    /// of the surface. When a monitor unplugs, the code must not let
    /// this id silently point at a neighbor surface.
    id: usize,
    output: WlOutput,
    /// The output's layout facts, in the cursor channel's convention.
    geo: OutputGeometry,
    layer: LayerSurface,
    /// `wp_viewport`: buffer pixels in, logical size out.
    viewport: Option<WpViewport>,
    /// The code keeps this object alive so `preferred_scale`
    /// continues to arrive.
    _scale: Option<WpFractionalScaleV1>,
    /// The last `preferred_scale` value, in 120ths. The code never
    /// latches this value. The code re-reads the value on every
    /// render.
    preferred: Option<u32>,
    /// The logical size the compositor last configured.
    configured: Option<(i32, i32)>,
    /// The time when the code asked for the outstanding
    /// `wl_surface.frame` callback. This field is `None` when no
    /// callback is outstanding.
    awaiting_frame: Option<Instant>,
    /// The one coalesced pending frame.
    pending: Option<Pending>,
}

/// Everything the popup owns.
pub struct Popup {
    /// The queue that created every popup object. The code stores
    /// this queue so the popup's own API needs no `QueueHandle`
    /// argument. The daemon reaches the popup from a control-socket
    /// callback and from a cursor sample. Neither callback is a
    /// Wayland dispatch.
    qh: QueueHandle<App>,
    compositor: CompositorState,
    outputs: OutputState,
    registry: RegistryState,
    shm: Shm,
    /// `None` on a compositor that advertises no `zwlr_layer_shell_v1`,
    /// for example stock GNOME. The popup then maps no surface, and
    /// every show attempt returns a named error. The rest of the
    /// popup still answers SCTK.
    shell: Option<LayerShell>,
    fractional: Option<WpFractionalScaleManagerV1>,
    viewporter: Option<WpViewporter>,
    pool: SlotPool,
    panels: Vec<Panel>,
    /// This field assigns each `Panel::id`. The code never reuses a
    /// value.
    next_id: usize,
    text: TextEngine,
    /// The painter's decoded-asset cache. This field is `None` when
    /// this build cannot open the dictionary database.
    ///
    /// This cache uses its own `MediaStore` connection on this thread.
    /// The worker owns the dictionary on a different thread, and a
    /// `rusqlite::Connection` is not `Sync`. `None` is a real state,
    /// not a failure. The popup still paints, and every image falls
    /// back to its `alt` text.
    media: Option<chibipop_linux::media::MediaSurfaces>,
    /// Where the dictionary lives. The code stores this path so that
    /// `reconfigure` can reopen `media` after a rebuild.
    ///
    /// This field is the popup's half of the reload contract, and it
    /// is easy to miss. A rebuild renames a new file over this path.
    /// Therefore, a connection opened before the rename keeps reading
    /// the replaced inode for as long as the code holds it open. The
    /// worker reopens the dictionary on `Event::ConfigReloaded`
    /// (`worker::ReopenDict`). This process once held only that one
    /// handle, and now it holds a second handle here. If a reload
    /// reopened only the worker's handle, the popup would still draw
    /// gaiji from the old dictionary, but against the new
    /// dictionary's ids.
    db: std::path::PathBuf,
    /// The logical theme. Every render scales a copy of this theme.
    theme: Theme,
    layer: Layer,
    /// Width and height caps, percent of the output.
    caps: (u8, u8),
    side_panel: bool,
    /// How much of an entry the panel draws. The code re-reads this
    /// value on every reload (`reconfigure`), so a settings change
    /// reaches the popup that is already on screen.
    render: chibipop::ui::layout::RenderSettings,
    vis: Visibility,
    /// What is on screen, kept for a re-render at a new scale.
    current: Option<ShowRequest>,
    /// The latest painted scene for text address lookup.
    ///
    /// `current` holds only the unmeasured request. The scene holds source
    /// spans, so the popup keeps this scene beside the hit frame.
    scene: Option<PopupScene>,
    /// Whether the Controller has an active selection drag.
    dragging: bool,
    /// The text address sent with the last pointer interaction.
    last_text: Option<TextAddr>,
    /// The seat's pointer, and where the pointer is. Wayland has no
    /// machine-wide mouse hook, so this field holds the whole input
    /// state of the popup (ARCHITECTURE.md#input-ladders).
    pointer: Pointer,
    /// The frame on screen, as the pointer resolves against it. Every
    /// repaint takes a fresh copy of this frame, and every hide drops
    /// it. Therefore, a hit test never answers with geometry that the
    /// code no longer paints.
    hits: Option<HitScene>,
    notes: Vec<String>,
}

impl Popup {
    /// Bind the popup's globals. Surfaces come later, once the output
    /// roundtrip has geometry to place them against.
    ///
    /// The layer shell is the one global whose absence is a state,
    /// not an error. Stock GNOME advertises no layer shell, so the
    /// popup then exists with no shell to draw on. It maps no
    /// surface, shows nothing, and reports this state. Meanwhile the
    /// seat, the outputs, and the shm pool, which the popup also
    /// owns, keep serving the handlers that SCTK dispatches into
    /// `App`. If this function returned `Err` here instead, it would
    /// leave those live proxies with nothing to answer their events.
    /// That gap is exactly how a session with no layer shell used to
    /// panic on its first `wl_shm::format`.
    ///
    /// Therefore, an `Err` from this function means something that no
    /// session can work without: no `wl_compositor`, no `wl_shm`, or
    /// no shared memory. The capability report already names each of
    /// these as fatal.
    pub fn bind(
        globals: &GlobalList,
        qh: &QueueHandle<App>,
        config: &Config,
        db: &std::path::Path,
    ) -> Result<Popup> {
        let compositor = CompositorState::bind(globals, qh)
            .map_err(|e| anyhow!("binding wl_compositor: {e}"))?;
        let shm = Shm::bind(globals, qh).map_err(|e| anyhow!("binding wl_shm: {e}"))?;
        // A full 4K screen is 33 MB. The pool grows on demand.
        // Therefore, the code starts at one modest popup size and
        // lets the pool grow from there.
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

        // The code resolves the font against the engine's own font
        // database. Therefore, a config value carried over from
        // Windows, such as `Yu Gothic UI`, degrades to the Linux
        // default font with a visible notice. It does not silently
        // draw tofu glyphs.
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

        // The pointer (ARCHITECTURE.md#input-ladders). The code binds
        // the seat now. The pointer itself arrives later with the
        // seat's `capabilities` event. The same event reports a
        // pointer that a user plugs in later. Without
        // `wp_cursor_shape_v1`, the code leaves the cursor as it was
        // over the panel. This popup never loads XCursor themes
        // itself.
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

        // The painter's own decoded-asset cache, on its own read-only
        // connection to the media store. A database that this build
        // cannot open produces one diagnostic and no images. It never
        // stops the popup from drawing. An image node then renders
        // its `alt` text, the character that the image stood for.
        let media = open_media(db, &mut notes);

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
            db: db.to_path_buf(),
            theme,
            layer: layer_of(config.popup_layer()),
            caps: (config.popup.max_width_percent, config.popup.max_height_percent),
            side_panel: config.popup.side_panel,
            render: config.popup.render_settings(),
            vis: Visibility::Hidden,
            current: None,
            scene: None,
            dragging: false,
            last_text: None,
            pointer,
            hits: None,
            notes,
        })
    }

    /// Can this popup ever draw? This function returns false on a
    /// compositor with no layer shell. That case is a supported
    /// state, not an error. The daemon reports the popup channel
    /// down and keeps every other channel running.
    pub fn available(&self) -> bool {
        self.shell.is_some()
    }

    /// Diagnostics accumulated since the last drain. The popup owns
    /// no log. The daemon thread owns the log.
    pub fn drain_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// One diagnostic, for the handlers that live in the sibling
    /// module. The popup owns no log. The daemon drains these
    /// diagnostics.
    pub fn note(&mut self, line: String) {
        self.notes.push(line);
    }

    pub fn surface_count(&self) -> usize {
        self.panels.len()
    }

    /// The outputs, as the surfaces beside the popup need them.
    ///
    /// Only one `OutputState` serves the whole daemon.
    /// `registry_handlers!` names exactly one `OutputState`.
    /// Therefore, the selector and the outline read the monitor list
    /// here instead of binding a second `OutputState`. They also get
    /// the popup's own surface ids with the list. This is why
    /// "surface 1" means the same monitor in every diagnostic that
    /// this daemon writes.
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

    /// The one `wl_shm` and the one `wl_compositor` that this process
    /// bound.
    ///
    /// The selector and the outline share these two globals, so
    /// neither binds a second copy. A `wl_shm`'s format list is
    /// per-process knowledge. A `wl_compositor` is a factory handle
    /// that is worth holding exactly once. A `SlotPool` is different:
    /// the code never shares a `SlotPool`. Each `SlotPool` is an
    /// mmap, and one per surface set is the goal.
    pub fn shm(&self) -> &Shm {
        &self.shm
    }

    pub fn compositor(&self) -> &CompositorState {
        &self.compositor
    }

    /// The layer that carries every popup panel.
    ///
    /// The Press-mode catcher must share this layer. A lower layer cannot
    /// receive a click that the popup owns, so the catcher reads this value
    /// instead of keeping a second config mapping.
    pub fn layer(&self) -> Layer {
        self.layer
    }

    /// The one `wp_viewporter`. This function returns no
    /// fractional-scale object beside it. The popup already has a
    /// surface on every output, so its `preferred_scale` is this
    /// daemon's single source of scale truth. The other two surfaces
    /// read this scale back through [`Popup::screens`]. Nothing
    /// latches the scale there either.
    pub fn viewporter(&self) -> Option<&WpViewporter> {
        self.viewporter.as_ref()
    }

    /// Return the popup pointer's shape source.
    ///
    /// The Press-mode catcher reuses this pointer and shape manager. A second
    /// `wl_pointer` would split enter serials from the pointer events it shapes.
    pub fn pointer_shape_source(&self) -> Option<(&CursorShapeManager, &WlPointer)> {
        self.pointer.shape_source()
    }

    /// The logical theme that this popup resolved from the config.
    /// The popup re-resolves this theme on every reload.
    ///
    /// The scan outline reads its four `scan_*` colors from this
    /// theme instead of resolving a second theme of its own. The
    /// outline draws beside the popup, for the same lookup, so there
    /// is exactly one correct answer for what this session looks
    /// like. Lengths in this theme are logical values. The outline's
    /// own lengths are physical constants, so the outline never wants
    /// the scaled copy that [`physical_theme`] makes for a render.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Does one `wl_surface` belong to the popup?
    ///
    /// Every shared SCTK handler now asks this routing question.
    /// Three kinds of layer surface answer into one `App`. Therefore,
    /// the code must sort a `configure` or a `frame` event by
    /// identity. The code must never assume that such an event
    /// belongs to the popup.
    pub fn owns(&self, surface: &WlSurface) -> bool {
        self.panels.iter().any(|p| p.layer.wl_surface() == surface)
    }

    /// The same question for a `closed` or `configure` event. These
    /// events name a layer surface, not a `wl_surface`.
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

    // ---- pointer input ----

    /// The seat, for SCTK's own `wl_seat` dispatch.
    pub fn seats(&mut self) -> &mut SeatState {
        self.pointer.seats()
    }

    /// The seat gained a pointer. The code takes the pointer and a
    /// matching cursor-shape device. [`Popup::pointer_gone`] drops
    /// both again.
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

    /// Which surface a pointer event names, if the surface belongs to
    /// the popup.
    pub fn panel_of(&self, surface: &WlSurface) -> Option<usize> {
        self.panels.iter().find(|p| p.layer.wl_surface() == surface).map(|p| p.id)
    }

    /// The pointer crossed onto a panel.
    ///
    /// The code can reach this function only while the popup is
    /// shown. A hidden surface's input region is empty, so the
    /// compositor never sends this event. Each logged crossing proves
    /// that the region works. `serial` is `None` for a scripted pass,
    /// because a scripted pass has no real enter event to quote.
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

    /// Resolve a button press against the frame on screen.
    pub fn pointer_press(&mut self, pos: (f64, f64), button: Button) -> Option<Interaction> {
        self.pointer.motion(pos);
        let (local, hit) = {
            let hits = self.hits.as_ref()?;
            if self.pointer.focus()?.panel != hits.panel {
                return None;
            }
            let local = hits.local(pos);
            (local, hits.hit(local))
        };
        self.pointer.button_down(button);
        let text = if matches!(&hit, Some(Hit::Anki)) { None } else { self.text_hit(local) };
        self.last_text = text;
        Some(pointer::press(local, button, hit, text))
    }

    /// Resolve a button release against the frame on screen.
    pub fn pointer_release(&mut self, pos: (f64, f64), button: Button) -> Option<Interaction> {
        self.pointer.motion(pos);
        let held = self.pointer.button_up(button);
        if !held {
            return None;
        }
        let local = {
            let hits = self.hits.as_ref()?;
            if self.pointer.focus()?.panel != hits.panel {
                return None;
            }
            hits.local(pos)
        };
        self.last_text = None;
        Some(Interaction::Up { local, button })
    }

    /// Resolve pointer motion during a button hold or an active drag.
    pub fn pointer_move(&mut self, pos: (f64, f64)) -> Option<Interaction> {
        self.pointer.motion(pos);
        self.hover();
        if !self.pointer.forwards_motion() {
            return None;
        }
        let local = {
            let hits = self.hits.as_ref()?;
            if self.pointer.focus()?.panel != hits.panel {
                return None;
            }
            hits.local(pos)
        };
        let text = self.text_hit(local);
        self.last_text = text;
        Some(Interaction::Move { local, text })
    }

    /// Resolve the current pointer against the latest painted scene.
    pub fn drag_move(&mut self) -> Option<Interaction> {
        if !self.dragging {
            return None;
        }
        let pos = self.pointer.focus()?.pos;
        let local = {
            let hits = self.hits.as_ref()?;
            if self.pointer.focus()?.panel != hits.panel {
                return None;
            }
            hits.local(pos)
        };
        let text = self.text_hit(local);
        if self.last_text == text {
            return None;
        }
        self.last_text = text;
        Some(Interaction::Move { local, text })
    }

    /// Tell the pointer whether the Controller has an active selection drag.
    pub fn set_dragging(&mut self, dragging: bool) {
        self.dragging = dragging;
        self.pointer.set_dragging(dragging);
        if !dragging {
            self.last_text = None;
        }
    }

    pub fn pointer_forwards_motion(&self) -> bool {
        self.pointer.forwards_motion()
    }

    /// Resolve one panel-local physical point to a gloss address.
    ///
    /// The surface stores the scene and uses the text engine for that scene.
    /// Therefore, the pointer never measures a different scene after a repaint.
    pub fn text_hit(&mut self, local: PhysPoint) -> Option<TextAddr> {
        let scroll = self.hits.as_ref()?.scroll;
        let scene = self.scene.as_ref()?;
        scene
            .text_hit(
                (local.x as f32, local.y as f32),
                scroll,
                &self.theme.font_name,
                &mut self.text,
            )
            .ok()
            .flatten()
    }

    /// The primary-button press for the scripted path.
    pub fn pointer_button(&mut self, pos: (f64, f64)) -> Option<Interaction> {
        self.pointer_press(pos, Button::Primary)
    }

    /// A wheel frame over the panel.
    pub fn pointer_wheel(&mut self, axis: &AxisScroll) -> Option<Interaction> {
        self.pointer.wheel(axis)
    }

    /// The same wheel path, driven from a raw `axis_value120`. A
    /// scripted pass needs this path because it holds no real
    /// `wl_pointer` frame.
    pub fn pointer_wheel_120(&mut self, value120: i32) -> Option<Interaction> {
        self.pointer_wheel(&AxisScroll { value120, ..AxisScroll::default() })
    }

    /// Drop the banked sub-notch delta. The Controller sends
    /// `Command::DiscardScroll` when a fresh popup replaces the old
    /// one.
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

    /// The code calls this function before a fresh frame arrives. It
    /// arms the next scripted pass.
    pub fn arm_script(&mut self) {
        self.pointer.arm_script();
    }

    /// The next scripted pass (`CHIBIPOP_POINTER_SCRIPT`), when one is
    /// armed and a frame is on screen.
    ///
    /// The daemon drives the steps, not this module. Each step's
    /// effect must reach the Controller and return as a repaint
    /// before the next step resolves. Otherwise, a scripted click
    /// could resolve against the old frame that a scripted scroll
    /// had just replaced.
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

    /// The frame's hit targets, in the logical coordinates that a
    /// pointer event carries. A scripted pass needs these coordinates
    /// to aim at anything.
    pub fn dump_hits(&mut self, selection: Option<&Selections>) {
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
        let items = selection
            .and_then(|all| all.card(0))
            .map_or(0, |card| card.items().len());
        let highlights = self.scene.as_ref().map_or(0, |scene| scene.highlights.len());
        self.notes.push(format!(
            "pointer: selection: card 0 items={items} highlights={highlights}"
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

    /// Map one hidden surface for each known output. The code maps
    /// every surface now, and each surface stays mapped. Only the
    /// active surface ever holds a popup-sized buffer.
    pub fn map_all(&mut self) {
        let outputs: Vec<WlOutput> = self.outputs.outputs().collect();
        for output in outputs {
            self.map_one(&output);
        }
    }

    /// Map the surface for one output, if the output has no surface
    /// yet.
    ///
    /// This function is idempotent on purpose. SCTK announces an
    /// output twice over a startup roundtrip: once from the initial
    /// global list, and once when `new_output` fires as the output's
    /// xdg-output info completes. Two surfaces on one output would
    /// show two popups.
    pub fn map_one(&mut self, output: &WlOutput) {
        if self.panels.iter().any(|p| &p.output == output) {
            return;
        }
        let qh = self.qh.clone();
        let qh = &qh;
        let Some(info) = self.outputs.info(output) else { return };
        let geo = place::geometry_of(&info);
        let id = self.next_id;

        // With no shell, there is no surface. The code creates
        // nothing and spends nothing. The daemon already reported
        // this state once at startup, so this path stays silent
        // instead of adding one note for each output.
        let Some(shell) = self.shell.as_ref() else { return };
        let surface = self.compositor.create_surface(qh);
        let viewport = self.viewporter.as_ref().map(|v| v.get_viewport(&surface, qh, ()));
        let scale = self.fractional.as_ref().map(|f| f.get_fractional_scale(&surface, qh, id));
        let layer = shell.create_layer_surface(qh, surface, self.layer, Some(NAMESPACE), Some(output));
        self.next_id += 1;
        // Fixed rules: the popup must never reserve space, the popup
        // must never take keyboard focus, and the popup's position
        // must use margins from the top-left corner of this output.
        layer.set_anchor(Anchor::TOP | Anchor::LEFT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_margin(0, 0, 0, 0);
        layer.set_size(1, 1);
        // The initial commit carries no buffer. The compositor
        // answers with a configure, and the hidden 1x1 buffer follows
        // it.
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

    /// An output went away, or the compositor closed its surface.
    /// This event leads to routine recreation, not an error.
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
        // The output is still there, so the surface comes back. This
        // is how a compositor asks for recreation.
        if self.outputs.info(&output).is_some() {
            self.map_one(&output);
        }
    }

    /// Re-read the settings that a reload can change, and reopen the
    /// dictionary that a rebuild replaced. `set_layer` needs no
    /// recreation. This is exactly why `popup.layer` is a runtime
    /// toggle, not a restart setting.
    pub fn reconfigure(&mut self, config: &Config) {
        // This is the popup's half of the reload. `reload` arrives
        // here for two different reasons at once. Either the
        // settings window applied a config change, or the settings
        // window finished a rebuild. The control socket carries no
        // way to tell these two reasons apart
        // (ARCHITECTURE.md#settings-and-config: the config file is
        // the only truth), and this code does not need to tell them
        // apart. Reopening on a plain config reload costs one
        // `sqlite3_open` call and an emptied cache. Skipping the
        // reopen after a real rebuild costs the popup every gaiji
        // that it draws, because a rebuild renames a new file over
        // this path and renumbers the `dict_id` that this cache uses
        // as a key. The worker reopens its own handle in response to
        // the same event.
        self.media = open_media(&self.db, &mut self.notes);
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
        // Windows arms its wheel hook on each dispatch tick from the
        // same setting. The popup's own region has nothing to arm, so
        // the gate lives on the pointer instead.
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
        // The code re-measures a shown popup at the new settings.
        if self.vis.shown().is_some() {
            if let Some(req) = self.current.clone() {
                let _ = self.show(&req);
            }
        }
    }

    /// Measure, place, raster, and commit. The daemon feeds the
    /// returned [`Placed`] back to the Controller as
    /// `Event::PopupPlaced`.
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

        // The caller decides whether the popup reserves the Anki
        // slot. The Controller knows whether AnkiConnect answered.
        // The surface does not know.
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
                selection: req.selection.as_ref(),
            },
            &mut self.text,
        )
        .with_context(|| "laying out the popup".to_string())?;

        // Width: the panel's own width when a side column widened it.
        // Otherwise the width is exactly the box that core offered.
        // The Windows bin follows the same rule, so the two platforms
        // wrap text identically.
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

    /// Re-render what is on screen. `Ok(None)` means nothing was on
    /// screen.
    pub fn reshow(&mut self) -> Result<Option<Placed>> {
        let Some(req) = self.current.clone() else { return Ok(None) };
        if self.vis.shown().is_none() {
            return Ok(None);
        }
        self.show(&req).map(Some)
    }

    /// Hide the popup: a transparent buffer and an empty input
    /// region, never an unmap. This action is cheap and instant. A
    /// hide while the popup is already hidden costs nothing.
    ///
    /// The pointer also loses its frame here. An empty input region
    /// means that no `leave` event may ever arrive. Therefore, the
    /// popup forgets what was under the cursor instead of waiting for
    /// a `leave` event that may never come.
    pub fn hide(&mut self) {
        if let Some(slot) = self.vis.hide().and_then(|id| self.slot(id)) {
            self.clear(slot);
        }
        self.current = None;
    }

    /// Handle one `preferred_scale` event. The code records the
    /// value, and re-renders when this surface is the one currently
    /// shown. The scale is never latched, so this is the only place
    /// that corrects a compositor that first sends 1.0 and then sends
    /// 1.5.
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

    /// A configure event landed. Draw the frame that the configure
    /// requested.
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

    /// The compositor consumed the last frame. Commit the coalesced
    /// frame.
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

    /// A show either resizes or draws right away. A resize costs a
    /// configure round trip, once for each new entry and never for a
    /// cursor move. The intermediate commit attaches no buffer, so
    /// there is no visible jump.
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

        // Keep the exact scene that reached the buffer.
        // Text address lookup must use the source geometry from this painted frame.
        // Record the highlight count for a human tester or a scripted pass.
        // A repaint can combine several requests before it reaches the buffer.
        let highlights = pending.scene.highlights.len();
        if highlights > 0 {
            self.notes.push(format!("popup: painted {highlights} selection highlight box(es)"));
        }
        self.scene = Some(pending.scene.clone());

        // The targets that a click resolves against come from the
        // frame that the code paints right now, never from a
        // remembered frame. This rule keeps a hit honest across a
        // scroll, where the offset moves, and across a scale change,
        // where every rect changes too.
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
        // The whole panel accepts input while the popup is shown.
        // Wheel and click events live on the popup's own region,
        // because Wayland has no machine-wide mouse hook to
        // synthesize them from. Region coordinates are surface-local,
        // that is, logical units.
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

    /// Give one surface a fully transparent buffer at its current
    /// size, and take away its input region.
    ///
    /// This function also drops any coalesced frame that was queued
    /// before the hide. If a later frame callback repainted that
    /// frame, a clickable panel would return to a screen that the
    /// Controller believes is clear. The pointer's frame goes too,
    /// for the same reason.
    fn clear(&mut self, idx: usize) {
        let logical = self.panels[idx].configured.unwrap_or((1, 1));
        self.panels[idx].pending = None;
        if self.hits.as_ref().is_some_and(|h| h.panel == self.panels[idx].id) {
            self.hits = None;
        }
        self.scene = None;
        self.dragging = false;
        self.last_text = None;
        self.pointer.cancel();
        self.hidden_frame(idx, logical);
    }

    /// The hidden frame. The code never frame-gates this commit. A
    /// hidden surface receives no frame callbacks, so a wait for one
    /// would leave the popup unable to hide.
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
            // underneath. This behavior is exactly the goal of a
            // hide.
            surface.set_input_region(Some(region.wl_region()));
        }
        surface.damage_buffer(0, 0, bw, bh);
        let _ = buffer.attach_to(&surface);
        panel.layer.commit();
    }
}

/// Convert premultiplied RGBA, which is tiny-skia's byte order, to
/// `Argb8888`, which is `wl_shm`'s byte order: a little-endian
/// 0xAARRGGBB word, that is, B, G, R, A in memory. This function
/// makes one pass over the buffer that the code just painted, in
/// place. The alternative would paint into a second pixmap and then
/// copy it.
fn to_argb(data: &mut [u8]) {
    let (pixels, _) = data.as_chunks_mut::<4>();
    for px in pixels {
        px.swap(0, 2);
    }
}

/// Open the painter's decoded-asset cache on a fresh connection.
/// Returns `None` and a note when the open fails.
///
/// One function serves both call sites. The store can fail to open
/// at bind time, or fail to reopen after a rebuild. Both failures
/// must answer the same way. A database that this build cannot open
/// produces one diagnostic and no images. It never stops the popup
/// from drawing. An image node then renders its `alt` text, the
/// character that the image stood for.
fn open_media(
    db: &std::path::Path,
    notes: &mut Vec<String>,
) -> Option<chibipop_linux::media::MediaSurfaces> {
    match chibipop_linux::media::MediaSurfaces::open(db) {
        Ok(cache) => Some(cache),
        Err(e) => {
            notes.push(format!(
                "popup: no dictionary media - {e:#}; image nodes will render their alt text"
            ));
            None
        }
    }
}

/// The theme that a config asks for. This function is not the same
/// as the Windows bin's `theme_from_config`. The Windows function
/// also reads and parses `popup.css`, which Linux never does. A CSS
/// override therefore works on Windows and is silently ignored here
/// (issue #53).
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

/// This macro defines one `Dispatch` impl for each interface and
/// user-data pair. Each impl forwards to the `Dispatch2` impl that
/// SCTK ships for that pair. See the module doc for why this macro
/// exists instead of `delegate_dispatch2!`.
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

/// The slot pool's own object data handles `wl_buffer` events. The
/// only such event is `release`. But the pool's fallback path still
/// needs this impl to exist.
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

/// The fractional-scale manager and the viewporter have no events.
/// `wp_fractional_scale_v1` has exactly one event, and this event is
/// the reason that the popup never latches a scale.
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
    /// The integer `wl_surface` scale. This value is not relevant
    /// here. The popup keeps `buffer_scale` at 1, and sizes its
    /// buffer from the fractional scale instead. The code never
    /// latches that scale.
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<App>, _: &WlSurface, _: i32) {}

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<App>,
        _: &WlSurface,
        _: wayland_client::protocol::wl_output::Transform,
    ) {
    }

    /// Only the popup asks for frame callbacks, and `App` checks this
    /// fact before it acts. Three kinds of layer surface now dispatch
    /// into one state, so this code must never assume that a surface
    /// is a panel.
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
        {
            let popup = self.popup_mut();
            if let Some(info) = popup.outputs.info(&output) {
                let geo = place::geometry_of(&info);
                if let Some(panel) = popup.panels.iter_mut().find(|p| p.output == output) {
                    panel.geo = geo;
                }
            }
        }
        self.catcher_output_updated(&output);
    }

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<App>, output: WlOutput) {
        self.catcher_output_destroyed(&output);
        self.popup_mut().drop_output(&output);
        self.flush_popup_notes();
        self.flush_surface_notes();
    }

}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.popup_mut().shm
    }
}

/// Every layer surface in the daemon sends its configure and close
/// events here: the popup's panels, the selector's full-output dim,
/// and the outline's frames. SCTK has one handler for each state.
/// `App` sorts the events by surface identity. This impl deliberately
/// does not know which surface is which.
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
