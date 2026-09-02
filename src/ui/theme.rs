//! Colors and sizes for the popup.
//!
//! This module stays platform-neutral by design.

/// The scrollbar track width uses layout units.
pub const SCROLLBAR_W: i32 = 4;

/// The minimum scrollbar thumb height uses layout units.
pub const SCROLLBAR_MIN_THUMB: i32 = 16;

/// Complete appearance for one theme.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// Opaque RGB color that fills the panel.
    pub background: (u8, u8, u8),
    /// RGB color for the panel edge.
    pub border: (u8, u8, u8),
    /// RGB color for card and row separators.
    pub separator: (u8, u8, u8),

    /// RGB color for the headword in the card.
    pub headword_text: (u8, u8, u8),
    /// RGB color for the reading in the card.
    pub reading_text: (u8, u8, u8),
    /// RGB color for Gloss text in the top card.
    pub body_text: (u8, u8, u8),
    /// RGB color for the Dictionary name label.
    pub dict_label_text: (u8, u8, u8),
    /// RGB color for collapsed rows.
    pub collapsed_text: (u8, u8, u8),
    /// RGB color for frequency, POS, and the `…` marker.
    pub dimmed_text: (u8, u8, u8),
    /// RGB color for the frequency badge.
    pub frequency_text: (u8, u8, u8),

    /// Font family. Both themes use Yu Gothic UI.
    pub font_name: String,
    /// Font size for the card headword.
    pub headword_size: f32,
    /// Font size for the Kana reading line.
    pub reading_size: f32,
    /// Font size for reading and gloss text.
    pub body_size: f32,
    /// Font size for the Dictionary name label.
    pub dict_label_size: f32,
    /// Font size for collapsed rows and metadata.
    pub collapsed_size: f32,
    /// Font size for frequency and POS tags.
    pub dimmed_size: f32,
    /// Font size for the frequency badge.
    pub frequency_size: f32,

    /// DirectWrite weight from 100 through 900.
    pub headword_weight: u16,
    pub reading_weight: u16,
    pub body_weight: u16,
    pub dict_label_weight: u16,
    pub collapsed_weight: u16,
    pub dimmed_weight: u16,
    pub frequency_weight: u16,

    /// DirectWrite italic flag.
    pub headword_italic: bool,
    pub reading_italic: bool,
    pub body_italic: bool,
    pub dict_label_italic: bool,
    pub collapsed_italic: bool,
    pub dimmed_italic: bool,
    pub frequency_italic: bool,

    /// The inner panel padding uses layout units.
    pub padding: i32,
    /// Corner radius. It must match `window::CORNER_RADIUS`.
    pub corner_radius: i32,
    /// The horizontal rule thickness uses layout units.
    pub separator_height: f32,
    /// The panel border stroke width uses layout units.
    pub border_width: f32,
    /// Window opacity from 0.0 through 1.0.
    pub opacity: f32,

    /// Overlay color for the pass-1 capture box.
    pub scan_pass1: (u8, u8, u8),
    /// Overlay color for a forward tile.
    pub scan_tile: (u8, u8, u8),
    /// Overlay color for the resolved word.
    pub scan_anchor: (u8, u8, u8),
    /// Overlay color for the defined chars.
    pub scan_match: (u8, u8, u8),
}

impl Theme {
    /// Default dark theme from spec M3-D6.
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
            frequency_text: (110, 112, 120),
            font_name: "Yu Gothic UI".to_string(),
            headword_size: 20.0,
            reading_size: 15.0,
            body_size: 15.0,
            dict_label_size: 13.0,
            collapsed_size: 13.0,
            dimmed_size: 13.0,
            frequency_size: 13.0,
            headword_weight: 400,
            reading_weight: 400,
            body_weight: 400,
            dict_label_weight: 400,
            collapsed_weight: 400,
            dimmed_weight: 400,
            frequency_weight: 400,
            headword_italic: false,
            reading_italic: false,
            body_italic: false,
            dict_label_italic: false,
            collapsed_italic: false,
            dimmed_italic: false,
            frequency_italic: false,
            padding: 12,
            corner_radius: 12,
            separator_height: 1.0,
            border_width: 1.0,
            opacity: 0.9,
            scan_pass1: (110, 150, 200),
            scan_tile: (240, 160, 50),
            scan_anchor: (255, 240, 120),
            scan_match: (80, 190, 255),
        }
    }

    /// TOML can override this theme.
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
            frequency_text: (150, 152, 158),
            font_name: "Yu Gothic UI".to_string(),
            headword_size: 20.0,
            reading_size: 15.0,
            body_size: 15.0,
            dict_label_size: 13.0,
            collapsed_size: 13.0,
            dimmed_size: 13.0,
            frequency_size: 13.0,
            headword_weight: 400,
            reading_weight: 400,
            body_weight: 400,
            dict_label_weight: 400,
            collapsed_weight: 400,
            dimmed_weight: 400,
            frequency_weight: 400,
            headword_italic: false,
            reading_italic: false,
            body_italic: false,
            dict_label_italic: false,
            collapsed_italic: false,
            dimmed_italic: false,
            frequency_italic: false,
            padding: 12,
            corner_radius: 12,
            separator_height: 1.0,
            border_width: 1.0,
            opacity: 0.9,
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

    /// Each scan outline color must differ from every other color in both themes.
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

    /// The match highlight must be brighter than the pass-1 box.
    #[test]
    fn the_match_highlight_is_brighter_than_the_pass1_box_in_dark_theme() {
        let t = Theme::dark();
        let sum = |c: (u8, u8, u8)| u32::from(c.0) + u32::from(c.1) + u32::from(c.2);
        assert!(
            sum(t.scan_match) > sum(t.scan_pass1),
            "scan_match {:?} must outshine scan_pass1 {:?}",
            t.scan_match,
            t.scan_pass1
        );
    }
}
