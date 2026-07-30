//! Popup colours and sizes.
//!
//! Windows-free by design.

/// Track and thumb width, px.
pub const SCROLLBAR_W: i32 = 4;

/// Shortest the thumb may get.
pub const SCROLLBAR_MIN_THUMB: i32 = 16;

/// One scheme's whole look.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// Panel fill. Opaque RGB.
    pub background: (u8, u8, u8),
    /// Panel edge.
    pub border: (u8, u8, u8),
    /// Card/rows rule.
    pub separator: (u8, u8, u8),

    /// The card's headword.
    pub headword_text: (u8, u8, u8),
    /// The card's reading.
    pub reading_text: (u8, u8, u8),
    /// Gloss text in the top card.
    pub body_text: (u8, u8, u8),
    /// Dictionary-name label.
    pub dict_label_text: (u8, u8, u8),
    /// Collapsed rows.
    pub collapsed_text: (u8, u8, u8),
    /// Frequency, POS, `…` marker.
    pub dimmed_text: (u8, u8, u8),

    /// Yu Gothic UI in both themes.
    pub font_name: String,
    /// The top card's headword.
    pub headword_size: f32,
    /// Reading and glosses.
    pub body_size: f32,
    /// Collapsed rows and metadata.
    pub collapsed_size: f32,

    /// Inner panel padding, px.
    pub padding: i32,
    /// Match window::CORNER_RADIUS.
    pub corner_radius: i32,

    /// Overlay: pass-1 capture box.
    pub scan_pass1: (u8, u8, u8),
    /// Overlay: a forward tile.
    pub scan_tile: (u8, u8, u8),
    /// Overlay: the resolved word.
    pub scan_anchor: (u8, u8, u8),
    /// Overlay: the defined chars.
    pub scan_match: (u8, u8, u8),
}

impl Theme {
    /// The default (spec M3-D6).
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

    /// Overridable in the TOML.
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

    /// Adjacent outlines must differ.
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

    /// Highlight outshines pass-1.
    #[test]
    fn the_match_highlight_is_brighter_than_the_pass1_box_in_dark_theme() {
        let t = Theme::dark();
        let sum = |c: (u8, u8, u8)| u32::from(c.0) + u32::from(c.1) + u32::from(c.2);
        assert!(sum(t.scan_match) > sum(t.scan_pass1),
                "scan_match {:?} must outshine scan_pass1 {:?}", t.scan_match, t.scan_pass1);
    }
}
