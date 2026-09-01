//! The popup (ADR-0004): one wlr-layer surface per output, painted by
//! hand with tiny-skia and cosmic-text.
//!
//! Nothing here is a toolkit. SCTK drives the surface and its shm
//! buffers ([`surface`]), core `layout` produces the measured
//! [`PopupScene`], [`text`] measures and rasterizes the runs in it and
//! [`paint`] fills everything else. The daemon owns one [`Popup`] and
//! executes `Command::ShowPopup`/`HidePopup` against it; the placement
//! it computes travels back as `Event::PopupPlaced`.
//!
//! Physical pixels are authoritative all the way down: the scene is
//! laid out at the output's *fractional* scale (`physical_theme`), the
//! buffer is that many device pixels, and only the surface's own
//! geometry - size, margins, `wp_viewport` destination - is derived
//! back into logical units ([`place`]). Nothing latches the scale.

mod demo;
mod paint;
mod place;
mod pointer;
mod surface;
mod text;
// `media.rs` is the seventh file here and is deliberately *not* declared
// from this module: the lib face owns it (`crate::lib.rs`, `#[path]`), so
// its tests can link against a real built database. Reach it as
// `chibipop_linux::media`.

pub use demo::{canned, Demo};
pub use place::{derive, Screen};
pub use pointer::{frame as pointer_frame, Interaction, Step};
pub use surface::{Placed, Popup, ShowRequest};
/// The name half of the Japanese-font probe, for the settings font
/// combo (ADR-0005). The popup itself asks the whole question through
/// [`surface::Popup`]'s engine.
pub use text::jp_capable;

use chibipop::ui::layout::StyledSpan;
use chibipop::ui::theme::Theme;

/// Panel opacity, matching Windows' `LWA_ALPHA` 230/255 exactly
/// (ADR-0004). Per-pixel here, so the antialiased corners fade with
/// it instead of being hard-clipped by a window region.
pub const PANEL_ALPHA: u8 = 230;

/// One run of text to rasterize.
///
/// The paint half of the text stack. Core's `TextMeasure` is the
/// measure half; the same engine implements both, so a run is never
/// wrapped one way and painted another.
///
/// It carries the same ordered styled spans the seam measured, not one
/// string, so a paragraph holding bold and normal text paints in the
/// wrap it was measured in (ADR-0013). Colour rides on each span, and
/// `shifts` says how far up each one sits off its line's baseline -
/// `verticalAlign`, already resolved by the scene.
#[derive(Debug, Clone, Copy)]
pub struct DrawRun<'a> {
    /// In reading order.
    pub spans: &'a [StyledSpan<'a>],
    /// One per span, up positive.
    /// Empty means every span sits on
    /// its line's own baseline.
    pub shifts: &'a [f32],
    /// The wrap width the scene measured this run at.
    pub max_w: f32,
    /// Top-left of the wrap box, in buffer pixels.
    pub origin: (f32, f32),
}

/// What the painter needs from the text stack.
///
/// `TextMeasure` is the seam core owns; this adds the one thing a bin
/// does with the same shaped run. It is a supertrait so the painter
/// can measure a label it centres itself (the Anki slot) through the
/// same borrow.
pub trait PanelText: chibipop::ui::layout::TextMeasure {
    /// Blend `run` into `target`, premultiplied.
    fn draw_run(&mut self, run: DrawRun<'_>, target: &mut tiny_skia::PixmapMut<'_>);
}

/// The theme in device pixels at `scale`.
///
/// Core layout carries one pixel space and no scale factor, so the
/// scale enters here, once: every length the theme carries - all seven
/// per-role font sizes, the padding, the corner radius, the rule and
/// the border - is the theme's logical value multiplied out. Colours,
/// weights, styles and the family are untouched: none of them is a
/// length.
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

/// One `Dispatch` per interface and user-data pair, forwarding to the
/// `Dispatch2` impl SCTK ships for that pair.
///
/// Why this instead of `delegate_dispatch2!`: SCTK 0.21's macro writes
/// a *blanket* `Dispatch<I, U>` impl, which would collide with the
/// hand-written impls the cursor channel already needs on this same
/// state (`daemon::App`). One `Dispatch` per interface and user-data
/// pair, each forwarding to SCTK's own `Dispatch2`, is the same code
/// the macro generates - only enumerated, so the two halves of the
/// daemon can share one Wayland queue. It lives in this module because
/// both dispatch halves of the popup - the surface's and the
/// pointer's - are written with it.
///
/// The expansion site provides `App`, `Dispatch`, `Dispatch2`, `Proxy`,
/// `Connection`, `QueueHandle` and `Arc`.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_physical_theme_scales_sizes_and_leaves_colours_alone() {
        let base = Theme::dark();
        let phys = physical_theme(&base, 1.5);
        assert_eq!(base.headword_size * 1.5, phys.headword_size);
        assert_eq!(base.body_size * 1.5, phys.body_size);
        assert_eq!(base.collapsed_size * 1.5, phys.collapsed_size);
        assert_eq!(18, phys.padding, "12 logical px at 1.5");
        assert_eq!(18, phys.corner_radius);
        assert_eq!(base.background, phys.background);
        assert_eq!(base.font_name, phys.font_name);
    }

    /// Every role's own size, and the
    /// two hairlines the theme carries:
    /// a length left logical here is a
    /// length painted too small on a
    /// scaled output.
    #[test]
    fn the_physical_theme_scales_every_length_the_theme_carries() {
        // Distinct values, so no field
        // can pass by borrowing another
        // role's number.
        let base = Theme {
            headword_size: 20.0,
            reading_size: 17.0,
            body_size: 15.0,
            dict_label_size: 13.0,
            collapsed_size: 12.0,
            dimmed_size: 11.0,
            frequency_size: 9.0,
            separator_height: 3.0,
            border_width: 2.0,
            ..Theme::dark()
        };
        let phys = physical_theme(&base, 2.0);
        assert_eq!(40.0, phys.headword_size);
        assert_eq!(34.0, phys.reading_size);
        assert_eq!(30.0, phys.body_size);
        assert_eq!(26.0, phys.dict_label_size);
        assert_eq!(24.0, phys.collapsed_size);
        assert_eq!(22.0, phys.dimmed_size);
        assert_eq!(18.0, phys.frequency_size);
        assert_eq!(6.0, phys.separator_height);
        assert_eq!(4.0, phys.border_width);
    }

    /// A weight is not a length.
    #[test]
    fn the_physical_theme_leaves_weight_and_style_alone() {
        let base = Theme { headword_weight: 700, reading_italic: true, ..Theme::dark() };
        let phys = physical_theme(&base, 1.5);
        assert_eq!(700, phys.headword_weight);
        assert!(phys.reading_italic);
    }

    #[test]
    fn scale_one_is_the_theme_itself() {
        let base = Theme::dark();
        assert_eq!(base, physical_theme(&base, 1.0));
    }
}
