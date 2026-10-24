use crate::popup::{paint, physical_theme, text::TextEngine};
use chibipop::config::{ProfileSession, ResolvedConfig};
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
    pub(crate) pointer_left: bool,
    pub session: ProfileSession,
    theme: Theme,
}

impl Definition {
    pub fn new(presentation: Presentation, session: ProfileSession, theme: Theme,
        text: &mut TextEngine, media: Option<&mut MediaSurfaces>) -> anyhow::Result<Self> {
        let mut size = Size::new(620.0, 560.0);
        let scene = measure(&presentation, session.config(), &theme, text, size, 1.0)?;
        size.height = paint::surface_height(&scene).ceil().max(1.0);
        let mut definition = Self {
            presentation, scene, image: Handle::from_rgba(1, 1, vec![0; 4]),
            size, scale: 1.0, scroll: 0.0, hovered: None, generation: 0,
            pointer: Point::ORIGIN, pointer_left: false, session, theme,
        };
        definition.paint(text, media)?;
        Ok(definition)
    }

    pub fn resize(&mut self, text: &mut TextEngine, media: Option<&mut MediaSurfaces>,
        size: Size, scale: f32) -> anyhow::Result<()> {
        self.size = Size::new(size.width.clamp(120.0, 2048.0), size.height.clamp(80.0, 2048.0));
        self.scale = if scale.is_finite() { scale.clamp(0.5, 4.0) } else { 1.0 };
        self.scene = measure(&self.presentation, self.session.config(), &self.theme, text, self.size, self.scale)?;
        self.hovered = None;
        self.generation = self.generation.wrapping_add(1);
        self.paint(text, media)
    }

    pub fn hover(&mut self, point: Point, text: &mut TextEngine) -> Option<String> {
        self.scene.hover_query((point.x * self.scale, point.y * self.scale),
            self.scroll, &self.theme.font_name, text).ok().flatten()
    }
    pub fn body_size(&self) -> f32 { self.theme.body_size }


    pub fn click(&self) -> Option<chibipop::controller::HitAction> {
        let x = self.pointer.x * self.scale;
        let y = self.pointer.y * self.scale + self.scroll;
        self.scene.hit_targets().into_iter().find(|hit| {
            y >= hit.y && y < hit.y + hit.h
                && hit.x.zip(hit.w).is_none_or(|(left, width)| x >= left && x < left + width)
        }).map(|hit| hit.action)
    }

    pub fn paint(&mut self, text: &mut TextEngine,
        media: Option<&mut MediaSurfaces>) -> anyhow::Result<()> {
        let theme = physical_theme(&self.theme, f64::from(self.scale));
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

fn measure(presentation: &Presentation, config: &ResolvedConfig, theme: &Theme,
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

fn theme_with_css(config: &ResolvedConfig, css: Option<&str>, text: &mut TextEngine) ->
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

pub fn theme(config: &ResolvedConfig, css_path: Option<&Path>, text: &mut TextEngine) -> Theme {
    let css = css_path.and_then(|path| std::fs::read_to_string(path).ok());
    let (theme, errors) = theme_with_css(config, css.as_deref(), text);
    for error in errors {
        eprintln!("chibipop: popup.css:{}: {}", error.line, error.message);
    }
    theme
}
