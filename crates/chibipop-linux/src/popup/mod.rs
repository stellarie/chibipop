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
mod surface;
mod text;

pub use demo::{canned, Demo};
pub use surface::{Placed, Popup, ShowRequest};

use chibipop::ui::layout::Rgb;
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
#[derive(Debug, Clone, Copy)]
pub struct DrawRun<'a> {
    pub text: &'a str,
    /// Physical pixels, already scaled.
    pub size: f32,
    /// The wrap width the scene measured this run at.
    pub max_w: f32,
    pub color: Rgb,
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
/// scale enters here, once: font sizes, padding and the corner radius
/// are the theme's logical values multiplied out. Colours and the
/// family are untouched.
pub fn physical_theme(theme: &Theme, scale: f64) -> Theme {
    let s = scale as f32;
    Theme {
        headword_size: theme.headword_size * s,
        body_size: theme.body_size * s,
        collapsed_size: theme.collapsed_size * s,
        padding: ((theme.padding as f64) * scale).round() as i32,
        corner_radius: ((theme.corner_radius as f64) * scale).round() as i32,
        ..theme.clone()
    }
}

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

    #[test]
    fn scale_one_is_the_theme_itself() {
        let base = Theme::dark();
        assert_eq!(base, physical_theme(&base, 1.0));
    }
}
