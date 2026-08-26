//! The iced window: widgetry only (ADR-0005). Every value it edits
//! lives on core's `SettingsForm` or on `LinuxFields`; this file just
//! renders them and routes messages back.
//!
//! The surface mirrors the Windows settings window's field list and
//! grouping (`crates/chibipop-windows/src/ui/settings_window.rs`) with
//! iced-native controls; per ADR-0012 `ocr.language` is hidden, the key
//! fields are the Linux ones, and capture exclusion is a snippet, not a
//! checkbox.
//!
//! The dictionary controls stage into core's `SettingsForm` and only
//! [`super::rebuild`] ever touches the library on disk; the window's one
//! piece of state about it is `rebuild_progress`, which is both the
//! busy gate and the line the status area shows.

use super::apply::{self, LinuxFields};
use super::autostart;
use super::channel::{HotkeyChannel, HotkeyControl};
use super::rebuild;
use super::snippets::{self, Compositor};
use crate::lock::{self, LockError};
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
    /// Where the dictionary archives live; a rebuild edits it.
    pub library_dir: PathBuf,
    /// The database a rebuild renames over.
    pub db_path: PathBuf,
    /// Where the library flock file goes.
    pub runtime_dir: PathBuf,
    /// `None` when no XDG config root resolves; the row says so.
    pub autostart: Option<autostart::Target>,
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
    library_dir: PathBuf,
    db_path: PathBuf,
    runtime_dir: PathBuf,
    autostart: Option<autostart::Target>,
    /// Mirrors the `.desktop` file, re-read after every toggle.
    autostart_on: bool,
    /// System font families for the font combo.
    fonts: Vec<String>,
    /// Text-edited numbers stay text until Apply parses them.
    capture_w: String,
    capture_h: String,
    selected_dict: Option<String>,
    /// The path typed into the Add row; there is no portal file dialog
    /// on this window's dependency budget, and a path entry never lies
    /// about which file it took.
    add_path: String,
    /// The live rebuild's last rendered progress line; empty when idle.
    /// Its presence *is* the busy flag - Rebuild and Apply are refused
    /// while it is set.
    rebuild_progress: Option<String>,
    /// A click's check is in flight; the button stays shut until the
    /// line comes back, so a held Enter cannot start ten of them.
    checking_update: bool,
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
            library_dir: init.library_dir,
            db_path: init.db_path,
            runtime_dir: init.runtime_dir,
            autostart_on: init.autostart.as_ref().is_some_and(autostart::Target::is_enabled),
            autostart: init.autostart,
            fonts,
            selected_dict: None,
            add_path: String::new(),
            rebuild_progress: None,
            checking_update: false,
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

    /// A rebuild is writing the library; edits to it would race.
    fn busy(&self) -> bool {
        self.rebuild_progress.is_some()
    }

    /// Stage the typed path for import.
    fn add_dictionary(&mut self) {
        let typed = self.add_path.trim();
        if typed.is_empty() {
            return;
        }
        let source = PathBuf::from(typed);
        match self.form.stage_add(&source) {
            Some(_) => {
                self.status = format!(
                    "{} is staged; press Rebuild to build it in.",
                    source.display()
                );
                self.add_path.clear();
            }
            // stage_add refuses an unreadable archive and a source it
            // already holds; the file itself is the only thing to say.
            None => {
                self.status = format!(
                    "{} is not a readable dictionary archive, or is already staged.",
                    source.display()
                );
            }
        }
    }

    /// Stage the selected row for removal.
    fn remove_dictionary(&mut self) {
        let Some(name) = self.selected_dict.take() else {
            self.status = "Select a dictionary to remove first.".to_string();
            return;
        };
        self.form.stage_remove(&name);
        self.status = format!("{name} is staged for removal; press Rebuild to apply it.");
    }

    /// Take the library and start building.
    fn start_rebuild(&mut self) -> Task<Message> {
        let plan = rebuild::Plan {
            library_dir: self.library_dir.clone(),
            out: self.db_path.clone(),
            runtime_dir: self.runtime_dir.clone(),
            socket: self.socket_path.clone(),
        };
        // Bounded by the builder's own line rate; the window drains it
        // every frame, so an unbounded channel never grows.
        let (tx, rx) = iced::futures::channel::mpsc::unbounded();
        match rebuild::spawn(&self.form, plan, move |p| {
            let _ = tx.unbounded_send(p);
        }) {
            Ok(()) => {
                self.rebuild_progress = Some("Starting the rebuild…".to_string());
                self.status = "Rebuilding your dictionary. This can take a few minutes.".to_string();
                Task::run(rx, Message::RebuildProgress)
            }
            Err(LockError::AlreadyRunning { path, pid }) => {
                self.status = lock::rebuild_refusal(&self.library_dir, &path, pid);
                Task::none()
            }
            Err(LockError::Io(e)) => {
                self.status = format!("Could not claim the library lock: {e}");
                Task::none()
            }
        }
    }

    /// One message from the rebuild thread.
    fn took_progress(&mut self, progress: rebuild::Progress) {
        match &progress {
            // Only lines the shared renderer has words for; the rest are
            // builder chatter the user never asked about.
            rebuild::Progress::Line(line) => {
                if let Some(text) = chibipop::dict::progress::friendly(line) {
                    self.rebuild_progress = Some(text);
                }
                return;
            }
            rebuild::Progress::Done { .. } => {
                // The library on disk now matches the form.
                self.form.clear_staged();
                self.selected_dict = None;
            }
            // The archives went back; the form still describes what the
            // user asked for, so the staged edits stay staged.
            rebuild::Progress::Failed(_) => {}
        }
        self.rebuild_progress = None;
        self.status = rebuild::describe(&progress);
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
    AddPath(String),
    DictAdd,
    DictRemove,
    Rebuild,
    RebuildProgress(rebuild::Progress),
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
    Autostart(bool),
    CheckUpdate,
    UpdateChecked(String),
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
        Message::AddPath(v) => app.add_path = v,
        Message::DictAdd => app.add_dictionary(),
        Message::DictRemove => app.remove_dictionary(),
        Message::Rebuild => return app.start_rebuild(),
        Message::RebuildProgress(p) => app.took_progress(p),
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
        Message::Autostart(on) => {
            // Write or remove, then re-read: the file is the state, so
            // the widget shows what the filesystem has, not the click.
            let Some(target) = &app.autostart else {
                app.status = "Autostart needs an XDG config directory \
                              (set XDG_CONFIG_HOME or HOME)."
                    .to_string();
                return Task::none();
            };
            if let Err(e) = target.set(on) {
                app.status =
                    format!("Autostart update failed for {}: {e}", target.file().display());
            } else {
                app.status = String::new();
            }
            app.autostart_on = target.is_enabled();
        }
        Message::CheckUpdate => {
            app.checking_update = true;
            app.status = "Checking for updates\u{2026}".to_string();
            // ureq blocks and this is the UI thread: one click, one
            // thread, one line back - the rebuild row's shape, with a
            // single message instead of a stream.
            let (tx, rx) = iced::futures::channel::mpsc::unbounded();
            std::thread::spawn(move || {
                let _ = tx.unbounded_send(super::update::report(env!("CARGO_PKG_VERSION")));
            });
            return Task::run(rx, Message::UpdateChecked);
        }
        Message::UpdateChecked(line) => {
            app.checking_update = false;
            app.status = line;
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
        startup_section(app),
        update_section(app),
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
            // The portal rung (ticket 36). There is no in-app rebind to
            // offer and pretending otherwise would be the one thing
            // ADR-0005 forbids here: the portal owns the binding, the
            // dialog it raises at bind time and the desktop's own
            // shortcut editor are where a key changes, and the chord
            // above is only what we ask for next time.
            HotkeyControl::Rebind { current } => column![
                text("Portal channel: the GlobalShortcuts portal owns this binding."),
                text(match &current {
                    Some(key) => format!("Current key: {key}"),
                    None => "Current key: your desktop does not report one - open its global-shortcuts settings to see or change it".to_string(),
                }),
                text(
                    "The chord above is the preferred trigger, offered to the portal at the next start; your desktop's shortcut editor has the last word."
                )
                .size(13),
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
            // Only these two touch the library, so only these two are
            // refused while a rebuild owns it.
            button("Remove")
                .on_press_maybe((!app.busy()).then_some(Message::DictRemove))
                .width(Length::Fill),
        ]
        .spacing(6)
        .width(140),
    ]
    .spacing(16);

    let add = row![
        text_input("path to a Yomitan .zip", &app.add_path)
            .on_input_maybe((!app.busy()).then_some(Message::AddPath))
            .width(Length::Fill),
        button("Add").on_press_maybe((!app.busy()).then_some(Message::DictAdd)),
    ]
    .spacing(8)
    .align_y(iced::Center);

    let mut body = column![
        lists,
        add,
        text(
            "Order is matched by dictionary name. Adds and removals are staged: \
             Rebuild imports them and rebuilds the database."
        )
        .size(13),
    ]
    .spacing(8);
    if !app.form.freq_names.is_empty() {
        body = body.push(text(format!("Frequency lists: {}", app.form.freq_names.join(", "))).size(13));
    }
    body = body.push(rebuild_row(app));
    section("Dictionaries", body)
}

/// The Rebuild button and whatever the running build last said.
///
/// One live rebuild at a time: `rebuild_progress` is both the button's
/// gate and the line it replaces itself with.
fn rebuild_row(app: &App) -> Element<'_, Message> {
    let note = match &app.rebuild_progress {
        Some(line) => line.clone(),
        None if app.form.has_staged() => {
            "Staged changes are not in the database yet.".to_string()
        }
        None => "Rebuild reads every archive again; the popup keeps working meanwhile."
            .to_string(),
    };
    row![
        text(note).size(13).width(Length::Fill),
        button("Rebuild").on_press_maybe((!app.busy()).then_some(Message::Rebuild)),
    ]
    .spacing(16)
    .align_y(iced::Center)
    .into()
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

/// The autostart row: stateless per ADR-0012 — the checkbox *is* the
/// XDG autostart `.desktop` file, applied on toggle, no Apply needed
/// and no TOML field anywhere.
fn startup_section(app: &App) -> Element<'_, Message> {
    let body: Element<'_, Message> = match &app.autostart {
        Some(target) => column![
            checkbox(app.autostart_on)
                .label("Start chibipop at login")
                .on_toggle(Message::Autostart),
            text(format!(
                "Writes {} on toggle - GNOME, KDE, and uwsm sessions read it. \
                 Bare Hyprland/sway: see extras/ in the release.",
                target.file().display()
            ))
            .size(13),
        ]
        .spacing(8)
        .into(),
        None => text(
            "Autostart needs an XDG config directory (set XDG_CONFIG_HOME or HOME).",
        )
        .size(13)
        .into(),
    };
    section("Startup", body)
}

/// The Updates row, mirroring the Windows window's group of the same
/// name - and stopping where ADR-0007 says it stops. The check reports;
/// there is no swap on this platform to offer, so the row says which
/// asset to fetch and who owns the binary.
fn update_section(app: &App) -> Element<'_, Message> {
    section(
        "Updates",
        column![
            button("Check for updates")
                .on_press_maybe((!app.checking_update).then_some(Message::CheckUpdate)),
            text(format!(
                "You are running {}. A check asks GitHub for the newest release \
                 and reports it; chibipop never replaces its own binary here.",
                env!("CARGO_PKG_VERSION"),
            ))
            .size(13),
        ]
        .spacing(8),
    )
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

/// The dictionary controls, driven the way iced drives them: one
/// `Message` at a time through [`update`]. No window is opened - the
/// widgetry is one `view` call over this same state, and what matters
/// here is which state a press leaves behind.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Env;
    use std::path::Path;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/yomitan")
            .join(name)
    }

    /// The window's state without touching fontdb or the filesystem.
    fn app(dir: &Path) -> App {
        let cfg = chibipop::config::Config::default();
        App {
            form: chibipop::settings::from_config(&cfg, &[]),
            linux: LinuxFields::from_config(&cfg),
            config_path: dir.join("chibipop.toml"),
            socket_path: dir.join("run/absent.sock"),
            log_path: dir.join("chibipop.log"),
            compositor: Compositor::Hyprland,
            channel: HotkeyChannel::Native,
            library_dir: dir.join("library"),
            db_path: dir.join("chibipop.sqlite"),
            runtime_dir: dir.join("run"),
            autostart: None,
            autostart_on: false,
            fonts: vec!["Noto Sans".to_string()],
            capture_w: "480".to_string(),
            capture_h: "160".to_string(),
            selected_dict: None,
            add_path: String::new(),
            rebuild_progress: None,
            checking_update: false,
            status: String::new(),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("chibipop_settings_app_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("library")).unwrap();
        std::fs::create_dir_all(dir.join("run")).unwrap();
        dir
    }

    #[test]
    fn adding_a_readable_archive_stages_it_under_its_own_title() {
        let dir = scratch("add");
        let mut app = app(&dir);
        let _ = update(&mut app, Message::AddPath(fixture("terms.zip").display().to_string()));
        let _ = update(&mut app, Message::DictAdd);

        assert!(app.form.dict_names.iter().any(|n| n == "FixtureTerms"), "{:?}", app.form.dict_names);
        assert!(app.form.has_staged(), "the add must wait for a rebuild");
        assert!(app.add_path.is_empty(), "the entry clears once the path is taken");
        assert!(app.status.contains("Rebuild"), "{}", app.status);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A path that is not a dictionary is refused, not silently ignored.
    #[test]
    fn adding_an_unreadable_path_says_so_and_stages_nothing() {
        let dir = scratch("add_bad");
        let mut app = app(&dir);
        let before = app.form.dict_names.clone();
        let _ = update(&mut app, Message::AddPath(dir.join("nope.zip").display().to_string()));
        let _ = update(&mut app, Message::DictAdd);

        assert_eq!(before, app.form.dict_names);
        assert!(!app.form.has_staged());
        assert!(app.status.contains("not a readable dictionary archive"), "{}", app.status);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_without_a_selection_asks_for_one() {
        let dir = scratch("remove_none");
        let mut app = app(&dir);
        let _ = update(&mut app, Message::DictRemove);
        assert!(!app.form.has_staged());
        assert!(app.status.contains("Select a dictionary"), "{}", app.status);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_the_selected_row_stages_it_and_drops_the_selection() {
        let dir = scratch("remove");
        let mut app = app(&dir);
        app.form.dict_names = vec!["Jitendex".to_string(), "Daijirin".to_string()];

        let _ = update(&mut app, Message::DictSelected("Jitendex".to_string()));
        let _ = update(&mut app, Message::DictRemove);

        assert_eq!(vec!["Daijirin".to_string()], app.form.dict_names);
        assert!(app.form.has_staged());
        assert_eq!(None, app.selected_dict);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Builder lines reach the user through the shared renderer, and the
    /// raw ones stay out of sight.
    #[test]
    fn progress_lines_render_through_the_shared_renderer() {
        let dir = scratch("progress");
        let mut app = app(&dir);
        let line = |l: &str| Message::RebuildProgress(rebuild::Progress::Line(l.to_string()));

        let _ = update(&mut app, line("progress  12500 / 768636"));
        assert_eq!(Some("12,500 of 768,636 entries…".to_string()), app.rebuild_progress);
        assert!(app.busy(), "a live rebuild keeps the library controls shut");

        // Nothing to say about it: the last line must not replace the one
        // the user can read.
        let _ = update(&mut app, line("wrote /tmp/x.sqlite.building: 3 entries"));
        assert_eq!(Some("12,500 of 768,636 entries…".to_string()), app.rebuild_progress);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_finished_rebuild_clears_the_staged_edits_and_reopens_the_controls() {
        let dir = scratch("done");
        let mut app = app(&dir);
        app.form.dict_names = vec!["Jitendex".to_string()];
        let _ = update(&mut app, Message::DictSelected("Jitendex".to_string()));
        let _ = update(&mut app, Message::DictRemove);
        app.rebuild_progress = Some("Creating search index…".to_string());

        let _ = update(
            &mut app,
            Message::RebuildProgress(rebuild::Progress::Done {
                entries: 3,
                terms: 5,
                reload: rebuild::Reload::Sent("OK reload".to_string()),
            }),
        );

        assert!(!app.busy(), "the controls reopen when the build ends");
        assert!(!app.form.has_staged(), "the library on disk now matches the form");
        assert!(app.status.contains("daemon reloaded"), "{}", app.status);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The archives went back, so the request must survive for a retry.
    #[test]
    fn a_failed_rebuild_keeps_the_staged_edits() {
        let dir = scratch("failed");
        let mut app = app(&dir);
        app.form.dict_names = vec!["Jitendex".to_string()];
        let _ = update(&mut app, Message::DictSelected("Jitendex".to_string()));
        let _ = update(&mut app, Message::DictRemove);
        app.rebuild_progress = Some("Creating search index…".to_string());

        let _ = update(
            &mut app,
            Message::RebuildProgress(rebuild::Progress::Failed("invalid Zip archive".to_string())),
        );

        assert!(!app.busy());
        assert!(app.form.has_staged(), "a failed rebuild must not forget the request");
        assert!(app.status.contains("unchanged"), "{}", app.status);
        assert!(app.status.contains("invalid Zip archive"), "{}", app.status);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pressing Rebuild while another process holds the library says who
    /// holds it, and starts nothing.
    #[test]
    fn a_rebuild_refused_by_the_flock_reports_the_holder() {
        let dir = scratch("contend");
        let mut app = app(&dir);
        let held = lock::acquire_at(&app.runtime_dir, &lock::library_file_name(&app.library_dir))
            .expect("standing in for the first settings process");

        let _ = update(&mut app, Message::Rebuild);

        assert!(!app.busy(), "a refused rebuild is not in flight");
        assert!(app.status.contains("Another rebuild"), "{}", app.status);
        assert!(app.status.contains(&std::process::id().to_string()), "{}", app.status);
        assert!(!app.db_path.exists(), "a refused rebuild writes nothing");
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn init_at(config_home: &std::path::Path) -> Init {
        let cfg = chibipop::config::Config::default();
        let env = Env { xdg_config_home: Some(config_home.to_path_buf()), ..Env::default() };
        Init {
            form: chibipop::settings::from_config(&cfg, &[]),
            linux: LinuxFields::from_config(&cfg),
            config_path: config_home.join("chibipop/chibipop.toml"),
            socket_path: config_home.join("sock"),
            log_path: config_home.join("log"),
            compositor: Compositor::Hyprland,
            channel: HotkeyChannel::Native,
            library_dir: config_home.join("library"),
            db_path: config_home.join("chibipop.sqlite"),
            runtime_dir: config_home.join("run"),
            autostart: autostart::Target::resolve(&env),
        }
    }

    /// What the checkbox click actually does, driven through the real
    /// message handler: the `.desktop` file appears and disappears, a
    /// reopened window reads its state back off the file, and no config
    /// is written on the way (ADR-0012: the file is the whole state).
    #[test]
    fn toggling_autostart_writes_the_file_and_leaves_the_config_alone() {
        let home = std::env::temp_dir().join(format!("chibipop_app_autostart_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();

        let init = init_at(&home);
        let entry = init.autostart.as_ref().expect("XDG_CONFIG_HOME resolves a target").file();
        let mut app = App::new(init.clone());
        let form_before = app.form.clone();
        assert!(!app.autostart_on, "a fresh config home opens with the box clear");

        // The toggle is applied in the handler; the returned task is a
        // no-op here because nothing about it reaches the filesystem.
        let _ = update(&mut app, Message::Autostart(true));
        assert!(app.autostart_on);
        assert!(entry.is_file(), "the click wrote {}", entry.display());
        assert!(std::fs::read_to_string(&entry).unwrap().starts_with("[Desktop Entry]"));

        // Reopening is a fresh App over the same paths: it must read the
        // box's state off the file, not off anything it remembered.
        assert!(App::new(init_at(&home)).autostart_on, "a reopened window sees the entry");

        let _ = update(&mut app, Message::Autostart(false));
        assert!(!app.autostart_on);
        assert!(!entry.exists(), "the second click removed the entry");
        assert!(!App::new(init_at(&home)).autostart_on);

        assert_eq!(form_before, app.form, "autostart touches no config field");
        assert!(!init.config_path.exists(), "autostart writes no config file");

        let _ = std::fs::remove_dir_all(&home);
    }

    /// Without a config root the row is inert rather than lying: the box
    /// stays clear and the status line says why.
    #[test]
    fn autostart_without_a_config_root_reports_instead_of_toggling() {
        let mut init = init_at(&std::env::temp_dir());
        init.autostart = None;
        let mut app = App::new(init);

        let _ = update(&mut app, Message::Autostart(true));
        assert!(!app.autostart_on);
        assert!(app.status.contains("XDG config directory"), "{}", app.status);
    }

    /// The check's answer is a status line and an open button again.
    /// Only the click's own arm reaches the network, so what is driven
    /// here is the line coming back - what it says for each outcome is
    /// [`super::super::update`]'s to prove.
    #[test]
    fn a_finished_update_check_reports_and_reopens_the_button() {
        let dir = scratch("update");
        let mut app = app(&dir);
        app.checking_update = true;
        app.status = "Checking\u{2026}".to_string();

        let _ = update(
            &mut app,
            Message::UpdateChecked("v9.9.9 is available.".to_string()),
        );

        assert!(!app.checking_update, "a finished check reopens the button");
        assert_eq!("v9.9.9 is available.", app.status);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
