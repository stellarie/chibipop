//! Convert between CSS text and Theme values.
//!
//! This module has no Windows dependency.

use crate::ui::theme::Theme;

/// One diagnostic from the CSS parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CssError {
    pub line: usize,
    pub message: String,
}

/// One parsed CSS declaration.
#[derive(Debug, Clone, PartialEq)]
enum Decl {
    Accent((u8, u8, u8)),
    Color((u8, u8, u8)),
    FontSize(f32),
    FontFamily(String),
    FontWeight(u16),
    Italic(bool),
    BorderRadius(i32),
    BorderWidth(f32),
    Padding(i32),
    Height(f32),
    Opacity(f32),
}

/// CSS class selectors that this module supports.
const SELECTORS: &[&str] = &[
    ":root",
    "popup",
    "headword",
    "reading",
    "body",
    "dict-label",
    "collapsed",
    "dimmed",
    "frequency",
    "separator",
];

/// Parse a CSS color in `#rrggbb` or `#rgb` form.
fn parse_hex_color(s: &str) -> Option<(u8, u8, u8)> {
    let hex = s.strip_prefix('#')?;
    match hex.len() {
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some((r, g, b))
        }
        3 => {
            let r = u8::from_str_radix(&hex[0..1], 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()?;
            Some((r * 17, g * 17, b * 17))
        }
        _ => None,
    }
}

/// Parse a `12px` value as an `f32`.
fn parse_px_f32(s: &str) -> Option<f32> {
    let num = s.strip_suffix("px")?;
    num.trim().parse().ok()
}

/// Parse a `12px` value as an `i32`.
fn parse_px_i32(s: &str) -> Option<i32> {
    let num = s.strip_suffix("px")?;
    num.trim().parse().ok()
}

/// Parse a font family and remove quotes such as `"Noto Sans JP"`.
fn parse_font_family(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if let Some(inner) = trimmed.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return Some(inner.to_string());
    }
    if let Some(inner) = trimmed
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
    {
        return Some(inner.to_string());
    }
    Some(trimmed.to_string())
}

/// Map "bold" to 700 and "normal" to 400.
/// Accept integer values from 100 to 900.
fn parse_font_weight(s: &str) -> Option<u16> {
    match s.trim() {
        "normal" => Some(400),
        "bold" => Some(700),
        n => {
            let v: u16 = n.parse().ok()?;
            if (100..=900).contains(&v) {
                Some(v)
            } else {
                None
            }
        }
    }
}

/// Parse one CSS property value.
fn parse_property(name: &str, value: &str) -> Result<Option<Decl>, String> {
    let decl = match name {
        "--accent" => parse_hex_color(value)
            .map(Decl::Accent)
            .ok_or_else(|| format!("bad color: {value}"))?,
        "color" | "background-color" | "border-color" => parse_hex_color(value)
            .map(Decl::Color)
            .ok_or_else(|| format!("bad color: {value}"))?,
        "font-size" => parse_px_f32(value)
            .map(Decl::FontSize)
            .ok_or_else(|| format!("bad font-size: {value}"))?,
        "font-family" => parse_font_family(value)
            .map(Decl::FontFamily)
            .ok_or_else(|| format!("bad font-family: {value}"))?,
        "font-weight" => parse_font_weight(value)
            .map(Decl::FontWeight)
            .ok_or_else(|| format!("bad font-weight: {value}"))?,
        "font-style" => match value.trim() {
            "italic" => Decl::Italic(true),
            "normal" => Decl::Italic(false),
            _ => return Err(format!("bad font-style: {value}")),
        },
        "border-radius" => parse_px_i32(value)
            .map(Decl::BorderRadius)
            .ok_or_else(|| format!("bad border-radius: {value}"))?,
        "border-width" => parse_px_f32(value)
            .map(Decl::BorderWidth)
            .ok_or_else(|| format!("bad border-width: {value}"))?,
        "padding" => parse_px_i32(value)
            .map(Decl::Padding)
            .ok_or_else(|| format!("bad padding: {value}"))?,
        "height" => parse_px_f32(value)
            .map(Decl::Height)
            .ok_or_else(|| format!("bad height: {value}"))?,
        "opacity" => match value.trim().parse::<f32>() {
            Ok(o) if (0.0..=1.0).contains(&o) => Decl::Opacity(o),
            _ => return Err(format!("bad opacity: {value}")),
        },
        _ => return Ok(None),
    };
    Ok(Some(decl))
}

/// Remove `/* ... */` comments from CSS text.
fn strip_block_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("*/") {
            Some(end) => {
                let skipped = &rest[start..start + 2 + end + 2];
                out.extend(skipped.chars().map(|c| if c == '\n' { '\n' } else { ' ' }));
                rest = &rest[start + 2 + end + 2..];
            }
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Parse CSS text and apply supported properties to a Theme.
///
/// Return a diagnostic for each parse error.
pub fn parse(css: &str, base: &mut Theme) -> Vec<CssError> {
    let mut errors = Vec::new();
    let cleaned = strip_block_comments(css);

    let mut chars = cleaned.char_indices().peekable();
    let mut line_num = 1usize;

    while chars.peek().is_some() {
        skip_whitespace(&mut chars, &mut line_num);
        if chars.peek().is_none() {
            break;
        }

        let selector = read_selector(&mut chars, &mut line_num);
        let selector = selector.trim().to_string();
        if selector.is_empty() {
            break;
        }

        skip_whitespace(&mut chars, &mut line_num);
        match chars.peek() {
            Some(&(_, '{')) => {
                chars.next();
            }
            _ => {
                errors.push(CssError {
                    line: line_num,
                    message: "expected '{'".into(),
                });
                break;
            }
        }

        let class = selector.strip_prefix('.').unwrap_or(&selector);
        let known = SELECTORS.contains(&class);

        loop {
            skip_whitespace(&mut chars, &mut line_num);
            match chars.peek() {
                Some(&(_, '}')) => {
                    chars.next();
                    break;
                }
                None => break,
                _ => {}
            }

            let prop_line = line_num;
            let decl = read_until_semicolon_or_brace(&mut chars, &mut line_num);
            let decl = decl.trim();
            if decl.is_empty() {
                continue;
            }

            let Some((name, value)) = decl.split_once(':') else {
                errors.push(CssError {
                    line: prop_line,
                    message: format!("bad property: {decl}"),
                });
                continue;
            };
            let name = name.trim();
            let value = value.trim();

            if !known {
                continue;
            }

            match parse_property(name, value) {
                Ok(Some(decl)) => apply(class, name, decl, base),
                Ok(None) => {}
                Err(msg) => errors.push(CssError {
                    line: prop_line,
                    message: msg,
                }),
            }
        }
    }

    errors
}

/// The color, size, weight, and italic slots of one text selector.
type TextSlots<'a> = (&'a mut (u8, u8, u8), &'a mut f32, &'a mut u16, &'a mut bool);

/// Return the Theme slots for one text selector.
fn text_slots<'a>(theme: &'a mut Theme, class: &str) -> Option<TextSlots<'a>> {
    match class {
        "headword" => Some((
            &mut theme.headword_text,
            &mut theme.headword_size,
            &mut theme.headword_weight,
            &mut theme.headword_italic,
        )),
        "reading" => Some((
            &mut theme.reading_text,
            &mut theme.reading_size,
            &mut theme.reading_weight,
            &mut theme.reading_italic,
        )),
        "body" => Some((
            &mut theme.body_text,
            &mut theme.body_size,
            &mut theme.body_weight,
            &mut theme.body_italic,
        )),
        "dict-label" => Some((
            &mut theme.dict_label_text,
            &mut theme.dict_label_size,
            &mut theme.dict_label_weight,
            &mut theme.dict_label_italic,
        )),
        "collapsed" => Some((
            &mut theme.collapsed_text,
            &mut theme.collapsed_size,
            &mut theme.collapsed_weight,
            &mut theme.collapsed_italic,
        )),
        "dimmed" => Some((
            &mut theme.dimmed_text,
            &mut theme.dimmed_size,
            &mut theme.dimmed_weight,
            &mut theme.dimmed_italic,
        )),
        "frequency" => Some((
            &mut theme.frequency_text,
            &mut theme.frequency_size,
            &mut theme.frequency_weight,
            &mut theme.frequency_italic,
        )),
        _ => None,
    }
}

/// Apply one parsed CSS property to a Theme.
fn apply(class: &str, name: &str, decl: Decl, theme: &mut Theme) {
    if let Decl::Accent(c) = decl {
        theme.accent = c;
        return;
    }
    if class == "popup" {
        match (name, decl) {
            ("background-color", Decl::Color(c)) => theme.background = c,
            ("border-color", Decl::Color(c)) => theme.border = c,
            ("border-radius", Decl::BorderRadius(r)) => theme.corner_radius = r,
            ("border-width", Decl::BorderWidth(w)) => theme.border_width = w,
            ("padding", Decl::Padding(p)) => theme.padding = p,
            ("font-family", Decl::FontFamily(f)) => theme.font_name = f,
            ("opacity", Decl::Opacity(o)) => theme.opacity = o,
            _ => {}
        }
        return;
    }
    if class == "separator" {
        match decl {
            Decl::Color(c) => theme.separator = c,
            Decl::Height(h) => theme.separator_height = h,
            _ => {}
        }
        return;
    }
    if let Some((text, size, weight, italic)) = text_slots(theme, class) {
        match decl {
            Decl::Color(c) => *text = c,
            Decl::FontSize(s) => *size = s,
            Decl::FontWeight(w) => *weight = w,
            Decl::Italic(i) => *italic = i,
            _ => {}
        }
    }
}

/// Format a text weight for CSS output.
fn weight_str(w: u16) -> &'static str {
    match w {
        400 => "normal",
        700 => "bold",
        _ => "",
    }
}

/// Format a text weight. Use a number when no CSS name exists.
fn weight_css(w: u16) -> String {
    let s = weight_str(w);
    if s.is_empty() {
        w.to_string()
    } else {
        s.to_string()
    }
}

/// Serialize a Theme as CSS text.
pub fn to_css(theme: &Theme) -> String {
    let c = |rgb: (u8, u8, u8)| format!("#{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2);
    let style = |italic: bool| if italic { "italic" } else { "normal" };

    let lines = [
        "/* chibipop popup theme */",
        "/* Share this file to share your theme. */",
        "",
        ":root {",
        &format!("  --accent: {};", c(theme.accent)),
        "}",
        "",
        "/* Panel background, border, shape */",
        ".popup {",
        &format!("  background-color: {};", c(theme.background)),
        &format!("  border-color: {};", c(theme.border)),
        &format!("  border-width: {}px;", theme.border_width),
        &format!("  border-radius: {}px;", theme.corner_radius),
        &format!("  padding: {}px;", theme.padding),
        &format!("  font-family: \"{}\";", theme.font_name),
        &format!("  opacity: {};", theme.opacity),
        "}",
        "",
        "/* Main word */",
        ".headword {",
        &format!("  color: {};", c(theme.headword_text)),
        &format!("  font-size: {}px;", theme.headword_size),
        &format!("  font-weight: {};", weight_css(theme.headword_weight)),
        &format!("  font-style: {};", style(theme.headword_italic)),
        "}",
        "",
        "/* Kana reading */",
        ".reading {",
        &format!("  color: {};", c(theme.reading_text)),
        &format!("  font-size: {}px;", theme.reading_size),
        &format!("  font-weight: {};", weight_css(theme.reading_weight)),
        &format!("  font-style: {};", style(theme.reading_italic)),
        "}",
        "",
        "/* Definition text */",
        ".body {",
        &format!("  color: {};", c(theme.body_text)),
        &format!("  font-size: {}px;", theme.body_size),
        &format!("  font-weight: {};", weight_css(theme.body_weight)),
        &format!("  font-style: {};", style(theme.body_italic)),
        "}",
        "",
        "/* Dictionary name label */",
        ".dict-label {",
        &format!("  color: {};", c(theme.dict_label_text)),
        &format!("  font-size: {}px;", theme.dict_label_size),
        &format!("  font-weight: {};", weight_css(theme.dict_label_weight)),
        &format!("  font-style: {};", style(theme.dict_label_italic)),
        "}",
        "",
        "/* Other results (collapsed) */",
        ".collapsed {",
        &format!("  color: {};", c(theme.collapsed_text)),
        &format!("  font-size: {}px;", theme.collapsed_size),
        &format!("  font-weight: {};", weight_css(theme.collapsed_weight)),
        &format!("  font-style: {};", style(theme.collapsed_italic)),
        "}",
        "",
        "/* Frequency, POS tags */",
        ".dimmed {",
        &format!("  color: {};", c(theme.dimmed_text)),
        &format!("  font-size: {}px;", theme.dimmed_size),
        &format!("  font-weight: {};", weight_css(theme.dimmed_weight)),
        &format!("  font-style: {};", style(theme.dimmed_italic)),
        "}",
        "",
        "/* Frequency badge */",
        ".frequency {",
        &format!("  color: {};", c(theme.frequency_text)),
        &format!("  font-size: {}px;", theme.frequency_size),
        &format!("  font-weight: {};", weight_css(theme.frequency_weight)),
        &format!("  font-style: {};", style(theme.frequency_italic)),
        "}",
        "",
        "/* Horizontal rule */",
        ".separator {",
        &format!("  color: {};", c(theme.separator)),
        &format!("  height: {}px;", theme.separator_height),
        "}",
        "",
    ];
    lines.join("\r\n")
}

fn skip_whitespace(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>, line: &mut usize) {
    while let Some(&(_, c)) = chars.peek() {
        if c == '\n' {
            *line += 1;
            chars.next();
        } else if c.is_ascii_whitespace() {
            chars.next();
        } else {
            break;
        }
    }
}

fn read_selector(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    line: &mut usize,
) -> String {
    let mut out = String::new();
    while let Some(&(_, c)) = chars.peek() {
        if c == '{' {
            break;
        }
        if c == '\n' {
            *line += 1;
        }
        out.push(c);
        chars.next();
    }
    out
}

fn read_until_semicolon_or_brace(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    line: &mut usize,
) -> String {
    let mut out = String::new();
    while let Some(&(_, c)) = chars.peek() {
        if c == ';' {
            chars.next();
            break;
        }
        if c == '}' {
            break;
        }
        if c == '\n' {
            *line += 1;
        }
        out.push(c);
        chars.next();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dark() -> Theme {
        Theme::dark()
    }

    #[test]
    fn a_hex_color_parses() {
        assert_eq!(Some((255, 0, 128)), parse_hex_color("#ff0080"));
    }

    #[test]
    fn a_short_hex_color_expands() {
        assert_eq!(Some((255, 255, 255)), parse_hex_color("#fff"));
    }

    #[test]
    fn an_invalid_hex_returns_none() {
        assert_eq!(None, parse_hex_color("#xyz"));
        assert_eq!(None, parse_hex_color("red"));
        assert_eq!(None, parse_hex_color("#12345"));
    }

    #[test]
    fn px_values_parse() {
        assert_eq!(Some(20.0), parse_px_f32("20px"));
        assert_eq!(Some(15.5), parse_px_f32("15.5px"));
        assert_eq!(None, parse_px_f32("20em"));
        assert_eq!(None, parse_px_f32("abc"));
    }

    #[test]
    fn font_family_unquotes() {
        assert_eq!(
            Some("Noto Sans JP".into()),
            parse_font_family("\"Noto Sans JP\"")
        );
        assert_eq!(
            Some("Noto Sans JP".into()),
            parse_font_family("'Noto Sans JP'")
        );
        assert_eq!(Some("monospace".into()), parse_font_family("monospace"));
    }

    #[test]
    fn to_css_round_trips_through_parse() {
        let original = dark();
        let css = to_css(&original);
        let mut rebuilt = Theme::light();
        let errors = parse(&css, &mut rebuilt);
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(original.background, rebuilt.background);
        assert_eq!(original.border, rebuilt.border);
        assert_eq!(original.headword_text, rebuilt.headword_text);
        assert_eq!(original.reading_text, rebuilt.reading_text);
        assert_eq!(original.body_text, rebuilt.body_text);
        assert_eq!(original.dict_label_text, rebuilt.dict_label_text);
        assert_eq!(original.collapsed_text, rebuilt.collapsed_text);
        assert_eq!(original.dimmed_text, rebuilt.dimmed_text);
        assert_eq!(original.frequency_text, rebuilt.frequency_text);
        assert_eq!(original.separator, rebuilt.separator);
        assert_eq!(original.accent, rebuilt.accent);
        assert_eq!(original.font_name, rebuilt.font_name);
        assert_eq!(original.headword_size, rebuilt.headword_size);
        assert_eq!(original.body_size, rebuilt.body_size);
        assert_eq!(original.collapsed_size, rebuilt.collapsed_size);
        assert_eq!(original.frequency_size, rebuilt.frequency_size);
        assert_eq!(original.padding, rebuilt.padding);
        assert_eq!(original.corner_radius, rebuilt.corner_radius);
        assert_eq!(original.border_width, rebuilt.border_width);
        assert_eq!(original.opacity, rebuilt.opacity);
        assert_eq!(original.headword_weight, rebuilt.headword_weight);
        assert_eq!(original.headword_italic, rebuilt.headword_italic);
    }

    #[test]
    fn unknown_selectors_are_silently_ignored() {
        let mut theme = dark();
        let original = theme.clone();
        let errors = parse(".unknown { color: #ff0000; }", &mut theme);
        assert!(errors.is_empty());
        assert_eq!(original, theme);
    }

    #[test]
    fn unknown_properties_are_silently_ignored() {
        let mut theme = dark();
        let original_bg = theme.background;
        let errors = parse(
            ".popup { background-color: #112233; margin: 10px; }",
            &mut theme,
        );
        assert!(errors.is_empty());
        assert_eq!((0x11, 0x22, 0x33), theme.background);
        assert_ne!(original_bg, theme.background);
    }

    #[test]
    fn a_bad_color_reports_an_error() {
        let mut theme = dark();
        let errors = parse(".headword { color: notacolor; }", &mut theme);
        assert_eq!(1, errors.len());
        assert!(errors[0].message.contains("bad color"));
    }

    #[test]
    fn block_comments_are_stripped() {
        let mut theme = dark();
        let css = "/* a comment */ .headword { color: #ff0000; /* inline */ }";
        let errors = parse(css, &mut theme);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!((255, 0, 0), theme.headword_text);
    }

    #[test]
    fn multiple_selectors_parse() {
        let mut theme = dark();
        let css = ".headword { color: #aabbcc; }\n.reading { color: #112233; }";
        let errors = parse(css, &mut theme);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!((0xaa, 0xbb, 0xcc), theme.headword_text);
        assert_eq!((0x11, 0x22, 0x33), theme.reading_text);
    }

    #[test]
    fn popup_properties_apply_correctly() {
        let mut theme = dark();
        let css = concat!(
            ".popup {\n",
            "  background-color: #112233;\n",
            "  border-color: #445566;\n",
            "  border-radius: 8px;\n",
            "  border-width: 2px;\n",
            "  padding: 16px;\n",
            "  font-family: \"Noto Sans JP\";\n",
            "  opacity: 0.8;\n",
            "}\n",
        );
        let errors = parse(css, &mut theme);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!((0x11, 0x22, 0x33), theme.background);
        assert_eq!((0x44, 0x55, 0x66), theme.border);
        assert_eq!(8, theme.corner_radius);
        assert_eq!(2.0, theme.border_width);
        assert_eq!(16, theme.padding);
        assert_eq!("Noto Sans JP", theme.font_name);
        assert_eq!(0.8, theme.opacity);
    }

    #[test]
    fn font_size_applies_to_headword() {
        let mut theme = dark();
        let css = ".headword { font-size: 24px; }";
        let errors = parse(css, &mut theme);
        assert!(errors.is_empty());
        assert_eq!(24.0, theme.headword_size);
    }

    #[test]
    fn empty_css_produces_no_errors() {
        let mut theme = dark();
        let errors = parse("", &mut theme);
        assert!(errors.is_empty());
    }

    #[test]
    fn whitespace_only_produces_no_errors() {
        let mut theme = dark();
        let errors = parse("  \n  \n  ", &mut theme);
        assert!(errors.is_empty());
    }

    #[test]
    fn scan_colours_are_not_exposed_to_css() {
        let mut theme = dark();
        let original = theme.scan_match;
        let errors = parse(".scan-match { color: #ff0000; }", &mut theme);
        assert!(errors.is_empty());
        assert_eq!(original, theme.scan_match);
    }

    #[test]
    fn error_reports_the_line_number() {
        let mut theme = dark();
        let css = ".headword {\n  color: #ff0000;\n  font-size: bad;\n}";
        let errors = parse(css, &mut theme);
        assert_eq!(1, errors.len());
        assert_eq!(3, errors[0].line);
    }

    #[test]
    fn light_theme_round_trips() {
        let original = Theme::light();
        let css = to_css(&original);
        let mut rebuilt = Theme::dark();
        let errors = parse(&css, &mut rebuilt);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(original.background, rebuilt.background);
        assert_eq!(original.headword_text, rebuilt.headword_text);
        assert_eq!(original.accent, rebuilt.accent);
        assert_eq!(original.font_name, rebuilt.font_name);
    }

    #[test]
    fn css_preserves_scan_colours_from_base() {
        let mut theme = dark();
        let original_scan = theme.scan_match;
        let original_accent = theme.accent;
        let css = to_css(&theme);
        let errors = parse(&css, &mut theme);
        assert!(errors.is_empty());
        assert_eq!(original_scan, theme.scan_match);
        assert_eq!(original_accent, theme.accent);
    }

    #[test]
    fn accent_custom_property_applies() {
        let mut theme = dark();
        let errors = parse(":root { --accent: #123456; }", &mut theme);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!((0x12, 0x34, 0x56), theme.accent);
    }

    #[test]
    fn missing_brace_reports_an_error() {
        let mut theme = dark();
        let errors = parse(".headword color: #ff0000; }", &mut theme);
        assert!(!errors.is_empty());
    }

    #[test]
    fn frequency_selector_applies_color_and_size() {
        let mut theme = dark();
        let css = ".frequency { color: #aabb00; font-size: 11px; }";
        let errors = parse(css, &mut theme);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!((0xaa, 0xbb, 0x00), theme.frequency_text);
        assert_eq!(11.0, theme.frequency_size);
    }

    #[test]
    fn font_weight_bold_maps_to_700() {
        let mut theme = dark();
        let css = ".headword { font-weight: bold; }";
        let errors = parse(css, &mut theme);
        assert!(errors.is_empty());
        assert_eq!(700, theme.headword_weight);
    }

    #[test]
    fn font_weight_numeric_600() {
        let mut theme = dark();
        let css = ".headword { font-weight: 600; }";
        let errors = parse(css, &mut theme);
        assert!(errors.is_empty());
        assert_eq!(600, theme.headword_weight);
    }

    #[test]
    fn font_style_italic() {
        let mut theme = dark();
        let css = ".reading { font-style: italic; }";
        let errors = parse(css, &mut theme);
        assert!(errors.is_empty());
        assert!(theme.reading_italic);
    }

    #[test]
    fn opacity_applies() {
        let mut theme = dark();
        let css = ".popup { opacity: 0.75; }";
        let errors = parse(css, &mut theme);
        assert!(errors.is_empty());
        assert_eq!(0.75, theme.opacity);
    }

    #[test]
    fn border_width_applies() {
        let mut theme = dark();
        let css = ".popup { border-width: 2px; }";
        let errors = parse(css, &mut theme);
        assert!(errors.is_empty());
        assert_eq!(2.0, theme.border_width);
    }

    #[test]
    fn opacity_out_of_range_is_rejected() {
        let mut theme = dark();
        let errors = parse(".popup { opacity: 1.5; }", &mut theme);
        assert_eq!(1, errors.len());
        assert!(errors[0].message.contains("bad opacity"));
    }

    #[test]
    fn font_weight_out_of_range_is_rejected() {
        let mut theme = dark();
        let errors = parse(".headword { font-weight: 50; }", &mut theme);
        assert_eq!(1, errors.len());
        assert!(errors[0].message.contains("bad font-weight"));
    }

    #[test]
    fn font_style_garbage_is_rejected() {
        let mut theme = dark();
        let errors = parse(".reading { font-style: oblique; }", &mut theme);
        assert_eq!(1, errors.len());
        assert!(errors[0].message.contains("bad font-style"));
    }
}
