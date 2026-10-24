//! The popup uses one wlr-layer surface per output.
//! tiny-skia and cosmic-text paint the surface.
//!
//! This module is not a toolkit.
//! SCTK drives the surface and its shm buffers ([`surface`]).
//! Core `layout` creates the measured [`PopupScene`].
//! [`text`] measures and rasterizes the runs in that scene.
//! [`paint`] fills all other pixels.
//! The daemon owns one [`Popup`].
//! It applies `Command::ShowPopup` and `Command::HidePopup` to that popup.
//! The placement result appears as `Event::PopupPlaced`.
//!
//! Physical pixels remain authoritative throughout this path.
//! The scene uses the output's *fractional* scale through `physical_theme`.
//! The buffer uses that scale in device pixels.
//! The surface converts only its own geometry to logical units through [`place`].
//! That geometry includes size, margins, and `wp_viewport` destination.
//! Nothing stores the scale for later use.

mod demo;
pub(crate) mod paint;
mod place;
mod pointer;
mod surface;
pub(crate) mod text;
// `media.rs` is the seventh file in this module.
// `popup/mod.rs` does not declare it.
// The library entry point owns it (`crate::lib.rs`, `#[path]`), so its tests can
// link against a built database.
// Reach it as `chibipop_linux::media`.

pub use demo::{canned, Demo};
pub use place::{derive, Screen};
pub use pointer::{frame as pointer_frame, Interaction, Step};
pub use surface::{Placed, Popup, ShowRequest};
/// Reexports the name half of the Japanese-font probe for the settings font
/// combo (ARCHITECTURE.md#settings-and-config).
/// The popup asks the full probe through [`surface::Popup`]'s engine.
pub use text::jp_capable;

use chibipop::ui::layout::StyledSpan;
use chibipop::ui::theme::Theme;

/// The panel alpha matches Windows' `LWA_ALPHA` value of 230/255.
/// Linux applies this fixed alpha to every pixel.
/// Antialiased corners therefore fade instead of a window region that clips them.
pub const PANEL_ALPHA: u8 = 230;

/// Defines one text run for rasterization.
///
/// This is the paint half of the text stack.
/// Core's `TextMeasure` is the measure half.
/// The same engine implements both halves, so it wraps and paints a run the same way.
///
/// It carries the ordered styled spans that the seam measured, not one text value.
/// A paragraph with bold and normal text therefore uses the measured wrap
/// (ARCHITECTURE.md#popup-and-measurement).
/// Each span carries its color.
/// `shifts` gives the upward distance for each span from its line baseline.
/// `verticalAlign` already resolves these offsets in the scene.
#[derive(Debug, Clone, Copy)]
pub struct DrawRun<'a> {
    /// The spans appear in text order.
    pub spans: &'a [StyledSpan<'a>],
    /// Stores one offset per span, with positive values above the baseline.
    /// An empty slice means every span uses its line baseline.
    pub shifts: &'a [f32],
    /// Stores the wrap width that the scene measured for this run.
    pub max_w: f32,
    /// Stores the wrap-box origin in buffer pixels.
    pub origin: (f32, f32),
}

/// Defines what the painter needs from the text stack.
///
/// `TextMeasure` is the seam that Core owns.
/// This trait adds one operation that a bin uses with the same shaped run.
/// It is a supertrait, so the painter can measure the Anki label that it centers
/// through the same borrow.
pub trait PanelText: chibipop::ui::layout::TextMeasure {
    /// Blend `run` into `target`, premultiplied.
    fn draw_run(&mut self, run: DrawRun<'_>, target: &mut tiny_skia::PixmapMut<'_>);
}

/// Returns the theme in device pixels at `scale`.
///
/// Core layout uses one pixel space and no scale factor.
/// This function applies the scale once to each theme length.
/// It scales all seven per-role font sizes, the padding, corner radius, rule, and border.
/// It does not change colors, weights, styles, or the family because none of them is a length.
pub fn physical_theme(theme: &Theme, scale: f64) -> Theme {
    let s = scale as f32;
    Theme {
        headword_size: theme.headword_size * s,
        reading_size: theme.reading_size * s,
        body_size: theme.body_size * s,
        dict_label_size: theme.dict_label_size * s,
        collapsed_size: theme.collapsed_size * s,
        dimmed_size: theme.dimmed_size * s,
        frequency_size: theme.frequency_size * s,
        separator_height: theme.separator_height * s,
        border_width: theme.border_width * s,
        padding: ((theme.padding as f64) * scale).round() as i32,
        corner_radius: ((theme.corner_radius as f64) * scale).round() as i32,
        ..theme.clone()
    }
}

/// Defines one `Dispatch` implementation for each interface and user-data pair.
/// Each implementation forwards events to the `Dispatch2` implementation that SCTK
/// provides for that pair.
///
/// This macro provides the form that `delegate_dispatch2!` cannot provide.
/// SCTK 0.21's macro writes a *blanket* `Dispatch<I, U>` implementation.
/// That implementation would collide with the hand-written implementations that
/// the cursor channel needs on `daemon::App`.
/// One implementation per pair matches the generated code and lets both popup
/// dispatch paths share one Wayland queue.
/// This module uses it for the surface path and the pointer path.
///
/// The expansion site provides `App`, `Dispatch`, `Dispatch2`, `Proxy`,
/// `Connection`, `QueueHandle`, and `Arc`.
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

pub(crate) use forward;
