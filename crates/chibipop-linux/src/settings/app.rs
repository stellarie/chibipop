//! The iced window: widgetry only (ADR-0005). Every value it edits
//! lives on core's `SettingsForm` or on `LinuxFields`; this file just
//! renders them and routes messages back.
//!
//! The surface mirrors the Windows settings window's field list and
//! grouping (`crates/chibipop-windows/src/ui/settings_window.rs`) with
//! iced-native controls; per ADR-0012 `ocr.language` is hidden, the key
//! fields are the Linux ones, and capture exclusion is a snippet, not a
//! checkbox. Dictionary add/remove/rebuild lands with ticket 41 - here
//! the lists reorder and scope only, which is pure config.

use super::apply::{self, LinuxFields};
use super::channel::{HotkeyChannel, HotkeyControl};
use super::snippets::{self, Compositor};
use anyhow::Context;
use chibipop::config::{
    PopupLayer, TriggerMode, MAX_HEIGHT_RANGE, MAX_WIDTH_RANGE, PASSES_RANGE, SUMMARY_RANGE,
};
use chibipop::settings::SettingsForm;
use iced::widget::{
    button, checkbox, column, container, pick_list, radio, row, scrollable, slider, text,
    text_input,
};
use iced::{Element, Font, Length, Task, Theme};
use std::path::PathBuf;

/// Everything `super::run` resolved before the window opens.
#[derive(Clone)]
pub struct Init {
    pub form: SettingsForm,
    pub linux: LinuxFields,
    pub config_path: PathBuf,
    pub socket_path: PathBuf,
    pub log_path: PathBuf,
    pub compositor: Compositor,
    pub channel: HotkeyChannel,
}

pub fn run(init: Init) -> anyhow::Result<()> {
    iced::application(move || App::new(init.clone()), update, view)
        .title("chibipop settings")
        .theme(|app: &App| {
            if app.form.theme == "light" {
                Theme::Light
            } else {
                Theme::Dark
            }
        })
        .window_size((860.0, 760.0))
        .run()
        .context("running the settings window")
}

struct App {
    form: SettingsForm,
    linux: LinuxFields,
    config_path: PathBuf,
    socket_path: PathBuf,
    log_path: PathBuf,
    compositor: Compositor,
    channel: HotkeyChannel,
    /// System font families for the font combo.
    fonts: Vec<String>,
    /// Text-edited numbers stay text until Apply parses them.
    capture_w: String,
    capture_h: String,
    selected_dict: Option<String>,
    status: String,
}

impl App {
    fn new(init: Init) -> App {
        let mut fonts = system_families();
        if !fonts.iter().any(|f| f == &init.form.font) {
            // The configured literal always appears, resolvable or not
            // (ADR-0012: no sentinel semantics).
            fonts.insert(0, init.form.font.clone());
        }
        App {
            capture_w: init.form.capture_width.to_string(),
            capture_h: init.form.capture_height.to_string(),
            form: init.form,
            linux: init.linux,
            config_path: init.config_path,
            socket_path: init.socket_path,
            log_path: init.log_path,
            compositor: init.compositor,
            channel: init.channel,
            fonts,
            selected_dict: None,
            status: String::new(),
        }
    }

    fn bind_snippet(&self) -> String {
        snippets::bind_snippet(self.compositor, &self.linux.trigger_key_linux)
    }

    fn apply(&mut self) {
        let (Ok(w), Ok(h)) = (self.capture_w.trim().parse(), self.capture_h.trim().parse())
        else {
            self.status = "Capture width and height must be numbers.".to_string();
            return;
        };
        self.form.capture_width = w;
        self.form.capture_height = h;
        match apply::apply(&self.form, &self.linux, &self.config_path, &self.socket_path) {
            Ok(applied) => {
                // The file now holds the clamped truth; show it.
                if let Ok(cfg) = chibipop::config::load_or_create(&self.config_path) {
                    self.form.capture_width = cfg.ocr.capture_width;
                    self.form.capture_height = cfg.ocr.capture_height;
                    self.capture_w = cfg.ocr.capture_width.to_string();
                    self.capture_h = cfg.ocr.capture_height.to_string();
                }
                self.status = apply::describe(&applied);
            }
            Err(e) => self.status = format!("Apply failed: {e:#}"),
        }
    }
}

#[derive(Debug, Clone)]
enum Message {
    Mode(TriggerMode),
    TriggerChord(String),
    PerChar(bool),
    ThemePicked(String),
    FontPicked(String),
    MaxWidth(u8),
    MaxHeight(u8),
    Summary(u16),
    Highlight(bool),
    Scroll(bool),
    SidePanel(bool),
    LayerPicked(String),
    DictSelected(String),
    DictUp,
    DictDown,
    DictExclude,
    DictInclude,
    Passes(u8),
    PreferVertical(bool),
    ScanAlnum(bool),
    ShowScanRegion(bool),
    CaptureW(String),
    CaptureH(String),
    ShowLookupLog(bool),
    AnkiEnabled(bool),
    AnkiUrl(String),
    AnkiDeck(String),
    AnkiModel(String),
    AnkiAddKey(String),
    FieldMapAnki(usize, String),
    FieldMapSource(usize, String),
    CopyBind,
    CopyRule,
    Apply,
}

fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Mode(mode) => app.form.mode = mode,
        Message::TriggerChord(chord) => app.linux.trigger_key_linux = chord,
        Message::PerChar(on) => app.form.per_character_lookup = on,
        Message::ThemePicked(theme) => app.form.theme = theme,
        Message::FontPicked(font) => app.form.font = font,
        Message::MaxWidth(v) => app.form.max_width_percent = v,
        Message::MaxHeight(v) => app.form.max_height_percent = v,
        Message::Summary(v) => app.form.summary_chars = v as usize,
        Message::Highlight(on) => app.form.highlight_match = on,
        Message::Scroll(on) => app.form.scroll_popup = on,
        Message::SidePanel(on) => app.form.side_panel = on,
        Message::LayerPicked(layer) => {
            app.linux.layer = if layer == "top" { PopupLayer::Top } else { PopupLayer::Overlay };
        }
        Message::DictSelected(name) => app.selected_dict = Some(name),
        Message::DictUp => move_selected(app, -1),
        Message::DictDown => move_selected(app, 1),
        Message::DictExclude => {
            if let Some(name) = shift_between(&mut app.form.dict_names, &mut app.form.dict_excluded, app.selected_dict.as_deref()) {
                app.selected_dict = Some(name);
            }
        }
        Message::DictInclude => {
            if let Some(name) = shift_between(&mut app.form.dict_excluded, &mut app.form.dict_names, app.selected_dict.as_deref()) {
                app.selected_dict = Some(name);
            }
        }
        Message::Passes(v) => app.form.max_ocr_passes = v,
        Message::PreferVertical(on) => app.form.prefer_vertical = on,
        Message::ScanAlnum(on) => app.form.scan_alphanumeric = on,
        Message::ShowScanRegion(on) => app.form.show_scan_region = on,
        Message::CaptureW(v) => app.capture_w = v,
        Message::CaptureH(v) => app.capture_h = v,
        Message::ShowLookupLog(on) => app.linux.show_lookup_log = on,
        Message::AnkiEnabled(on) => app.form.anki_enabled = on,
        Message::AnkiUrl(v) => app.form.anki_url = v,
        Message::AnkiDeck(v) => app.form.anki_deck = v,
        Message::AnkiModel(v) => app.form.anki_model = v,
        Message::AnkiAddKey(v) => app.linux.add_key_linux = v,
        Message::FieldMapAnki(i, v) => {
            if let Some(m) = app.form.field_map.get_mut(i) {
                m.anki_field = v;
            }
        }
        Message::FieldMapSource(i, v) => {
            if let Some(m) = app.form.field_map.get_mut(i) {
                m.source = v;
            }
        }
        Message::CopyBind => return iced::clipboard::write(app.bind_snippet()),
        Message::CopyRule => {
            if let (_, Some(rule)) = snippets::capture_rule(app.compositor) {
                return iced::clipboard::write(rule);
            }
        }
        Message::Apply => app.apply(),
    }
    Task::none()
}

/// Move the selected dictionary within whichever list holds it.
fn move_selected(app: &mut App, delta: i32) {
    let Some(name) = app.selected_dict.as_deref() else { return };
    for list in [&mut app.form.dict_names, &mut app.form.dict_excluded] {
        if let Some(at) = list.iter().position(|n| n == name) {
            let to = at as i32 + delta;
            if to >= 0 && (to as usize) < list.len() {
                list.swap(at, to as usize);
            }
            return;
        }
    }
}

/// Move `selected` from one list's ranks to the other's end.
fn shift_between(
    from: &mut Vec<String>,
    to: &mut Vec<String>,
    selected: Option<&str>,
) -> Option<String> {
    let name = selected?;
    let at = from.iter().position(|n| n == name)?;
    let name = from.remove(at);
    to.push(name.clone());
    Some(name)
}

fn view(app: &App) -> Element<'_, Message> {
    let content = column![
        // 設定 doubles as the JP-fallback proof: kanji in the very
        // first line of the window, straight through cosmic-text.
        text("chibipop 設定 (settings)").size(24),
        trigger_section(app),
        popup_section(app),
        dictionaries_section(app),
        ocr_section(app),
        anki_section(app),
        debug_section(app),
        status_row(app),
    ]
    .spacing(18)
    .padding(20)
    .max_width(820);
    scrollable(container(content).center_x(Length::Fill)).into()
}

fn section<'a>(
    title: &'a str,
    body: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    column![text(title).size(18), body.into()].spacing(8).into()
}

fn labeled<'a>(label: &'a str, control: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    row![text(label).width(240), control.into()].spacing(10).align_y(iced::Center).into()
}

fn trigger_section(app: &App) -> Element<'_, Message> {
    let selected = if app.form.mode == TriggerMode::Live {
        TriggerMode::Live
    } else {
        // The legacy `hold-shift` alias reads as HoldKey, exactly like
        // the Windows radios.
        TriggerMode::HoldKey
    };
    let mode = row![
        radio("Live", TriggerMode::Live, Some(selected), Message::Mode),
        radio("Hold key", TriggerMode::HoldKey, Some(selected), Message::Mode),
    ]
    .spacing(20);

    let hotkey: Element<'_, Message> =
        match app.channel.control(app.compositor, &app.linux.trigger_key_linux) {
            HotkeyControl::Snippet { text: snippet } => column![
                text("Native channel: your compositor owns the binding. Paste this into its config:"),
                container(text(snippet).font(Font::MONOSPACE).size(13)).padding(8),
                button("Copy bind snippet").on_press(Message::CopyBind),
            ]
            .spacing(6)
            .into(),
            // Constructed by ticket 36's portal session; rendered here
            // so the portal channel needs no view reshaping.
            HotkeyControl::Rebind { current } => column![
                text(format!(
                    "Portal binding: {}",
                    current.as_deref().unwrap_or("(not bound)")
                )),
                button("Rebind..."),
            ]
            .spacing(6)
            .into(),
        };

    section(
        "Trigger",
        column![
            mode,
            labeled(
                "Trigger chord (portal syntax)",
                text_input("ALT+F", &app.linux.trigger_key_linux)
                    .on_input(Message::TriggerChord)
                    .width(200),
            ),
            hotkey,
        ]
        .spacing(10),
    )
}

fn popup_section(app: &App) -> Element<'_, Message> {
    let themes = vec!["dark".to_string(), "light".to_string()];
    let layers = vec!["overlay".to_string(), "top".to_string()];
    let layer_now = match app.linux.layer {
        PopupLayer::Overlay => "overlay".to_string(),
        PopupLayer::Top => "top".to_string(),
    };
    let (rule_caption, rule) = snippets::capture_rule(app.compositor);
    let mut capture: Vec<Element<'_, Message>> = vec![text(rule_caption).size(14).into()];
    if let Some(rule) = rule {
        capture.push(
            row![
                container(text(rule).font(Font::MONOSPACE).size(13)).padding(8),
                button("Copy rule").on_press(Message::CopyRule),
            ]
            .spacing(10)
            .align_y(iced::Center)
            .into(),
        );
    }

    section(
        "Popup",
        column![
            labeled(
                "Theme",
                pick_list(themes, Some(app.form.theme.clone()), Message::ThemePicked),
            ),
            labeled(
                "Font",
                pick_list(
                    app.fonts.as_slice(),
                    Some(app.form.font.clone()),
                    |f: String| Message::FontPicked(f),
                ),
            ),
            // Kanji and kana beside the combo: the live proof that the
            // system fallback renders Japanese.
            labeled("Preview", text("日本語プレビュー: 辞書・漢字・かな")),
            labeled(
                "Max width (% of screen)",
                row![
                    slider(MAX_WIDTH_RANGE.0..=MAX_WIDTH_RANGE.1, app.form.max_width_percent, Message::MaxWidth).width(220),
                    text(format!("{}%", app.form.max_width_percent)),
                ]
                .spacing(10),
            ),
            labeled(
                "Max height (% of screen)",
                row![
                    slider(MAX_HEIGHT_RANGE.0..=MAX_HEIGHT_RANGE.1, app.form.max_height_percent, Message::MaxHeight).width(220),
                    text(format!("{}%", app.form.max_height_percent)),
                ]
                .spacing(10),
            ),
            labeled(
                "Summary length (characters)",
                row![
                    slider(SUMMARY_RANGE.0 as u16..=SUMMARY_RANGE.1 as u16, app.form.summary_chars as u16, Message::Summary).width(220),
                    text(app.form.summary_chars.to_string()),
                ]
                .spacing(10),
            ),
            checkbox(app.form.highlight_match)
                .label("Box the word being defined")
                .on_toggle(Message::Highlight),
            checkbox(app.form.scroll_popup)
                .label("Scroll long entries with the wheel")
                .on_toggle(Message::Scroll),
            checkbox(app.form.side_panel)
                .label("Show related words beside the popup")
                .on_toggle(Message::SidePanel),
            labeled(
                "Layer (overlay clears fullscreen)",
                pick_list(layers, Some(layer_now), Message::LayerPicked),
            ),
            column(capture).spacing(6),
        ]
        .spacing(10),
    )
}

fn dict_rows<'a>(
    names: &'a [String],
    selected: Option<&str>,
) -> Element<'a, Message> {
    if names.is_empty() {
        return text("(none)").size(14).into();
    }
    column(names.iter().map(|name| {
        let b = button(text(name.as_str()).size(14))
            .on_press(Message::DictSelected(name.clone()))
            .width(Length::Fill);
        if Some(name.as_str()) == selected {
            b.style(button::primary).into()
        } else {
            b.style(button::text).into()
        }
    }))
    .spacing(2)
    .into()
}

fn dictionaries_section(app: &App) -> Element<'_, Message> {
    let selected = app.selected_dict.as_deref();
    let lists = row![
        column![
            text("Searched - for the selected OCR language").size(14),
            dict_rows(&app.form.dict_names, selected),
        ]
        .spacing(6)
        .width(Length::FillPortion(1)),
        column![
            text("Not searched").size(14),
            dict_rows(&app.form.dict_excluded, selected),
        ]
        .spacing(6)
        .width(Length::FillPortion(1)),
        column![
            button("Move up").on_press(Message::DictUp).width(Length::Fill),
            button("Move down").on_press(Message::DictDown).width(Length::Fill),
            button("Exclude").on_press(Message::DictExclude).width(Length::Fill),
            button("Include").on_press(Message::DictInclude).width(Length::Fill),
        ]
        .spacing(6)
        .width(140),
    ]
    .spacing(16);

    let mut body = column![
        lists,
        text("Order is matched by dictionary name. Adding and removing dictionaries lands with the rebuild ticket.")
            .size(13),
    ]
    .spacing(8);
    if !app.form.freq_names.is_empty() {
        body = body.push(text(format!("Frequency lists: {}", app.form.freq_names.join(", "))).size(13));
    }
    section("Dictionaries", body)
}

fn ocr_section(app: &App) -> Element<'_, Message> {
    // ocr.language is hidden on Linux (ADR-0012): meikiocr is JA-only;
    // the stored value is preserved by the whole-struct save.
    let passes: Vec<u8> = (PASSES_RANGE.0..=PASSES_RANGE.1).collect();
    section(
        "OCR",
        column![
            labeled(
                "OCR passes per hover",
                pick_list(passes, Some(app.form.max_ocr_passes), Message::Passes),
            ),
            text("1 = no tiling. Higher reads further ahead but can resolve the wrong character.")
                .size(13),
            labeled(
                "Capture width (px)",
                text_input("500", &app.capture_w).on_input(Message::CaptureW).width(120),
            ),
            labeled(
                "Capture height (px)",
                text_input("100", &app.capture_h).on_input(Message::CaptureH).width(120),
            ),
            text("Vertical mode swaps these two values.").size(13),
            checkbox(app.form.prefer_vertical)
                .label("Prefer vertical text (manga, VN)")
                .on_toggle(Message::PreferVertical),
            checkbox(app.form.scan_alphanumeric)
                .label("Scan alphanumeric text")
                .on_toggle(Message::ScanAlnum),
            checkbox(app.form.per_character_lookup)
                .label("Look up each character as you hover (Live mode only)")
                .on_toggle(Message::PerChar),
            checkbox(app.form.show_scan_region)
                .label("Outline what each hover captured")
                .on_toggle(Message::ShowScanRegion),
        ]
        .spacing(10),
    )
}

fn anki_section(app: &App) -> Element<'_, Message> {
    let mut body = column![
        checkbox(app.form.anki_enabled)
            .label("Enable Anki integration")
            .on_toggle(Message::AnkiEnabled),
        labeled(
            "AnkiConnect URL",
            text_input("http://localhost:8765", &app.form.anki_url)
                .on_input(Message::AnkiUrl)
                .width(260),
        ),
        labeled(
            "Deck",
            text_input("Default", &app.form.anki_deck).on_input(Message::AnkiDeck).width(260),
        ),
        labeled(
            "Note type",
            text_input("Lapis", &app.form.anki_model).on_input(Message::AnkiModel).width(260),
        ),
        labeled(
            "Add-card chord (portal syntax)",
            text_input("ALT+A", &app.linux.add_key_linux)
                .on_input(Message::AnkiAddKey)
                .width(200),
        ),
        text("Field mappings").size(14),
    ]
    .spacing(10);
    for (i, mapping) in app.form.field_map.iter().enumerate() {
        body = body.push(
            row![
                text_input("Anki field", &mapping.anki_field)
                    .on_input(move |v| Message::FieldMapAnki(i, v))
                    .width(220),
                text("<-").size(14),
                text_input("source", &mapping.source)
                    .on_input(move |v| Message::FieldMapSource(i, v))
                    .width(220),
            ]
            .spacing(10)
            .align_y(iced::Center),
        );
    }
    section("Anki", body)
}

fn debug_section(app: &App) -> Element<'_, Message> {
    section(
        "Debug",
        column![
            checkbox(app.linux.show_lookup_log)
                .label("Write looked-up words to the log file")
                .on_toggle(Message::ShowLookupLog),
            text(format!("Log: {}", app.log_path.display())).size(13),
        ]
        .spacing(8),
    )
}

fn status_row(app: &App) -> Element<'_, Message> {
    let hint = if app.status.is_empty() {
        "Apply saves the config file; a running daemon reloads it live.".to_string()
    } else {
        app.status.clone()
    };
    row![
        text(hint).size(14).width(Length::Fill),
        button("Apply").on_press(Message::Apply),
    ]
    .spacing(16)
    .align_y(iced::Center)
    .into()
}

/// Every installed family name, sorted and deduplicated. The JP-capable
/// filter (ADR-0004's probe) arrives with the popup's font work; a full
/// list never lies, it is just longer.
fn system_families() -> Vec<String> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let mut names: Vec<String> =
        db.faces().map(|face| face.families[0].0.clone()).collect();
    names.sort();
    names.dedup();
    names
}
