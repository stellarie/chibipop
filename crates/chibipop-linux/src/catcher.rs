//! The click catcher gives Press mode one outside-click input path.
//!
//! It is a separate full-output layer surface because the popup owns only its
//! panel and the outline must stay click-through. A strict lower layer cannot
//! receive a click that a higher popup surface owns, so this surface shares the
//! popup layer and leaves a hole over the popup. The catcher never unmaps. It
//! keeps one transparent buffer attached and changes only its input region.
//! The one outside click is swallowed by design. The daemon receives it as a
//! dismissal event instead of passing it to the application below.

use crate::daemon::App;
use crate::popup::{derive, Popup, Screen};
use chibipop::geom::PhysRect;
use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerSurface,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::QueueHandle;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

/// One logical rectangle in a `wl_region`.
///
/// The region uses surface-local logical units, unlike the physical popup
/// rectangle that derives it. The type keeps that conversion visible in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Return the full output region with the popup rectangle removed.
///
/// `derive` supplies the same output-local rounding as popup placement. The
/// four rectangles around the hole form a union that Wayland can use as an
/// input region. An empty result means that the popup covers the whole surface.
pub fn input_region_rects(
    screen: PhysRect,
    scale: f64,
    logical: (i32, i32),
    popup: PhysRect,
) -> Vec<RegionRect> {
    let (surface_w, surface_h) = (logical.0.max(0), logical.1.max(0));
    if surface_w == 0 || surface_h == 0 {
        return Vec::new();
    }
    if popup.w <= 0 || popup.h <= 0 {
        return vec![RegionRect { x: 0, y: 0, w: surface_w, h: surface_h }];
    }

    let placement = derive(popup, screen, scale);
    let popup_x = placement.margin.1;
    let popup_y = placement.margin.0;
    let popup_w = placement.logical.0.max(0);
    let popup_h = placement.logical.1.max(0);
    let popup_right = popup_x.saturating_add(popup_w);
    let popup_bottom = popup_y.saturating_add(popup_h);

    let hole_x = popup_x.max(0).min(surface_w);
    let hole_y = popup_y.max(0).min(surface_h);
    let hole_right = popup_right.max(0).min(surface_w);
    let hole_bottom = popup_bottom.max(0).min(surface_h);
    if hole_right <= hole_x || hole_bottom <= hole_y {
        return vec![RegionRect { x: 0, y: 0, w: surface_w, h: surface_h }];
    }

    let hole_h = hole_bottom - hole_y;
    let mut regions = Vec::with_capacity(4);
    if hole_y > 0 {
        regions.push(RegionRect { x: 0, y: 0, w: surface_w, h: hole_y });
    }
    if hole_bottom < surface_h {
        regions.push(RegionRect {
            x: 0,
            y: hole_bottom,
            w: surface_w,
            h: surface_h - hole_bottom,
        });
    }
    if hole_x > 0 {
        regions.push(RegionRect { x: 0, y: hole_y, w: hole_x, h: hole_h });
    }
    if hole_right < surface_w {
        regions.push(RegionRect {
            x: hole_right,
            y: hole_y,
            w: surface_w - hole_right,
            h: hole_h,
        });
    }
    regions
}

/// One full-output catcher surface.
struct Pane {
    id: usize,
    rect: PhysRect,
    scale: f64,
    layer: LayerSurface,
    viewport: Option<WpViewport>,
    configured: Option<(i32, i32)>,
    buffer: Option<Buffer>,
    buffer_size: Option<(i32, i32)>,
    active: bool,
}

/// The transparent full-output input layer for Press mode.
pub struct Catcher {
    qh: QueueHandle<App>,
    compositor: CompositorState,
    shell: LayerShell,
    viewporter: Option<WpViewporter>,
    pool: SlotPool,
    layer: Layer,
    panes: Vec<Pane>,
    popup: Option<PhysRect>,
    shape: Option<WpCursorShapeDeviceV1>,
    shape_pointer: Option<WlPointer>,
    notes: Vec<String>,
}

impl Catcher {
    /// Bind the catcher globals and share the popup's compositor resources.
    ///
    /// The catcher owns its pool because every surface set must keep its buffer
    /// attached while the input region changes. A missing layer shell or pool
    /// leaves Press mode without outside-click capture, like the other optional
    /// surfaces.
    pub fn bind(globals: &GlobalList, qh: &QueueHandle<App>, popup: &Popup) -> Option<Catcher> {
        let shell = LayerShell::bind(globals, qh).ok()?;
        let pool = SlotPool::new(256 * 256 * 4, popup.shm()).ok()?;
        Some(Catcher {
            qh: qh.clone(),
            compositor: popup.compositor().clone(),
            shell,
            viewporter: popup.viewporter().cloned(),
            pool,
            layer: popup.layer(),
            panes: Vec::new(),
            popup: None,
            shape: None,
            shape_pointer: None,
            notes: Vec::new(),
        })
    }

    /// Return diagnostics collected since the last drain.
    pub fn drain_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// Set the layer that the popup currently uses without unmapping panes.
    pub fn set_layer(&mut self, layer: Layer) {
        if self.layer == layer {
            return;
        }
        self.layer = layer;
        for pane in &self.panes {
            pane.layer.set_layer(layer);
            pane.layer.commit();
        }
    }

    /// Show a full-output catcher with a hole over `popup`.
    ///
    /// A pane is created only when this method first needs its output. The
    /// popup rectangle stays in physical pixels until `input_region_rects`
    /// applies popup placement's conversion.
    pub fn show(&mut self, screens: &[Screen], popup: PhysRect) {
        self.popup = Some(popup);

        for idx in 0..self.panes.len() {
            if !screens.iter().any(|screen| screen.id == self.panes[idx].id) {
                self.panes[idx].active = false;
                self.commit_region(idx);
            }
        }

        for screen in screens {
            let idx = match self.panes.iter().position(|pane| pane.id == screen.id) {
                Some(idx) => idx,
                None => self.map(screen),
            };
            self.panes[idx].rect = screen.rect;
            self.panes[idx].scale = screen.scale;
            self.panes[idx].active = true;
            if self.panes[idx].configured.is_some() {
                if self.panes[idx].buffer.is_none() {
                    self.commit_configured(idx);
                } else {
                    self.commit_region(idx);
                }
            }
        }
    }

    /// Hide every pane by clearing its input region.
    ///
    /// The transparent buffer stays attached and no layer surface is unmapped.
    pub fn hide(&mut self) {
        self.popup = None;
        for idx in 0..self.panes.len() {
            self.panes[idx].active = false;
            self.commit_region(idx);
        }
    }

    /// Test layer identity before shared SCTK handlers route a configure or close.
    ///
    /// The catcher owns several same-role surfaces, so the handler must not
    /// assume that each callback names the popup panel.
    pub fn owns_layer(&self, layer: &LayerSurface) -> bool {
        self.panes.iter().any(|pane| &pane.layer == layer)
    }

    /// Attach the popup pointer's cursor-shape device to this catcher.
    ///
    /// The popup already owns the seat pointer and its shape manager. Reusing
    /// that source avoids a second pointer object and keeps cursor requests on
    /// the same pointer that produced these events.
    pub fn sync_pointer(&mut self, popup: &Popup) {
        let Some((shapes, pointer)) = popup.pointer_shape_source() else {
            if let Some(device) = self.shape.take() {
                device.destroy();
            }
            self.shape_pointer = None;
            return;
        };
        if self.shape_pointer.as_ref() == Some(pointer) {
            return;
        }
        if let Some(device) = self.shape.take() {
            device.destroy();
        }
        self.shape = Some(shapes.get_shape_device(pointer, &self.qh));
        self.shape_pointer = Some(pointer.clone());
    }

    /// Test whether one pointer event landed on a catcher pane.
    ///
    /// The daemon routes each event of a frame by surface. Hyprland sends
    /// one frame for a leave from the catcher and an enter onto the popup,
    /// because both surfaces belong to one client. A rule that claims the
    /// whole frame for the catcher would hide that enter from the popup.
    /// Then the popup holds no focus and ignores every later press.
    pub fn owns_surface(&self, surface: &WlSurface) -> bool {
        self.panes.iter().any(|pane| pane.layer.wl_surface() == surface)
    }

    /// Handle the catcher's events of one pointer frame.
    ///
    /// Enter sets the default shape. Any button press asks the daemon to
    /// dismiss the popup, so this function returns `true` for it. Other events
    /// on the catcher need no answer.
    pub fn pointer_frame(&mut self, events: &[PointerEvent]) -> bool {
        let mut pressed = false;
        for event in events.iter().filter(|event| self.owns_surface(&event.surface)) {
            match &event.kind {
                PointerEventKind::Enter { serial } => {
                    if let Some(device) = self.shape.as_ref() {
                        device.set_shape(*serial, Shape::Default);
                    }
                }
                PointerEventKind::Press { .. } => pressed = true,
                PointerEventKind::Motion { .. }
                | PointerEventKind::Release { .. }
                | PointerEventKind::Axis { .. }
                | PointerEventKind::Leave { .. } => {}
            }
        }
        pressed
    }

    /// Update one pane after its output geometry changes.
    pub fn update_output(&mut self, screen: &Screen) {
        let Some(idx) = self.panes.iter().position(|pane| pane.id == screen.id) else {
            return;
        };
        self.panes[idx].rect = screen.rect;
        self.panes[idx].scale = screen.scale;
        if self.panes[idx].active && self.panes[idx].configured.is_some() {
            self.commit_region(idx);
        }
    }

    /// Record a configure and attach a transparent buffer.
    /// SCTK acknowledges the configure before this function runs.
    pub fn configured(&mut self, layer: &LayerSurface, size: (u32, u32)) {
        let Some(idx) = self.panes.iter().position(|pane| &pane.layer == layer) else {
            return;
        };
        let logical = (size.0 as i32, size.1 as i32);
        self.panes[idx].configured = Some(logical);
        self.commit_configured(idx);
    }

    /// Remove a catcher pane after its layer surface closes.
    pub fn drop_layer(&mut self, layer: &LayerSurface) {
        let Some(idx) = self.panes.iter().position(|pane| &pane.layer == layer) else {
            return;
        };
        let id = self.panes[idx].id;
        self.panes.remove(idx);
        self.notes.push(format!("catcher: surface {id} closed"));
    }

    /// Remove the pane for a destroyed output.
    pub fn drop_output(&mut self, id: usize) {
        let Some(idx) = self.panes.iter().position(|pane| pane.id == id) else {
            return;
        };
        self.panes.remove(idx);
        self.notes.push(format!("catcher: output {id} destroyed"));
    }

    fn map(&mut self, screen: &Screen) -> usize {
        let qh = self.qh.clone();
        let surface = self.compositor.create_surface(&qh);
        let viewport = self.viewporter.as_ref().map(|v| v.get_viewport(&surface, &qh, ()));
        let layer = self.shell.create_layer_surface(
            &qh,
            surface,
            self.layer,
            Some("chibipop-catcher"),
            Some(&screen.output),
        );
        // A zero size needs anchors on both axes. The compositor then gives the
        // surface the whole output.
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_margin(0, 0, 0, 0);
        layer.set_size(0, 0);
        layer.commit();
        self.notes.push(format!(
            "catcher: full-output surface {} on {} (same layer, keyboard none)",
            screen.id, screen.name
        ));
        self.panes.push(Pane {
            id: screen.id,
            rect: screen.rect,
            scale: screen.scale,
            layer,
            viewport,
            configured: None,
            buffer: None,
            buffer_size: None,
            active: false,
        });
        self.panes.len() - 1
    }

    /// Attach the transparent buffer and apply the input region.
    ///
    /// With a viewport, one 1x1 pixel scaled to the surface is enough. A
    /// full-size buffer for each output would cost megabytes of shared memory
    /// for pixels that are all transparent.
    fn commit_configured(&mut self, idx: usize) {
        let Some(logical) = self.panes[idx].configured else {
            return;
        };
        let logical = (logical.0.max(1), logical.1.max(1));
        let size = if self.panes[idx].viewport.is_some() { (1, 1) } else { logical };
        let mut attach = false;
        if self.panes[idx].buffer_size != Some(size) {
            match self.pool.create_buffer(
                size.0,
                size.1,
                size.0 * 4,
                wayland_client::protocol::wl_shm::Format::Argb8888,
            ) {
                Ok((buffer, canvas)) => {
                    canvas.fill(0);
                    self.panes[idx].buffer = Some(buffer);
                    self.panes[idx].buffer_size = Some(size);
                    attach = true;
                }
                Err(error) => self
                    .notes
                    .push(format!("catcher: no transparent buffer for {}x{}: {error}", size.0, size.1)),
            }
        }

        let surface = self.panes[idx].layer.wl_surface().clone();
        self.set_region(idx);
        if attach {
            if let Some(buffer) = self.panes[idx].buffer.as_ref() {
                if let Err(error) = buffer.attach_to(&surface) {
                    self.notes
                        .push(format!("catcher: attaching the transparent buffer failed: {error}"));
                } else {
                    surface.damage_buffer(0, 0, size.0, size.1);
                }
            }
        }
        if let Some(viewport) = &self.panes[idx].viewport {
            viewport.set_destination(logical.0, logical.1);
        }
        self.panes[idx].layer.commit();
    }

    fn commit_region(&mut self, idx: usize) {
        self.set_region(idx);
        self.panes[idx].layer.commit();
    }

    fn set_region(&mut self, idx: usize) {
        let (logical, rect, scale, active, popup, surface) = {
            let pane = &self.panes[idx];
            (
                pane.configured,
                pane.rect,
                pane.scale,
                pane.active,
                self.popup,
                pane.layer.wl_surface().clone(),
            )
        };
        let Ok(region) = Region::new(&self.compositor) else { return };
        if active {
            if let (Some(logical), Some(popup)) = (logical, popup) {
                for part in input_region_rects(rect, scale, logical, popup) {
                    region.add(part.x, part.y, part.w, part.h);
                }
            }
        }
        surface.set_input_region(Some(region.wl_region()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_region_leaves_a_popup_sized_hole() {
        let got = input_region_rects(
            PhysRect { x: 0, y: 0, w: 300, h: 200 },
            2.0,
            (150, 100),
            PhysRect { x: 50, y: 40, w: 100, h: 40 },
        );
        assert_eq!(
            got,
            vec![
                RegionRect { x: 0, y: 0, w: 150, h: 20 },
                RegionRect { x: 0, y: 40, w: 150, h: 60 },
                RegionRect { x: 0, y: 20, w: 25, h: 20 },
                RegionRect { x: 75, y: 20, w: 75, h: 20 },
            ]
        );
    }

    #[test]
    fn an_offscreen_popup_leaves_the_full_surface_in_the_region() {
        assert_eq!(
            input_region_rects(
                PhysRect { x: 0, y: 0, w: 300, h: 200 },
                1.0,
                (300, 200),
                PhysRect { x: 500, y: 500, w: 20, h: 20 },
            ),
            vec![RegionRect { x: 0, y: 0, w: 300, h: 200 }]
        );
    }

    #[test]
    fn a_popup_that_covers_the_output_leaves_no_input_region() {
        assert!(input_region_rects(
            PhysRect { x: 0, y: 0, w: 300, h: 200 },
            1.0,
            (300, 200),
            PhysRect { x: 0, y: 0, w: 300, h: 200 },
        )
        .is_empty());
    }
}
