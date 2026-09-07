use crate::popup::{paint, physical_theme, text::TextEngine};
use chibipop::config::Config;
use chibipop::present::Presentation;
use chibipop::ui::layout::{self, PopupScene, SceneRequest};
use chibipop::ui::theme::Theme;
use chibipop_linux::media::MediaSurfaces;
use iced::widget::image::Handle;
use iced::{Point, Size};
use std::path::Path;

pub struct Definition {
    pub presentation: Presentation,
    pub scene: PopupScene,
    pub image: Handle,
    pub size: Size,
    pub scale: f32,
    pub scroll: f32,
    pub hovered: Option<String>,
    pub generation: u64,
    pub pointer: Point,
}

impl Definition {
    pub fn new(presentation: Presentation, config: &Config, theme: &Theme,
        text: &mut TextEngine, media: Option<&mut MediaSurfaces>) -> anyhow::Result<Self> {
        let mut size = Size::new(620.0, 560.0);
        let scene = measure(&presentation, config, theme, text, size, 1.0)?;
        size.height = paint::surface_height(&scene).ceil().max(1.0);
        let mut definition = Self {
            presentation, scene, image: Handle::from_rgba(1, 1, vec![0; 4]),
            size, scale: 1.0, scroll: 0.0, hovered: None, generation: 0, pointer: Point::ORIGIN,
        };
        definition.paint(theme, text, media)?;
        Ok(definition)
    }

    pub fn resize(&mut self, config: &Config, theme: &Theme, text: &mut TextEngine,
        media: Option<&mut MediaSurfaces>, size: Size, scale: f32) -> anyhow::Result<()> {
        self.size = Size::new(size.width.clamp(120.0, 2048.0), size.height.clamp(80.0, 2048.0));
        self.scale = if scale.is_finite() { scale.clamp(0.5, 4.0) } else { 1.0 };
        self.scene = measure(&self.presentation, config, theme, text, self.size, self.scale)?;
        self.hovered = None;
        self.generation = self.generation.wrapping_add(1);
        self.paint(theme, text, media)
    }

    pub fn restyle(&mut self, config: &Config, theme: &Theme, text: &mut TextEngine,
        media: Option<&mut MediaSurfaces>) -> anyhow::Result<()> {
        let mut size = Size::new(self.size.width, 560.0);
        let scene = measure(&self.presentation, config, theme, text, size, self.scale)?;
        size.height = (paint::surface_height(&scene) / self.scale).ceil();
        self.resize(config, theme, text, media, size, self.scale)
    }

    pub fn hover(&mut self, point: Point, theme: &Theme, text: &mut TextEngine) -> Option<String> {
        self.scene.hover_query((point.x * self.scale, point.y * self.scale),
            self.scroll, &theme.font_name, text).ok().flatten()
    }

    pub fn click(&self) -> Option<chibipop::controller::HitAction> {
        let x = self.pointer.x * self.scale;
        let y = self.pointer.y * self.scale + self.scroll;
        self.scene.hit_targets().into_iter().find(|hit| {
            y >= hit.y && y < hit.y + hit.h
                && hit.x.zip(hit.w).is_none_or(|(left, width)| x >= left && x < left + width)
        }).map(|hit| hit.action)
    }

    pub fn paint(&mut self, theme: &Theme, text: &mut TextEngine,
        media: Option<&mut MediaSurfaces>) -> anyhow::Result<()> {
        let theme = physical_theme(theme, f64::from(self.scale));
        self.scroll = self.scroll.clamp(0.0, (self.scene.content_h - self.scene.view_h).max(0.0));
        let width = (self.size.width * self.scale).ceil() as u32;
        let height = (self.size.height * self.scale).ceil() as u32;
        let mut pixels = tiny_skia::Pixmap::new(width, height)
            .ok_or_else(|| anyhow::anyhow!("Definition image is too large"))?;
        paint::panel(&paint::Panel { scene: &self.scene, theme: &theme,
            scroll: self.scroll, scale: self.scale }, text, media, &mut pixels.as_mut());
        let bytes: Vec<u8> = pixels.pixels().iter().flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), themed_alpha(color.alpha(), theme.opacity)]
        }).collect();
        self.image = Handle::from_rgba(width, height, bytes);
        Ok(())
    }
}

fn measure(presentation: &Presentation, config: &Config, theme: &Theme,
    text: &mut TextEngine, size: Size, scale: f32) -> anyhow::Result<PopupScene> {
    Ok(layout::scene(&SceneRequest {
        presentation, theme: &physical_theme(theme, f64::from(scale)),
        max_w: size.width * scale, max_h: size.height * scale,
        show_back: true, side_panel: false, render: config.popup.render_settings(),
        anki: None, selection: None,
    }, text)?)
}

fn themed_alpha(alpha: u8, opacity: f32) -> u8 {
    let target = (opacity.clamp(0.0, 1.0) * 255.0).round() as u32;
    let base = u32::from(crate::popup::PANEL_ALPHA);
    ((u32::from(alpha) * target + base / 2) / base).min(255) as u8
}

fn theme_with_css(config: &Config, css: Option<&str>, text: &mut TextEngine) ->
    (Theme, Vec<chibipop::ui::css::CssError>) {
    let mut theme = if config.popup.theme == "light" { Theme::light() } else { Theme::dark() };
    theme.font_name.clone_from(&config.popup.font);
    theme.headword_weight = 700;
    theme.collapsed_italic = true;
    let errors = css.map_or_else(Vec::new, |css| chibipop::ui::css::parse(css, &mut theme));
    theme.font_name = chibipop::config::resolve_font(&theme.font_name,
        chibipop::config::Platform::Linux, |family| text.resolvable(family)).family().to_string();
    text.set_family(&theme.font_name);
    (theme, errors)
}

pub fn theme(config: &Config, css_path: Option<&Path>, text: &mut TextEngine) -> Theme {
    let css = css_path.and_then(|path| std::fs::read_to_string(path).ok());
    let (theme, errors) = theme_with_css(config, css.as_deref(), text);
    for error in errors {
        eprintln!("chibipop: popup.css:{}: {}", error.line, error.message);
    }
    theme
}

#[cfg(test)]
mod tests {
    use super::*;
    use chibipop::ui::layout::{ElemKind, MeasureRun, Measured, TextMeasure};

    #[test]
    fn native_scene_raster_hover_and_back_use_the_same_scaled_geometry() {
        let config = Config::default();
        let mut engine = TextEngine::new("Noto Sans CJK JP");
        let theme = theme(&config, None, &mut engine);
        let mut definition = Definition::new(crate::popup::canned(), &config, &theme, &mut engine, None).unwrap();
        for scale in [1.0, 1.5, 2.0] {
            definition.scroll = 0.0;
            definition.resize(&config, &theme, &mut engine, None, Size::new(620.0, 420.0), scale).unwrap();
            let elem = definition.scene.elems.iter().find(|elem| elem.kind == ElemKind::Headword).unwrap();
            let spans: Vec<_> = elem.styled_spans(&theme.font_name).collect();
            let run = MeasureRun { spans: &spans, max_w: elem.wrap_w };
            let mut measured = Measured::default();
            engine.measure(run, &mut measured).unwrap();
            let mut boxes = Vec::new();
            engine.caret_boxes(run, &[0], &mut boxes).unwrap();
            let glyph = boxes[0];
            let slack = (elem.wrap_w - measured.lines[0].w).max(0.0) * elem.align.slack_before();
            let point = Point::new((elem.pen.0 + slack + glyph.x + glyph.w / 2.0) / scale,
                (elem.pen.1 + glyph.y + glyph.h / 2.0) / scale);
            assert!(definition.hover(point, &theme, &mut engine).is_some_and(|query| query.starts_with('漢')));
            let back = definition.scene.hit_targets().into_iter().find(|hit|
                hit.action == chibipop::controller::HitAction::Back).unwrap();
            definition.pointer = Point::new((back.x.unwrap_or(0.0) + 1.0) / scale, (back.y + back.h / 2.0) / scale);
            assert_eq!(definition.click(), Some(chibipop::controller::HitAction::Back));
            definition.scroll = f32::MAX;
            definition.paint(&theme, &mut engine, None).unwrap();
            assert_eq!(definition.scroll, (definition.scene.content_h - definition.scene.view_h).max(0.0));
        }
    }

    #[test]
    fn search_defaults_keep_both_palettes_clear_and_emphasized() {
        for name in ["dark", "light"] {
            let mut config = Config::default();
            config.popup.theme = name.into();
            let mut engine = TextEngine::new("Noto Sans CJK JP");
            let (theme, errors) = theme_with_css(&config, None, &mut engine);
            assert!(errors.is_empty());
            assert_ne!(theme.background, theme.body_text);
            assert_ne!(theme.background, theme.border);
            assert!(theme.border_width > 0.0);
            assert_eq!(theme.headword_weight, 700);
            assert!(theme.collapsed_italic);
            assert!(!theme.body_italic);
            assert!(theme.headword_size > theme.body_size);
        }
    }

    #[test]
    fn css_maps_search_roles_and_overrides_emphasis_defaults() {
        let config = Config::default();
        let mut engine = TextEngine::new("Noto Sans CJK JP");
        let css = concat!(
            ".popup { background-color: #123456; border-color: #abcdef; ",
            "border-width: 3px; border-radius: 7px; padding: 9px; opacity: 0.5; }",
            ".headword { color: #fedcba; font-size: 24px; font-weight: normal; }",
            ".body { color: #112233; font-size: 17px; font-style: italic; }",
            ".collapsed { color: #778899; font-size: 13px; font-style: normal; }",
            ".reading { color: #445566; font-size: 14px; font-style: italic; }",
        );
        let (theme, errors) = theme_with_css(&config, Some(css), &mut engine);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(theme.background, (0x12, 0x34, 0x56));
        assert_eq!(theme.border, (0xab, 0xcd, 0xef));
        assert_eq!(theme.border_width, 3.0);
        assert_eq!(theme.corner_radius, 7);
        assert_eq!(theme.padding, 9);
        assert_eq!(theme.opacity, 0.5);
        assert_eq!(theme.headword_text, (0xfe, 0xdc, 0xba));
        assert_eq!(theme.headword_size, 24.0);
        assert_eq!(theme.headword_weight, 400);
        assert_eq!(theme.body_text, (0x11, 0x22, 0x33));
        assert_eq!(theme.body_size, 17.0);
        assert!(theme.body_italic);
        assert_eq!(theme.collapsed_text, (0x77, 0x88, 0x99));
        assert_eq!(theme.collapsed_size, 13.0);
        assert!(!theme.collapsed_italic);
        assert!(theme.reading_italic);
    }

    #[test]
    fn definition_alpha_tracks_css_opacity() {
        assert_eq!(themed_alpha(crate::popup::PANEL_ALPHA, 0.9), crate::popup::PANEL_ALPHA);
        assert_eq!(themed_alpha(crate::popup::PANEL_ALPHA, 0.5), 128);
        assert_eq!(themed_alpha(crate::popup::PANEL_ALPHA, 0.0), 0);
    }

    #[test]
    fn larger_css_remeasures_definition_height_at_each_scale() {
        let config = Config::default();
        let mut engine = TextEngine::new("Noto Sans CJK JP");
        let base = theme(&config, None, &mut engine);
        let mut larger = base.clone();
        assert!(chibipop::ui::css::parse(
            ".popup { padding: 24px; } .body { font-size: 28px; } .headword { font-size: 36px; }",
            &mut larger,
        ).is_empty());
        for scale in [1.0, 1.5, 2.0] {
            let mut presentation = crate::popup::canned();
            presentation.collapsed.clear();
            presentation.top.as_mut().unwrap().blocks.truncate(1);
            let mut definition = Definition::new(presentation, &config, &base, &mut engine, None).unwrap();
            definition.scale = scale;
            definition.restyle(&config, &base, &mut engine, None).unwrap();
            let old_height = definition.size.height;
            definition.restyle(&config, &larger, &mut engine, None).unwrap();
            assert!(definition.size.height > old_height);
            assert!(definition.size.height <= 560.0);
            assert!(definition.scene.view_h > 0.0);
        }
    }
}
