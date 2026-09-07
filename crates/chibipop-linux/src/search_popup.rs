use crate::popup::{paint, physical_theme, text::TextEngine};
use chibipop::config::Config;
use chibipop::present::Presentation;
use chibipop::ui::layout::{self, PopupScene, SceneRequest};
use chibipop::ui::theme::Theme;
use chibipop_linux::media::MediaSurfaces;
use iced::widget::image::Handle;
use iced::{Point, Size};

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
            [color.red(), color.green(), color.blue(), color.alpha()]
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

pub fn theme(config: &Config, text: &mut TextEngine) -> Theme {
    let mut theme = if config.popup.theme == "light" { Theme::light() } else { Theme::dark() };
    theme.font_name = chibipop::config::resolve_font(&config.popup.font,
        chibipop::config::Platform::Linux, |family| text.resolvable(family)).family().to_string();
    text.set_family(&theme.font_name);
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
        let theme = theme(&config, &mut engine);
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
}
