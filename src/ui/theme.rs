//! Colours, sizes and layout constants for the popup - the visual half of
//! `present.rs`'s decisions about what to show.
//!
//! Deliberately Windows-free (the M3 plan's hard rule extends it to this
//! file specifically): colours are plain `(u8, u8, u8)` RGB triples, not a
//! D2D or Win32 colour type, so this file compiles and tests on any
//! platform. `src/ui/render.rs` (M3 Task 5) is where a `Theme`'s colours
//! get converted to `D2D1_COLOR_F` - at the point they are actually used to
//! paint, not here.

/// Width of the scrollbar track and thumb, in physical pixels.
pub const SCROLLBAR_W: i32 = 4;

/// Shortest the thumb may get, so a very long entry still shows a thumb rather
/// than a one-pixel sliver.
pub const SCROLLBAR_MIN_THUMB: i32 = 16;

/// Everything the renderer needs to know about how the popup looks, for one
/// colour scheme. `dark()` and `light()` each populate every field - Rust's
/// struct-literal rules make an incomplete variant a compile error rather
/// than something that has to be checked by hand.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// Panel fill. The window's own constant alpha
    /// (`ui::window::LAYERED_ALPHA`) applies uniformly on top of this at the
    /// OS layer, so this is opaque RGB, not a translucent colour - the
    /// theme does not manage its own alpha channel.
    pub background: (u8, u8, u8),
    /// Panel edge, drawn just inside the rounded silhouette.
    pub border: (u8, u8, u8),
    /// The rule between the top card and the collapsed rows.
    pub separator: (u8, u8, u8),

    /// The top card's headword (e.g. `昨日`).
    pub headword_text: (u8, u8, u8),
    /// The top card's reading (e.g. `きのう`).
    pub reading_text: (u8, u8, u8),
    /// Gloss text in the top card.
    pub body_text: (u8, u8, u8),
    /// A `GlossBlock`'s dictionary-name label.
    pub dict_label_text: (u8, u8, u8),
    /// Collapsed rows below the top card.
    pub collapsed_text: (u8, u8, u8),
    /// De-emphasised metadata: the frequency in the card's top-right
    /// corner, the POS line, and the trailing `…` marker drawn when
    /// content was clamped (M3-D4).
    pub dimmed_text: (u8, u8, u8),

    /// Yu Gothic UI in both themes, per spec §4.2/§4.3 - already present on
    /// the target machine and the face the OCR fixtures were rendered in.
    pub font_name: String,
    /// The top card's headword.
    pub headword_size: f32,
    /// Everything else in the top card: reading and glosses.
    pub body_size: f32,
    /// Collapsed rows - deliberately smaller than the card (render.rs's
    /// spec'd requirement that collapsed rows read as visibly smaller) -
    /// and the card's own de-emphasised metadata, POS and frequency.
    pub collapsed_size: f32,

    /// Inner panel padding, in physical pixels.
    pub padding: i32,
    /// Must match `ui::window::CORNER_RADIUS` (12): this is the radius the
    /// D2D-painted background rounds itself to, while the window constant
    /// is what `SetWindowRgn` clips the whole window to. A mismatch would
    /// show as background poking past the silhouette, or a visible gap
    /// between the two.
    pub corner_radius: i32,

    /// `ui::overlay`'s outline colour for a pass-1 capture box - the
    /// cursor-centred box drawn on every hover, so it is deliberately the
    /// dimmest of the three scan colours.
    pub scan_pass1: (u8, u8, u8),
    /// `ui::overlay`'s outline colour for a forward tile - the accent hue,
    /// between `scan_pass1` and `scan_anchor`.
    pub scan_tile: (u8, u8, u8),
    /// `ui::overlay`'s outline colour for the resolved word's anchor box -
    /// the brightest of the three, since it marks the answer the other two
    /// exist to find.
    pub scan_anchor: (u8, u8, u8),
    /// `ui::overlay`'s outline colour for the characters the popup is
    /// defining.
    ///
    /// Deliberately a *brighter, more saturated* blue than `scan_pass1`
    /// rather than merely a different hue: this is the everyday case, drawn
    /// on every hover, while the three capture colours are the debug case.
    /// The one the user sees constantly should read first.
    pub scan_match: (u8, u8, u8),
}

impl Theme {
    /// The default (spec M3-D6): manga, visual novels and terminals are
    /// mostly dark reading contexts, and a light popup flashing over dark
    /// content at night is worse than the reverse.
    pub fn dark() -> Theme {
        Theme {
            background: (24, 24, 28),
            border: (60, 60, 68),
            separator: (50, 50, 56),
            headword_text: (240, 240, 245),
            reading_text: (170, 175, 185),
            body_text: (210, 212, 218),
            dict_label_text: (130, 170, 220),
            collapsed_text: (150, 152, 160),
            dimmed_text: (110, 112, 120),
            font_name: "Yu Gothic UI".to_string(),
            headword_size: 20.0,
            body_size: 15.0,
            collapsed_size: 13.0,
            padding: 12,
            corner_radius: 12,
            scan_pass1: (110, 150, 200),
            scan_tile: (240, 160, 50),
            scan_anchor: (255, 240, 120),
            scan_match: (80, 190, 255),
        }
    }

    /// Overridable in the TOML (spec §4.3). Not the default - see `dark()`.
    pub fn light() -> Theme {
        Theme {
            background: (250, 250, 252),
            border: (210, 210, 215),
            separator: (225, 225, 230),
            headword_text: (20, 20, 24),
            reading_text: (90, 92, 100),
            body_text: (40, 42, 48),
            dict_label_text: (30, 90, 160),
            collapsed_text: (100, 102, 110),
            dimmed_text: (150, 152, 158),
            font_name: "Yu Gothic UI".to_string(),
            headword_size: 20.0,
            body_size: 15.0,
            collapsed_size: 13.0,
            padding: 12,
            corner_radius: 12,
            scan_pass1: (70, 100, 150),
            scan_tile: (210, 110, 20),
            scan_anchor: (200, 20, 20),
            scan_match: (0, 120, 255),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Four outlines drawn at one alpha over arbitrary screen content are
    /// only readable if the colours differ from each other. Pairwise, since
    /// any two of them can end up adjacent.
    #[test]
    fn scan_colours_are_distinct_in_both_themes() {
        for t in [Theme::dark(), Theme::light()] {
            let all = [
                ("pass1", t.scan_pass1),
                ("tile", t.scan_tile),
                ("anchor", t.scan_anchor),
                ("match", t.scan_match),
            ];
            for (i, (an, a)) in all.iter().enumerate() {
                for (bn, b) in all.iter().skip(i + 1) {
                    assert_ne!(a, b, "scan_{an} and scan_{bn} are the same colour");
                }
            }
        }
    }

    /// D6: the highlight is the everyday case and the pass-1 box is the debug
    /// case, so the highlight must read as the brighter of the two blues
    /// rather than merely a different one.
    #[test]
    fn the_match_highlight_is_brighter_than_the_pass1_box_in_dark_theme() {
        let t = Theme::dark();
        let sum = |c: (u8, u8, u8)| u32::from(c.0) + u32::from(c.1) + u32::from(c.2);
        assert!(sum(t.scan_match) > sum(t.scan_pass1),
                "scan_match {:?} must outshine scan_pass1 {:?}", t.scan_match, t.scan_pass1);
    }
}
