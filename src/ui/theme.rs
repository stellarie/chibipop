//! Colours, sizes and layout constants for the popup - the visual half of
//! `present.rs`'s decisions about what to show.
//!
//! Deliberately Windows-free (the M3 plan's hard rule extends it to this
//! file specifically): colours are plain `(u8, u8, u8)` RGB triples, not a
//! D2D or Win32 colour type, so this file compiles and tests on any
//! platform. `src/ui/render.rs` (M3 Task 5) is where a `Theme`'s colours
//! get converted to `D2D1_COLOR_F` - at the point they are actually used to
//! paint, not here.

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
    /// Gloss text, POS and frequency in the top card.
    pub body_text: (u8, u8, u8),
    /// A `GlossBlock`'s dictionary-name label.
    pub dict_label_text: (u8, u8, u8),
    /// Collapsed rows below the top card.
    pub collapsed_text: (u8, u8, u8),
    /// The trailing `…` marker drawn when content was clamped (M3-D4).
    pub dimmed_text: (u8, u8, u8),

    /// Yu Gothic UI in both themes, per spec §4.2/§4.3 - already present on
    /// the target machine and the face the OCR fixtures were rendered in.
    pub font_name: String,
    /// The top card's headword.
    pub headword_size: f32,
    /// Everything else in the top card: reading, POS, frequency, glosses.
    pub body_size: f32,
    /// Collapsed rows - deliberately smaller than the card (render.rs's
    /// spec'd requirement that collapsed rows read as visibly smaller).
    pub collapsed_size: f32,

    /// Inner panel padding, in physical pixels.
    pub padding: i32,
    /// Must match `ui::window::CORNER_RADIUS` (12): this is the radius the
    /// D2D-painted background rounds itself to, while the window constant
    /// is what `SetWindowRgn` clips the whole window to. A mismatch would
    /// show as background poking past the silhouette, or a visible gap
    /// between the two.
    pub corner_radius: i32,
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
        }
    }
}
