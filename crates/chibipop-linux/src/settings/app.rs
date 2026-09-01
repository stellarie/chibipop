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
use super::filechooser;
use super::rebuild;
use super::snippets::{self, Compositor};
use crate::clipboard;
use crate::control::Verb;
use crate::lock::{self, LockError};
use crate::popup;
use anyhow::Context;
use chibipop::config::{
    FieldMapping, LayoutMode, PopupLayer, SentenceMode, TriggerMode, FIELD_SOURCES,
    MAX_HEIGHT_RANGE, MAX_WIDTH_RANGE, PASSES_RANGE, SUMMARY_RANGE,
};
use chibipop::settings::SettingsForm;
use iced::widget::{
    button, checkbox, column, container, pick_list, radio, row, scrollable, slider, text,
    text_input,
};
use iced::{Element, Font, Length, Task, Theme};
use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::LazyLock;

/// Everything `super::run` resolved before the window opens.
#[derive(Clone)]
pub struct Init {
    pub form: SettingsForm,
    pub linux: LinuxFields,
    pub config_path: PathBuf,
    pub socket_path: PathBuf,
    pub log_path: PathBuf,
    pub compositor: Compositor,
    /// Who owns the trigger binding.
    pub channel: HotkeyChannel,
    /// Who owns the add-card binding. A separate value because the
    /// portal answers per id and a row may only name its own key
    /// (`super::hotkey_channel`).
    pub add_channel: HotkeyChannel,
    /// Where the dictionary archives live; a rebuild edits it.
    pub library_dir: PathBuf,
    /// The database a rebuild renames over.
    pub db_path: PathBuf,
    /// Where the library flock file goes.
    pub runtime_dir: PathBuf,
    /// `None` when no XDG config root resolves; the row says so.
    pub autostart: Option<autostart::Target>,
    /// `$HOME`, for expanding a typed `~` path; `None` when it is unset.
    pub home: Option<PathBuf>,
    /// The binary compositor snippets must name, resolved before the
    /// window opened (`paths::exec_name`): a pasted bind has to exec
    /// *this* daemon, not whatever `chibipop` PATH finds (ticket 51).
    pub exe: PathBuf,
    /// Which data-control protocol this session advertises, if any
    /// (`clipboard::rung`). `None` - stock GNOME - is what the
    /// OCR-to-clipboard row reports instead of a chord that could only
    /// log a refusal.
    pub clipboard_rung: Option<clipboard::Rung>,
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
    add_channel: HotkeyChannel,
    library_dir: PathBuf,
    db_path: PathBuf,
    runtime_dir: PathBuf,
    autostart: Option<autostart::Target>,
    home: Option<PathBuf>,
    exe: PathBuf,
    /// Whether this session has a clipboard protocol at all; see
    /// [`Init::clipboard_rung`].
    clipboard_rung: Option<clipboard::Rung>,
    /// Mirrors the `.desktop` file, re-read after every toggle.
    autostart_on: bool,
    /// The font combo's items; see [`font_items`].
    fonts: Vec<Cow<'static, str>>,
    /// Text-edited numbers stay text until Apply parses them.
    capture_w: String,
    capture_h: String,
    selected_dict: Option<String>,
    /// The path typed into the Add row. Kept beside the Browse button
    /// rather than replaced by it: the portal is not on every desktop
    /// (`filechooser::explain` says so when it is missing), and a path
    /// entry never lies about which file it took.
    add_path: String,
    /// A Browse dialog is open. The portal call waits on a human, so it
    /// runs on its own thread; this is what keeps a second click from
    /// stacking a second dialog on top of the first.
    picking: bool,
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
        let fonts = font_items(&init.form.font);
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
            add_channel: init.add_channel,
            library_dir: init.library_dir,
            db_path: init.db_path,
            runtime_dir: init.runtime_dir,
            autostart_on: init.autostart.as_ref().is_some_and(autostart::Target::is_enabled),
            autostart: init.autostart,
            home: init.home,
            exe: init.exe,
            clipboard_rung: init.clipboard_rung,
            fonts,
            selected_dict: None,
            add_path: String::new(),
            picking: false,
            rebuild_progress: None,
            checking_update: false,
            status: String::new(),
        }
    }

    /// The trigger row's copyable bind: the press/release pair.
    fn bind_snippet(&self) -> String {
        snippets::bind_snippet(
            self.compositor,
            &self.linux.trigger_key_linux,
            &self.exe,
            snippets::Bind::Hold,
        )
    }

    /// The add-card row's copyable bind, or `None` when there is no
    /// chord to bind. `None` is also what the row renders on, so the
    /// button and the text cannot disagree.
    fn add_bind_snippet(&self) -> Option<String> {
        match self.add_control() {
            HotkeyControl::Snippet { text } => Some(text),
            HotkeyControl::Rebind { .. } | HotkeyControl::NoChord => None,
        }
    }

    /// What the add-card chord row renders (ADR-0003's 2026-08-26
    /// addendum: the add is a control-socket verb, so the native rung
    /// can bind it exactly like the trigger).
    fn add_control(&self) -> HotkeyControl {
        self.add_channel.control(
            self.compositor,
            &self.linux.add_key_linux,
            &self.exe,
            snippets::Bind::Press(Verb::AnkiAdd),
        )
    }

    /// What the static-region chord row renders.
    ///
    /// [`HotkeyChannel::Native`] unconditionally, and that is the whole
    /// point of decision D1: the portal id set stays at exactly two, so
    /// no GlobalShortcuts session ever registers this action and the
    /// compositor bind is its *only* global channel. Reading
    /// `self.channel` here would render the trigger's portal key under
    /// this chord - a row claiming a key it was never given, which is
    /// the one thing ADR-0005 forbids this window.
    fn static_region_control(&self) -> HotkeyControl {
        HotkeyChannel::Native.control(
            self.compositor,
            &self.linux.static_region_key_linux,
            &self.exe,
            snippets::Bind::Press(Verb::StaticRegion),
        )
    }

    /// The static-region row's copyable bind, or `None` when the chord
    /// is blank. The button exists only while this is `Some`, so a
    /// cleared chord cannot put a stale bind on the clipboard.
    fn static_region_bind_snippet(&self) -> Option<String> {
        match self.static_region_control() {
            HotkeyControl::Snippet { text } => Some(text),
            HotkeyControl::Rebind { .. } | HotkeyControl::NoChord => None,
        }
    }

    /// What the mining screenshot's chord row renders.
    ///
    /// [`HotkeyChannel::Native`] for exactly the reason given above:
    /// nothing registers this action with the portal (D1), so the
    /// compositor bind is its only global channel.
    fn screenshot_control(&self) -> HotkeyControl {
        HotkeyChannel::Native.control(
            self.compositor,
            self.linux.screenshot_key_linux.as_deref().unwrap_or_default(),
            &self.exe,
            snippets::Bind::Press(Verb::Screenshot),
        )
    }

    /// The screenshot row's copyable bind, or `None` when there is no
    /// chord to build one from.
    fn screenshot_bind_snippet(&self) -> Option<String> {
        match self.screenshot_control() {
            HotkeyControl::Snippet { text } => Some(text),
            HotkeyControl::Rebind { .. } | HotkeyControl::NoChord => None,
        }
    }

    /// What the OCR-to-clipboard chord row renders. Native only, for
    /// the same reason as the two above.
    fn ocr_clipboard_control(&self) -> HotkeyControl {
        HotkeyChannel::Native.control(
            self.compositor,
            self.linux.ocr_clipboard_key_linux.as_deref().unwrap_or_default(),
            &self.exe,
            snippets::Bind::Press(Verb::OcrClipboard),
        )
    }

    /// The OCR-to-clipboard row's copyable bind, or `None` when there is
    /// no chord to build one from - or when this compositor has no
    /// clipboard protocol to copy *into*, because a bind that could only
    /// ever log a refusal is the invalid line ADR-0005 forbids this
    /// window from handing a user.
    fn ocr_clipboard_bind_snippet(&self) -> Option<String> {
        // No rung, no bind: the `?` is the guard.
        self.clipboard_rung?;
        match self.ocr_clipboard_control() {
            HotkeyControl::Snippet { text } => Some(text),
            HotkeyControl::Rebind { .. } | HotkeyControl::NoChord => None,
        }
    }

    fn apply(&mut self) {
        let (Ok(w), Ok(h)) = (self.capture_w.trim().parse(), self.capture_h.trim().parse())
        else {
            self.status = "Capture width and height must be numbers.".to_string();
            return;
        };
        self.form.capture_width = w;
        self.form.capture_height = h;
        // A field-map row with no Anki field is not a storable state:
        // Anki has no field called "", so core would look that name up
        // on every add and place nothing under it forever. Add seeds
        // exactly such a row on purpose (only the user's note type
        // knows its field names), so this is the one place a row they
        // opened and left blank stops - on the way to the file, not in
        // core and not per keystroke, because a half-typed name is a
        // normal thing for a text box to hold.
        if let Some(rows) = self.form.field_map.as_mut() {
            rows.retain(|mapping| !mapping.anki_field.trim().is_empty());
        }
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

    /// Stage the typed path for import. `~` is expanded first, because
    /// the entry takes what a shell would have expanded; the Browse
    /// button beside it hands over absolute paths and skips this.
    fn add_dictionary(&mut self) {
        let typed = self.add_path.trim();
        if typed.is_empty() {
            return;
        }
        let source = match crate::paths::expand_tilde(typed, self.home.as_deref()) {
            crate::paths::Typed::Path(p) => p,
            crate::paths::Typed::NoHome => {
                self.status = format!(
                    "{typed} starts with ~ but HOME is unset, so there is nothing to \
                     expand it against; type the full path instead."
                );
                return;
            }
            crate::paths::Typed::UserRelative => {
                self.status = format!(
                    "{typed} is a user-relative ~ path, which is not supported; \
                     type the full path instead."
                );
                return;
            }
        };
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

    /// Open the desktop's own file dialog.
    ///
    /// The portal call waits on a human, so it cannot run on iced's
    /// executor: a thread makes the blocking call and the window learns
    /// the answer through a one-shot channel, which is the same shape
    /// the rebuild uses for its progress lines.
    fn browse_dictionaries(&mut self) -> Task<Message> {
        if self.picking || self.busy() {
            return Task::none();
        }
        let (tx, rx) = iced::futures::channel::oneshot::channel();
        if let Err(err) = std::thread::Builder::new()
            .name("chibipop-filechooser".to_string())
            .spawn(move || {
                let _ = tx.send(filechooser::pick("Add dictionary archives"));
            })
        {
            self.status = format!("Could not open the file dialog: {err}");
            return Task::none();
        }
        self.picking = true;
        self.status = "Choose one or more Yomitan .zip archives…".to_string();
        // The sender is dropped only if the thread panics, and a
        // cancelled dialog is already an `Ok`; either way the button has
        // to come back, so the channel's own failure is an answer too.
        Task::perform(rx, |sent| {
            Message::DictPicked(sent.unwrap_or_else(|_| {
                Err("The file dialog stopped without answering.".to_string())
            }))
        })
    }

    /// Stage everything the dialog handed back, in the order it listed.
    ///
    /// Reported as one line, not one per file: picking twelve archives
    /// is the point of the button, and twelve statuses would leave the
    /// user reading only the last one.
    fn took_picked(&mut self, picked: Result<filechooser::Picked, String>) {
        self.picking = false;
        let sources = match picked {
            Ok(filechooser::Picked::Files(paths)) => paths,
            Ok(filechooser::Picked::Cancelled) => {
                self.status = "No dictionary was chosen.".to_string();
                return;
            }
            Err(why) => {
                self.status = why;
                return;
            }
        };
        // A rebuild can have claimed the library while the dialog was
        // open; staging into a form the builder is already reading is
        // the race `busy` exists to refuse.
        if self.busy() {
            self.status = "A rebuild is running; add those archives once it finishes.".to_string();
            return;
        }
        let mut staged = 0usize;
        let mut refused = Vec::new();
        for source in &sources {
            match self.form.stage_add(source) {
                Some(_) => staged += 1,
                // stage_add refuses an unreadable archive and a source
                // it already holds; the file itself is the only thing to
                // say, and naming which ones is why they are collected.
                None => refused.push(name_of(source)),
            }
        }
        self.status = match (staged, refused.as_slice()) {
            (0, []) => "No dictionary was chosen.".to_string(),
            (0, names) => format!(
                "{} is not a readable dictionary archive, or is already staged.",
                names.join(", ")
            ),
            (n, []) => format!("{n} archive{} staged; press Rebuild to build them in.", plural(n)),
            (n, names) => format!(
                "{n} archive{} staged; press Rebuild to build them in. Skipped {} - not a \
                 readable dictionary archive, or already staged.",
                plural(n),
                names.join(", ")
            ),
        };
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

/// A refused archive named the way the user picked it: the file name,
/// not the whole path. A dialog's worth of absolute paths in one status
/// line is unreadable, and the directory is the one part they just saw.
fn name_of(source: &std::path::Path) -> String {
    source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| source.display().to_string())
}

/// The `s` in "2 archives"; empty for one.
fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

#[derive(Debug, Clone)]
enum Message {
    Mode(TriggerMode),
    TriggerChord(String),
    PerChar(bool),
    ThemePicked(String),
    FontPicked(Cow<'static, str>),
    MaxWidth(u8),
    MaxHeight(u8),
    Summary(u16),
    Highlight(bool),
    Scroll(bool),
    SidePanel(bool),
    LayerPicked(String),
    /// The layout-mode picker's label; mapped back through
    /// [`LAYOUT_MODES`], never by index or by string comparison at the
    /// call site.
    LayoutModePicked(String),
    /// `popup.dictionary_styling`: whether a dictionary's own `style`
    /// declarations and its `styles.css` reach the panel at all.
    DictStyling(bool),
    ShowExamples(bool),
    ShowAttributions(bool),
    ShowImages(bool),
    ShowPartOfSpeech(bool),
    DictSelected(String),
    DictUp,
    DictDown,
    DictExclude,
    DictInclude,
    AddPath(String),
    DictAdd,
    DictRemove,
    /// Open the desktop's file dialog (`super::filechooser`).
    DictBrowse,
    /// What that dialog came back with, off its own thread.
    DictPicked(Result<filechooser::Picked, String>),
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
    FirstDictOnly(bool),
    /// `actions.screenshot.include_on_add`: whether an add carries a
    /// mining picture (core's gate, `chibipop::shot::plan_add`).
    IncludeScreenshot(bool),
    /// The mining screenshot's chord. Empty text is `None` on the
    /// config field, and this arm is the only place that mapping lives.
    ScreenshotKey(String),
    ScreenshotSaveDir(String),
    /// The OCR-to-clipboard chord. Empty text is `None` on the config
    /// field, and this arm is the only place that mapping lives.
    OcrClipboardKey(String),
    /// The sentence-capture picker's label; mapped back through
    /// [`SENTENCE_MODES`], never by index or by string comparison at the
    /// call site.
    SentenceModePicked(String),
    ShowStaticOverlay(bool),
    StaticRegionKey(String),
    /// A field-map row's Anki field name, as typed. Free text because
    /// only the user's note type knows its own field names and this
    /// window never asks Anki for them (see [`field_map_rows`]).
    FieldMapAnki(usize, String),
    /// A field-map row's picked source, mapped back through
    /// [`FIELD_SOURCES`] by [`field_source_of`] rather than trusted as
    /// it arrives: the vocabulary is closed, and `anki::mapped_fields`
    /// drops a row naming anything outside it without a word.
    FieldMapSource(usize, String),
    /// Append a field-map row, seeded on [`NEW_ROW_SOURCE`]. Until this
    /// existed the shipped `field_map` was the only one a Linux user
    /// could have - `screenshot` included, which is to say excluded.
    FieldMapAdd,
    /// Drop the field-map row at this position.
    FieldMapRemove(usize),
    /// Copy the trigger chord's press/release bind.
    CopyBind,
    /// Copy the add-card chord's one-press bind.
    CopyAddBind,
    /// Copy the static-region chord's one-press bind.
    CopyStaticRegionBind,
    /// Copy the mining screenshot's one-press bind.
    CopyScreenshotBind,
    /// Copy the OCR-to-clipboard chord's one-press bind.
    CopyOcrClipboardBind,
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
        Message::FontPicked(font) => app.form.font = font.into_owned(),
        Message::MaxWidth(v) => app.form.max_width_percent = v,
        Message::MaxHeight(v) => app.form.max_height_percent = v,
        Message::Summary(v) => app.form.summary_chars = v as usize,
        Message::Highlight(on) => app.form.highlight_match = on,
        Message::Scroll(on) => app.form.scroll_popup = on,
        Message::SidePanel(on) => app.form.side_panel = on,
        Message::LayerPicked(layer) => {
            app.linux.layer = if layer == "top" { PopupLayer::Top } else { PopupLayer::Overlay };
        }
        Message::LayoutModePicked(label) => app.form.layout_mode = layout_mode_of(&label),
        Message::DictStyling(on) => app.form.dictionary_styling = on,
        Message::ShowExamples(on) => app.form.show_examples = on,
        Message::ShowAttributions(on) => app.form.show_attributions = on,
        Message::ShowImages(on) => app.form.show_images = on,
        Message::ShowPartOfSpeech(on) => app.form.show_part_of_speech = on,
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
        Message::DictBrowse => return app.browse_dictionaries(),
        Message::DictPicked(picked) => app.took_picked(picked),
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
        Message::FirstDictOnly(v) => app.form.first_dict_only = v,
        Message::IncludeScreenshot(on) => app.form.include_screenshot = on,
        // The one place empty text becomes `None`: the config field is
        // an `Option` so that absence stays typed, and a `""` chord
        // written into it would be a sentinel the daemon would then
        // have to know about.
        Message::ScreenshotKey(v) => {
            app.linux.screenshot_key_linux = (!v.trim().is_empty()).then_some(v);
        }
        Message::ScreenshotSaveDir(v) => app.linux.screenshot_save_dir = v,
        // Same mapping, same reason (`upstream-merge-fallout` ticket 06
        // removed this sentinel from the Windows twin).
        Message::OcrClipboardKey(v) => {
            app.linux.ocr_clipboard_key_linux = (!v.trim().is_empty()).then_some(v);
        }
        Message::SentenceModePicked(label) => app.form.sentence_mode = sentence_mode_of(&label),
        Message::ShowStaticOverlay(on) => app.form.show_static_overlay = on,
        Message::StaticRegionKey(v) => app.linux.static_region_key_linux = v,
        Message::FieldMapAnki(i, v) => {
            if let Some(m) = app.form.field_map.as_mut().and_then(|rows| rows.get_mut(i)) {
                m.anki_field = v;
            }
        }
        // The picker hands out [`FIELD_SOURCES`] entries and nothing
        // else, so the vocabulary check is unreachable through the UI.
        // It is here because this arm is the only way a source reaches
        // the form, and a row naming something outside the closed set
        // is one core silently contributes nothing for (`anki.rs`'s
        // `mapped_fields`) - a mapping that looks set and is not.
        Message::FieldMapSource(i, v) => {
            let row = app.form.field_map.as_mut().and_then(|rows| rows.get_mut(i));
            if let (Some(source), Some(m)) = (field_source_of(&v), row) {
                m.source = source.to_string();
            }
        }
        // The Anki field starts blank: it is the user's note type that
        // names its fields. `App::apply` is what refuses to store the
        // row if they never fill it in. A user adding a row is a window
        // that knows its rows, so this creates the list rather than
        // dropping the press when the form has no answer yet.
        Message::FieldMapAdd => {
            app.form.field_map.get_or_insert_with(Vec::new).push(FieldMapping {
                anki_field: String::new(),
                source: NEW_ROW_SOURCE.to_string(),
            });
        }
        // The index is a position in the list the last frame rendered,
        // so a stale one would panic `Vec::remove`; bounds-checked
        // rather than trusting message ordering to rule that out.
        Message::FieldMapRemove(i) => {
            if let Some(rows) = app.form.field_map.as_mut() {
                if i < rows.len() {
                    rows.remove(i);
                }
            }
        }
        Message::CopyBind => return iced::clipboard::write(app.bind_snippet()),
        // The button only exists while there is a snippet, so `None`
        // here is unreachable through the UI; it stays a no-op rather
        // than putting a stale bind on the clipboard.
        Message::CopyAddBind => {
            if let Some(snippet) = app.add_bind_snippet() {
                return iced::clipboard::write(snippet);
            }
        }
        Message::CopyStaticRegionBind => {
            if let Some(snippet) = app.static_region_bind_snippet() {
                return iced::clipboard::write(snippet);
            }
        }
        Message::CopyScreenshotBind => {
            if let Some(snippet) = app.screenshot_bind_snippet() {
                return iced::clipboard::write(snippet);
            }
        }
        Message::CopyOcrClipboardBind => {
            if let Some(snippet) = app.ocr_clipboard_bind_snippet() {
                return iced::clipboard::write(snippet);
            }
        }
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
        content_section(app),
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

    let hotkey: Element<'_, Message> = match app.channel.control(
        app.compositor,
        &app.linux.trigger_key_linux,
        &app.exe,
        snippets::Bind::Hold,
    ) {
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
        HotkeyControl::NoChord => {
            text("Type a chord above to get a bind you can paste or a key to ask the portal for.")
                .size(13)
                .into()
        }
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
    let font_now = selected_family(app);

    section(
        "Popup",
        column![
            labeled(
                "Theme",
                pick_list(themes, Some(app.form.theme.clone()), Message::ThemePicked),
            ),
            labeled(
                "Font",
                pick_list(app.fonts.as_slice(), font_now.cloned(), Message::FontPicked),
            ),
            // Kanji and kana beside the combo, painted with the family
            // just picked: the live proof that it renders Japanese.
            labeled("Preview", text("日本語プレビュー: 辞書・漢字・かな").font(preview_font(font_now))),
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

/// The layout-mode picker, in the order it is offered.
///
/// One ordered table for both halves of the UI edge, exactly as
/// [`SENTENCE_MODES`] is: the labels going out and the mode coming back,
/// so nothing in between gets to decide the mapping.
const LAYOUT_MODES: [(LayoutMode, &str); 2] = [
    (LayoutMode::Roomy, "Roomy — one item per line"),
    (LayoutMode::Compact, "Compact — one line per dictionary"),
];

/// The picker's items, in table order.
fn layout_labels() -> Vec<String> {
    LAYOUT_MODES.iter().map(|&(_, label)| label.to_string()).collect()
}

/// The label a mode is offered under. Every `LayoutMode` is in the
/// table, so the fallback is unreachable.
fn layout_mode_label(mode: LayoutMode) -> &'static str {
    LAYOUT_MODES.iter().find(|&&(m, _)| m == mode).map_or(LAYOUT_MODES[0].1, |&(_, l)| l)
}

/// The mode a picked label names. Only labels this table handed out can
/// come back, so the default is unreachable through the UI.
fn layout_mode_of(label: &str) -> LayoutMode {
    LAYOUT_MODES.iter().find(|&&(_, l)| l == label).map_or(LayoutMode::Roomy, |&(m, _)| m)
}

/// How much of an entry the popup draws: the render settings' decision
/// table, one control per knob.
///
/// Its own group rather than more rows under Popup, and the Windows
/// window groups them the same way: these six decide what an entry
/// *contains*, where the rows above decide how big the panel is.
///
/// Every one of them is a portable field. Neither platform may drop one,
/// so a config file shared between a Windows and a Linux machine means
/// the same thing on both.
fn content_section(app: &App) -> Element<'_, Message> {
    section(
        "Entry content",
        column![
            labeled(
                "Layout",
                pick_list(
                    layout_labels(),
                    Some(layout_mode_label(app.form.layout_mode).to_string()),
                    Message::LayoutModePicked,
                ),
            ),
            checkbox(app.form.dictionary_styling)
                .label("Use the dictionary's own fonts and colours")
                .on_toggle(Message::DictStyling),
            checkbox(app.form.show_examples)
                .label("Show example sentences")
                .on_toggle(Message::ShowExamples),
            checkbox(app.form.show_attributions)
                .label("Show attributions and footnotes")
                .on_toggle(Message::ShowAttributions),
            checkbox(app.form.show_images)
                .label("Show images")
                .on_toggle(Message::ShowImages),
            checkbox(app.form.show_part_of_speech)
                .label("Show part-of-speech labels inside the entry")
                .on_toggle(Message::ShowPartOfSpeech),
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

    // Browse first, because it is the one that opens a picker and the
    // entry beside it is the fallback for a desktop with no portal. Both
    // are shut while a rebuild owns the library, and Browse is shut
    // again while its own dialog is up.
    let add = row![
        button("Browse…")
            .on_press_maybe((!app.busy() && !app.picking).then_some(Message::DictBrowse)),
        text_input("or type a path to a Yomitan .zip", &app.add_path)
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
            // OCR-to-clipboard lives here rather than in a group of its
            // own: it reads the screen with the same engine and the same
            // settings every row above configures, and the only thing
            // it does differently is where the text goes.
            labeled(
                "OCR-to-clipboard chord (portal syntax)",
                text_input(
                    "ALT+C",
                    app.linux.ocr_clipboard_key_linux.as_deref().unwrap_or_default(),
                )
                .on_input(Message::OcrClipboardKey)
                .width(200),
            ),
            ocr_clipboard_bind(app),
        ]
        .spacing(10),
    )
}

/// The OCR-to-clipboard chord's copyable bind, or the reason there is
/// none.
///
/// Two reasons there might be none, and they are different facts: no
/// chord typed, or no clipboard protocol on this compositor at all. The
/// second is checked first, because a bind pasted on a session where
/// chibipop cannot write the selection would be a key that only ever
/// logs a refusal - and ADR-0005's rule is that this window does not
/// hand out lines that cannot work.
fn ocr_clipboard_bind(app: &App) -> Element<'_, Message> {
    if app.clipboard_rung.is_none() {
        return text(
            "This compositor has no clipboard protocol chibipop can use, so there is nothing \
             to bind: writing the selection without keyboard focus needs \
             ext_data_control_manager_v1 or zwlr_data_control_manager_v1, and this session \
             advertises neither. Every other feature is unaffected."
        )
        .size(13)
        .into();
    }
    match app.ocr_clipboard_control() {
        HotkeyControl::Snippet { text: snippet } => column![
            text(
                "Native channel only: this action has no portal shortcut, so a compositor \
                 bind is the only way to reach it. Paste this into your compositor's config:"
            ),
            container(text(snippet).font(Font::MONOSPACE).size(13)).padding(8),
            button("Copy OCR-to-clipboard bind").on_press(Message::CopyOcrClipboardBind),
        ]
        .spacing(6)
        .into(),
        // Unreachable: `ocr_clipboard_control` always asks as
        // `HotkeyChannel::Native`, precisely so this row can never claim
        // a portal key. Rendered honestly rather than unwrapped.
        HotkeyControl::Rebind { .. } => {
            text("OCR-to-clipboard has no portal shortcut; bind it in your compositor.")
                .size(13)
                .into()
        }
        HotkeyControl::NoChord => {
            text("No OCR-to-clipboard chord is set, so there is no bind to copy - type one above.")
                .size(13)
                .into()
        }
    }
}

/// The sentence-capture picker, in the order it is offered.
///
/// One ordered table is both halves of the UI edge - the labels going
/// out and the mode coming back - so nothing in between gets to decide
/// the mapping. The Windows window's `SENTENCE_MODES` exists because a
/// Win32 combo answers with an index; iced answers with the label, which
/// is the same problem and gets the same answer rather than a `match` on
/// a string literal at the call site.
const SENTENCE_MODES: [(SentenceMode, &str); 3] = [
    (SentenceMode::Line, "Current line"),
    (SentenceMode::All, "All lines"),
    (SentenceMode::Static, "Static region"),
];

/// The picker's items, in table order.
fn sentence_labels() -> Vec<String> {
    SENTENCE_MODES.iter().map(|&(_, label)| label.to_string()).collect()
}

/// The label a mode is offered under. Every `SentenceMode` is in the
/// table, so the fallback is unreachable; it is the first item because
/// that is the one a fourth, untabled mode would want.
fn sentence_mode_label(mode: SentenceMode) -> &'static str {
    SENTENCE_MODES.iter().find(|&&(m, _)| m == mode).map_or(SENTENCE_MODES[0].1, |&(_, l)| l)
}

/// The mode a picked label names. Only labels this table handed out can
/// come back, so the default is unreachable through the UI.
fn sentence_mode_of(label: &str) -> SentenceMode {
    SENTENCE_MODES.iter().find(|&&(_, l)| l == label).map_or(SentenceMode::Line, |&(m, _)| m)
}

/// The static-region chord's copyable bind, or the reason there is none.
///
/// Its caption says "native channel" in every case, unlike the trigger's
/// and the add's: this action has no portal id at all (D1), so a
/// compositor bind is not one of two ways to reach it - it is the only
/// way, and a row that left that implicit would be inviting the user to
/// wait for a consent dialog that is never coming.
fn static_region_bind(app: &App) -> Element<'_, Message> {
    match app.static_region_control() {
        HotkeyControl::Snippet { text: snippet } => column![
            text(
                "Native channel only: this action has no portal shortcut, so a compositor \
                 bind is the only way to reach it. Paste this into your compositor's config:"
            ),
            container(text(snippet).font(Font::MONOSPACE).size(13)).padding(8),
            button("Copy static-region bind").on_press(Message::CopyStaticRegionBind),
        ]
        .spacing(6)
        .into(),
        // Unreachable: `static_region_control` always asks as
        // `HotkeyChannel::Native`, precisely so this row can never claim
        // a portal key. Rendered honestly rather than unwrapped.
        HotkeyControl::Rebind { .. } => {
            text("The static region has no portal shortcut; bind it in your compositor.")
                .size(13)
                .into()
        }
        HotkeyControl::NoChord => text(
            "No static-region chord is set, so there is no bind to copy - type one above."
        )
        .size(13)
        .into(),
    }
}

/// The sentence-capture group: which text the Anki sentence field gets,
/// and - only where it means anything - the static region's two rows.
///
/// Windows hides its region rows unless the mode is Static, and so does
/// this, with one deliberate exception: the *chord* row stays. The region
/// can be set in any mode (that is how a user decides to switch to
/// Static), so hiding the only way to bind the key until they had
/// already switched would be a chicken and egg.
fn sentence_rows(app: &App) -> Vec<Element<'_, Message>> {
    let mut rows: Vec<Element<'_, Message>> = vec![labeled(
        "Anki sentence field",
        pick_list(
            sentence_labels(),
            Some(sentence_mode_label(app.form.sentence_mode).to_string()),
            Message::SentenceModePicked,
        ),
    )];
    if app.form.sentence_mode == SentenceMode::Static {
        rows.push(
            checkbox(app.form.show_static_overlay)
                .label("Show the static region outline")
                .on_toggle(Message::ShowStaticOverlay)
                .into(),
        );
        rows.push(
            text(
                "The outline is a layer surface, so it needs zwlr_layer_shell_v1 like the \
                 popup; without it the region still serves lookups, unmarked."
            )
            .size(13)
            .into(),
        );
    }
    rows.push(labeled(
        "Static region chord (portal syntax)",
        text_input("ALT+R", &app.linux.static_region_key_linux)
            .on_input(Message::StaticRegionKey)
            .width(200),
    ));
    rows.push(static_region_bind(app));
    rows
}

/// The mining screenshot's copyable bind, or the reason there is none.
///
/// Native-channel wording in every case, for the same reason
/// `static_region_bind` uses it: this action has no portal id (D1), so
/// a compositor bind is not one of two ways to reach it but the only
/// one, and a row that left that implicit would be inviting the user to
/// wait for a consent dialog that is never coming.
fn screenshot_bind(app: &App) -> Element<'_, Message> {
    match app.screenshot_control() {
        HotkeyControl::Snippet { text: snippet } => column![
            text(
                "Native channel only: this action has no portal shortcut, so a compositor \
                 bind is the only way to reach it. Paste this into your compositor's config:"
            ),
            container(text(snippet).font(Font::MONOSPACE).size(13)).padding(8),
            button("Copy screenshot bind").on_press(Message::CopyScreenshotBind),
        ]
        .spacing(6)
        .into(),
        // Unreachable: `screenshot_control` always asks as
        // `HotkeyChannel::Native`, precisely so this row can never claim
        // a portal key. Rendered honestly rather than unwrapped.
        HotkeyControl::Rebind { .. } => {
            text("The mining screenshot has no portal shortcut; bind it in your compositor.")
                .size(13)
                .into()
        }
        HotkeyControl::NoChord => text(
            "No screenshot chord is set, so there is no bind to copy - type one above. \
             Adding a card can still take a picture with the checkbox above."
        )
        .size(13)
        .into(),
    }
}

/// The mining screenshot's group: the gate that puts a picture on an
/// add, where the PNG lands, and the chord for taking one on its own.
///
/// Every row is shown whatever the gate says, unlike Windows' Anki tab:
/// the folder and the chord both matter to the standalone screenshot
/// action, which `include_on_add` has nothing to do with.
fn screenshot_rows(app: &App) -> Vec<Element<'_, Message>> {
    vec![
        checkbox(app.form.include_screenshot)
            .label("Include screenshot when adding")
            .on_toggle(Message::IncludeScreenshot)
            .into(),
        text(
            "Asking for a card dims the screen: drag the area to capture, release to \
             confirm. Esc or a right-click skips the picture and files the card without \
             one. Add a field mapping below with source \"screenshot\" to say which Anki \
             field the picture lands in."
        )
        .size(13)
        .into(),
        labeled(
            "Screenshots folder",
            text_input("screenshots", &app.linux.screenshot_save_dir)
                .on_input(Message::ScreenshotSaveDir)
                .width(260),
        ),
        text(
            "An absolute path is taken as typed. A relative one lands under your XDG data \
             directory, or beside the executable in portable mode."
        )
        .size(13)
        .into(),
        labeled(
            "Mining screenshot chord",
            text_input(
                "SUPER+S",
                app.linux.screenshot_key_linux.as_deref().unwrap_or_default(),
            )
            .on_input(Message::ScreenshotKey)
            .width(200),
        ),
        screenshot_bind(app),
    ]
}

/// The source a picked item names, or `None` when core's vocabulary
/// does not hold it.
///
/// [`FIELD_SOURCES`] is one ordered sequence serving both halves of the
/// UI edge - the items going out and the source coming back - so nothing
/// in between gets to decide the mapping, exactly as [`SENTENCE_MODES`]
/// does for the sentence picker. `None` is unreachable from the picker;
/// it is what a source hand-written into the TOML gets, and it is why
/// such a row renders unset rather than dressing an unknown string up
/// as a mapping core would honour.
fn field_source_of(picked: &str) -> Option<&'static str> {
    FIELD_SOURCES.iter().copied().find(|&source| source == picked)
}

/// The source a freshly added row starts on.
///
/// `screenshot`, not the vocabulary's first entry: `default_field_map`
/// ships no row for `glossary_html`, `sentence` or `screenshot`, and of
/// those three only `screenshot`'s absence makes another setting inert -
/// `actions.screenshot.include_on_add` can be on and still put no
/// picture anywhere, because `shot::plan_add` reads the picture's field
/// name straight off this row. That gap is the whole reason this section
/// grew an Add button, so it is what Add guesses; a wrong guess costs
/// one pick.
const NEW_ROW_SOURCE: &str = "screenshot";

/// The field-map group: one row per mapping, and the two controls that
/// make the list growable.
///
/// Windows builds its rows from a live AnkiConnect `modelFieldNames`
/// call - one combo per field the note type actually has
/// (`ui/settings_window.rs`) - and this deliberately does not. Fetching
/// makes the row list a property of whichever model happened to be
/// reachable, which is why the Windows window drops a config row for a
/// field the fetched model lacks: a silent edit to a saved mapping
/// whenever Anki is closed or pointed at another note type. These rows
/// are the config's rows, so an explicit Add/Remove pair is both simpler
/// and free of that failure mode. The cost is that the Anki field name
/// is typed rather than picked; the source, which is core's closed
/// vocabulary and not the user's, is picked.
fn field_map_rows(app: &App) -> Vec<Element<'_, Message>> {
    let mut rows: Vec<Element<'_, Message>> = app
        .form
        .field_map
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .enumerate()
        .map(|(i, mapping)| {
            row![
                text_input("Anki field", &mapping.anki_field)
                    .on_input(move |v| Message::FieldMapAnki(i, v))
                    .width(220),
                text("<-").size(14),
                pick_list(FIELD_SOURCES, field_source_of(&mapping.source), move |source| {
                    Message::FieldMapSource(i, source.to_string())
                })
                .placeholder("source")
                .width(220),
                button("Remove").on_press(Message::FieldMapRemove(i)),
            ]
            .spacing(10)
            .align_y(iced::Center)
            .into()
        })
        .collect();
    rows.push(button("Add field mapping").on_press(Message::FieldMapAdd).into());
    rows.push(
        text(format!(
            "A new row arrives on \"{NEW_ROW_SOURCE}\", the one source the shipped \
             defaults leave out; type the Anki field it belongs in, or pick another \
             source. A row with no field name is dropped on Apply."
        ))
        .size(13)
        .into(),
    );
    rows
}

fn anki_section(app: &App) -> Element<'_, Message> {
    // The add-card chord's own hotkey control, the same shape the
    // trigger row has: on the native rung the compositor bind is the
    // only thing that can reach the add at all (ADR-0003 rung 2 plus
    // its 2026-08-26 addendum), so a row without a copyable bind was a
    // chord the user could type and never bind.
    let add_bind: Element<'_, Message> = match app.add_control() {
        HotkeyControl::Snippet { text: snippet } => column![
            text("Native channel: your compositor owns this binding. Paste this into its config:"),
            container(text(snippet).font(Font::MONOSPACE).size(13)).padding(8),
            button("Copy add-card bind").on_press(Message::CopyAddBind),
        ]
        .spacing(6)
        .into(),
        // The portal rung, with the key the portal published for the
        // *add-card* id (ticket 09) - the same status vocabulary the
        // trigger row uses, because it is the same fact about a
        // different action. `current: None` is the honest absence: a
        // desktop that reports no key, or one that never answered for
        // this id.
        HotkeyControl::Rebind { current } => column![
            text("Portal channel: the GlobalShortcuts portal owns this binding."),
            text(match &current {
                Some(key) => format!("Current key: {key}"),
                None => "Current key: your desktop does not report one - open its global-shortcuts settings to see or change it".to_string(),
            }),
            text(
                "The chord above is the preferred add-card key, offered to the portal at the next start; your desktop's shortcut editor has the last word."
            )
            .size(13),
        ]
        .spacing(6)
        .into(),
        HotkeyControl::NoChord => text(
            "No add-card chord is set, so there is no bind to copy - type one above."
        )
        .size(13)
        .into(),
    };

    let body = column![
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
        add_bind,
        // Windows' "First dictionary only" (`ui/settings_window.rs`),
        // the same field on the same form: the daemon already honours
        // `anki.first_dict_only`, so without this row the only way to
        // use it was hand-editing the TOML.
        checkbox(app.form.first_dict_only)
            .label("First dictionary only")
            .on_toggle(Message::FirstDictOnly),
        column(screenshot_rows(app)).spacing(10),
        column(sentence_rows(app)).spacing(10),
        text("Field mappings").size(14),
        column(field_map_rows(app)).spacing(10),
    ]
    .spacing(10);
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

/// The installed families the combo offers: fontdb's JP-capable ones
/// (ADR-0005), sorted and deduplicated, owned by the process. The
/// filter is the popup's own classifier ([`popup::jp_capable`]) and not
/// a second marker table, so both halves of the product agree on what
/// "draws Japanese" means.
///
/// A `static` because iced's [`iced::font::Family::Name`] takes a
/// `&'static str`: the preview label has to name the family it paints
/// with, and these names are the only honest source of one.
static FAMILIES: LazyLock<Vec<String>> = LazyLock::new(|| {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let mut names: Vec<String> = db.faces().map(|face| face.families[0].0.clone()).collect();
    names.sort();
    names.dedup();
    offered(names)
});

/// The names [`FAMILIES`] keeps, and what happens when none of them
/// draws Japanese.
///
/// A machine with no Japanese face gets the whole list back: an empty
/// combo is a control the user cannot use at all, and the popup meets
/// the same machine by painting anyway and naming the missing package
/// (ADR-0004's degrade-visibly posture) rather than by refusing.
///
/// Pure, so the filter is testable without a font stack - the property
/// the popup's classifier tables are written for.
fn offered(all: Vec<String>) -> Vec<String> {
    let (jp, rest): (Vec<String>, Vec<String>) =
        all.into_iter().partition(|name| popup::jp_capable(name));
    if jp.is_empty() {
        rest
    } else {
        jp
    }
}

/// The font combo's items.
///
/// `Borrowed` is one of [`FAMILIES`], so iced can be told to paint the
/// preview with it. `Owned` is the configured literal the combo would
/// not otherwise offer - uninstalled, or installed but not a family
/// that draws Japanese. It is still offered and still selected
/// (ADR-0012: no sentinel semantics); it just previews as iced's
/// default, which is where a family with no kanji in it would have
/// ended up glyph by glyph anyway.
fn font_items(configured: &str) -> Vec<Cow<'static, str>> {
    let mut items: Vec<Cow<'static, str>> =
        FAMILIES.iter().map(|f| Cow::Borrowed(f.as_str())).collect();
    if !items.iter().any(|f| f == configured) {
        items.insert(0, Cow::Owned(configured.to_string()));
    }
    items
}

/// The combo item the form's font names.
///
/// [`font_items`] guarantees a hit for the font the window opened with;
/// after that every write to `form.font` comes from a picked item, so
/// `None` is unreachable and only means "nothing to preview with".
fn selected_family(app: &App) -> Option<&Cow<'static, str>> {
    app.fonts.iter().find(|f| f.as_ref() == app.form.font.as_str())
}

/// The family the preview paints with: the selection when the combo
/// offers that name itself, iced's default when it is the configured
/// literal ([`font_items`]).
fn preview_font(selected: Option<&Cow<'static, str>>) -> Font {
    match selected {
        Some(Cow::Borrowed(name)) => Font::with_name(name),
        _ => Font::DEFAULT,
    }
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

    /// The rows the window is editing. Linux's rows are the config's
    /// rows, so this form always has an answer about the field map;
    /// `None` is the Windows-only state where AnkiConnect never named
    /// the fields (`chibipop::settings::SettingsForm::field_map`).
    fn form_rows(app: &App) -> &[FieldMapping] {
        app.form.field_map.as_deref().expect("a Linux form always has field-map rows")
    }

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
            add_channel: HotkeyChannel::Native,
            library_dir: dir.join("library"),
            db_path: dir.join("chibipop.sqlite"),
            runtime_dir: dir.join("run"),
            autostart: None,
            home: None,
            exe: PathBuf::from("/usr/bin/chibipop"),
            // A session that can copy, so the OCR-to-clipboard row's
            // chord half is what a test drives; the no-protocol case is
            // asserted by setting this to `None`.
            clipboard_rung: Some(clipboard::Rung::Wlr),
            autostart_on: false,
            fonts: vec![Cow::Borrowed("Noto Sans")],
            capture_w: "480".to_string(),
            capture_h: "160".to_string(),
            selected_dict: None,
            add_path: String::new(),
            picking: false,
            rebuild_progress: None,
            checking_update: false,
            status: String::new(),
        }
    }

    /// The add-card row carries the trigger row's status vocabulary
    /// (ticket 09): on the portal rung it names the key the portal
    /// published for `anki-add` and offers no snippet, because a
    /// pasted compositor bind is not what owns that key.
    #[test]
    fn the_add_card_row_reports_the_portals_own_add_key() {
        let dir = scratch("addportalrow");
        let mut app = app(&dir);
        app.add_channel = HotkeyChannel::Portal { current_binding: Some("Meta+A".into()) };

        assert_eq!(
            HotkeyControl::Rebind { current: Some("Meta+A".into()) },
            app.add_control()
        );
        // And no copy button, so a portal row cannot hand out a bind
        // that would not be the one in force.
        assert_eq!(None, app.add_bind_snippet());

        // The trigger row keeps its own key: two rows, two channels.
        app.channel = HotkeyChannel::Portal { current_binding: Some("Meta+F".into()) };
        assert_eq!(
            HotkeyControl::Rebind { current: Some("Meta+F".into()) },
            app.channel.control(
                app.compositor,
                &app.linux.trigger_key_linux,
                &app.exe,
                snippets::Bind::Hold,
            )
        );
        // The whole window still builds a widget tree with both rows on
        // the portal rung: the status block is real widgetry, not just a
        // control value.
        let _ = view(&app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No daemon has published anything, so the add row falls back to
    /// the affordance a user can act on: the pasteable bind.
    #[test]
    fn a_silent_daemon_leaves_the_add_card_row_offering_a_snippet() {
        let dir = scratch("addsnippetrow");
        let app = app(&dir);
        assert!(
            matches!(app.add_control(), HotkeyControl::Snippet { .. }),
            "got {:?}",
            app.add_control()
        );
        assert!(app.add_bind_snippet().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The checkbox is only worth having if it reaches the file: the
    /// toggle must land on `anki.first_dict_only`, which the Linux
    /// daemon already reads.
    #[test]
    fn the_first_dictionary_only_checkbox_round_trips_into_the_config() {
        let dir = scratch("firstdict");
        let mut app = app(&dir);
        let cfg = chibipop::config::Config::default();
        assert!(!app.form.first_dict_only, "the default is every dictionary");

        let _ = update(&mut app, Message::FirstDictOnly(true));
        assert!(chibipop::settings::apply_to(&app.form, &cfg).anki.first_dict_only);

        let _ = update(&mut app, Message::FirstDictOnly(false));
        assert!(!chibipop::settings::apply_to(&app.form, &cfg).anki.first_dict_only);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The OCR-to-clipboard row's whole point on the native rung: the
    /// typed chord comes back as a bind naming the running binary and the
    /// `ocr-clipboard` verb, and never as a portal key - this action has
    /// no portal id (D1), so borrowing the trigger's would be a row
    /// claiming a key it was never given.
    #[test]
    fn the_ocr_clipboard_chord_offers_a_pasteable_native_bind() {
        let dir = scratch("ocrclipbind");
        let mut app = app(&dir);
        app.channel = HotkeyChannel::Portal { current_binding: Some("Meta+F".into()) };

        let _ = update(&mut app, Message::OcrClipboardKey("ALT+C".to_string()));
        let snippet = app.ocr_clipboard_bind_snippet().expect("a typed chord has a bind");

        assert_eq!("bind = ALT, C, exec, /usr/bin/chibipop ctl ocr-clipboard", snippet);
        assert!(
            !snippet.contains("Meta+F"),
            "the trigger's portal key must never reach this row: {snippet}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A cleared box is an absent `Option`, not a `""` chord: the config
    /// field is `Option<String>` precisely so absence stays typed
    /// (`upstream-merge-fallout` ticket 06), and the row offers no bind.
    #[test]
    fn a_cleared_ocr_clipboard_chord_is_absent_rather_than_an_empty_string() {
        let dir = scratch("ocrclipclear");
        let mut app = app(&dir);

        let _ = update(&mut app, Message::OcrClipboardKey("ALT+C".to_string()));
        assert_eq!(Some("ALT+C".to_string()), app.linux.ocr_clipboard_key_linux);

        let _ = update(&mut app, Message::OcrClipboardKey("   ".to_string()));
        assert_eq!(None, app.linux.ocr_clipboard_key_linux, "whitespace is not a chord");
        assert_eq!(None, app.ocr_clipboard_bind_snippet());
        // And the copy message is inert rather than pasting a stale bind.
        let _ = update(&mut app, Message::CopyOcrClipboardBind);
        assert_eq!(None, app.ocr_clipboard_bind_snippet());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Stock GNOME: a chord that could only ever log a refusal is not a
    /// bind this window hands out (ADR-0005), so the row withholds it
    /// even though the chord itself is perfectly well typed.
    #[test]
    fn a_session_with_no_clipboard_protocol_offers_no_ocr_clipboard_bind() {
        let dir = scratch("ocrclipnoproto");
        let mut app = app(&dir);
        let _ = update(&mut app, Message::OcrClipboardKey("ALT+C".to_string()));
        assert!(app.ocr_clipboard_bind_snippet().is_some(), "a session that can copy offers one");

        app.clipboard_rung = None;

        assert_eq!(None, app.ocr_clipboard_bind_snippet());
        // The chord is still carried: a GNOME user who later moves to a
        // compositor with data control must not lose what they typed.
        assert_eq!(Some("ALT+C".to_string()), app.linux.ocr_clipboard_key_linux);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The add-card row's whole point: on the native rung the chord the
    /// user typed comes back as a bind they can paste, naming the
    /// running binary and the `anki-add` verb. Without this the chord
    /// was uneditable into anything (ADR-0003 rung 2 is the only rung a
    /// sway session has).
    #[test]
    fn the_add_card_chord_offers_a_pasteable_bind_for_the_typed_chord() {
        let dir = scratch("addbind");
        let mut app = app(&dir);

        let _ = update(&mut app, Message::AnkiAddKey("CTRL+SHIFT+A".to_string()));
        let snippet = app.add_bind_snippet().expect("a chord has a bind");

        assert_eq!("bind = CTRL SHIFT, A, exec, /usr/bin/chibipop ctl anki-add", snippet);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A cleared chord has nothing to bind, so the row must say so
    /// rather than hand out `bind = , A, …`.
    #[test]
    fn a_cleared_add_card_chord_offers_no_bind_at_all() {
        let dir = scratch("addnobind");
        let mut app = app(&dir);

        let _ = update(&mut app, Message::AnkiAddKey(String::new()));

        assert_eq!(HotkeyControl::NoChord, app.add_control());
        assert_eq!(None, app.add_bind_snippet());
        // And the copy message is inert rather than pasting a stale one.
        let _ = update(&mut app, Message::CopyAddBind);
        assert_eq!(None, app.add_bind_snippet());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The trigger row is untouched by all of it: the default chord
    /// still produces the hold pair, byte for byte.
    #[test]
    fn the_trigger_row_still_hands_out_the_hold_pair() {
        let dir = scratch("holdpair");
        let app = app(&dir);
        assert_eq!(
            "bind = ALT, F, exec, /usr/bin/chibipop ctl trigger-down\n\
             bindr = ALT, F, exec, /usr/bin/chibipop ctl trigger-up\n\
             # Release F before ALT - Hyprland drops modifier-first releases (hyprwm/Hyprland#5032).\n\
             # If the popup sticks, tap the chord again (release F first), or bind `ctl toggle` instead.",
            app.bind_snippet()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The static-region row's whole point: the chord the user typed
    /// comes back as a bind naming the running binary and the
    /// `static-region` verb. Without it a shipped, editable chord could
    /// not be bound at all, because this action has no portal rung to
    /// fall back on.
    #[test]
    fn the_static_region_chord_offers_a_pasteable_bind_for_the_typed_chord() {
        let dir = scratch("srbind");
        let mut app = app(&dir);

        let _ = update(&mut app, Message::StaticRegionKey("ALT+R".to_string()));
        let snippet = app.static_region_bind_snippet().expect("a chord has a bind");

        assert_eq!("bind = ALT, R, exec, /usr/bin/chibipop ctl static-region", snippet);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Decision D1, in the one place a user could notice it broken: this
    /// action is never portal-registered, so its row asks as
    /// `HotkeyChannel::Native` however the *trigger* resolved. A row that
    /// read `app.channel` would print the trigger's portal key under the
    /// static-region chord - a window claiming a key it was never given.
    #[test]
    fn the_static_region_row_never_borrows_the_portals_trigger_key() {
        let dir = scratch("srnative");
        let mut app = app(&dir);
        app.channel = HotkeyChannel::Portal { current_binding: Some("Meta+F".into()) };
        app.add_channel = HotkeyChannel::Portal { current_binding: Some("Meta+A".into()) };

        let _ = update(&mut app, Message::StaticRegionKey("ALT+R".to_string()));

        assert_eq!(
            HotkeyControl::Snippet {
                text: "bind = ALT, R, exec, /usr/bin/chibipop ctl static-region".to_string()
            },
            app.static_region_control(),
            "a portal session must not change what this row offers"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The default is unset (`anki.static_region_key_linux` ships
    /// empty), so out of the box the row has nothing to copy and says so
    /// rather than handing out `bind = , R, …`.
    #[test]
    fn an_unset_static_region_chord_offers_no_bind_at_all() {
        let dir = scratch("srnobind");
        let mut app = app(&dir);

        assert_eq!("", app.linux.static_region_key_linux, "the shipped default is unset");
        assert_eq!(HotkeyControl::NoChord, app.static_region_control());
        assert_eq!(None, app.static_region_bind_snippet());
        // Inert rather than pasting a stale bind.
        let _ = update(&mut app, Message::CopyStaticRegionBind);
        assert_eq!(None, app.static_region_bind_snippet());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The screenshot row's own bind, and the `Option` at its edge: the
    /// text box is the only place `""` becomes `None`, so a typed chord
    /// lands as `Some` and a cleared one as absence - never as an
    /// empty-string sentinel in the config file.
    #[test]
    fn the_screenshot_chord_offers_a_pasteable_bind_and_clears_to_none() {
        let dir = scratch("shotbind");
        let mut app = app(&dir);

        assert_eq!(None, app.linux.screenshot_key_linux, "the shipped default is unset");
        assert_eq!(HotkeyControl::NoChord, app.screenshot_control());

        let _ = update(&mut app, Message::ScreenshotKey("SUPER+S".to_string()));
        assert_eq!(Some("SUPER+S".to_string()), app.linux.screenshot_key_linux);
        assert_eq!(
            "bind = SUPER, S, exec, /usr/bin/chibipop ctl screenshot",
            app.screenshot_bind_snippet().expect("a chord has a bind")
        );

        let _ = update(&mut app, Message::ScreenshotKey("   ".to_string()));
        assert_eq!(None, app.linux.screenshot_key_linux, "blank is absence, not an empty chord");
        assert_eq!(None, app.screenshot_bind_snippet());
        // Inert rather than pasting a stale bind.
        let _ = update(&mut app, Message::CopyScreenshotBind);
        assert_eq!(None, app.screenshot_bind_snippet());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Decision D1 again, for this row: never portal-registered, so it
    /// asks as `HotkeyChannel::Native` however the trigger resolved.
    #[test]
    fn the_screenshot_row_never_borrows_the_portals_trigger_key() {
        let dir = scratch("shotnative");
        let mut app = app(&dir);
        app.channel = HotkeyChannel::Portal { current_binding: Some("Meta+F".into()) };
        app.add_channel = HotkeyChannel::Portal { current_binding: Some("Meta+A".into()) };

        let _ = update(&mut app, Message::ScreenshotKey("SUPER+S".to_string()));

        assert_eq!(
            HotkeyControl::Snippet {
                text: "bind = SUPER, S, exec, /usr/bin/chibipop ctl screenshot".to_string()
            },
            app.screenshot_control(),
            "a portal session must not change what this row offers"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The two rows beside the chord: the gate is core's own form field,
    /// and an emptied folder box means the default folder rather than
    /// the data directory itself (a relative `save_dir` is joined onto
    /// it, so `""` would scatter PNGs among the database).
    #[test]
    fn the_screenshot_rows_apply_the_gate_and_never_save_an_empty_folder() {
        let dir = scratch("shotrows");
        let mut app = app(&dir);
        let cfg = chibipop::config::Config::default();

        let _ = update(&mut app, Message::IncludeScreenshot(true));
        assert!(chibipop::settings::apply_to(&app.form, &cfg).actions.screenshot.include_on_add);

        let _ = update(&mut app, Message::ScreenshotSaveDir("  /tmp/mining  ".to_string()));
        let mut out = chibipop::settings::apply_to(&app.form, &cfg);
        app.linux.apply_over(&mut out);
        assert_eq!("/tmp/mining", out.actions.screenshot.save_dir, "trimmed, as typed");

        let _ = update(&mut app, Message::ScreenshotSaveDir(String::new()));
        let mut out = chibipop::settings::apply_to(&app.form, &cfg);
        app.linux.apply_over(&mut out);
        assert_eq!(
            cfg.actions.screenshot.save_dir, out.actions.screenshot.save_dir,
            "a cleared box falls back to the shipped folder"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The picker is an ordered table, so a label round-trips to its own
    /// mode and nothing between the two halves gets to decide. Every
    /// `SentenceMode` has to be in it: a mode the table forgot would be
    /// unreachable in the window and would silently read as `Line`.
    #[test]
    fn every_sentence_mode_round_trips_through_its_own_label() {
        for mode in [SentenceMode::Line, SentenceMode::All, SentenceMode::Static] {
            let label = sentence_mode_label(mode);
            assert_eq!(mode, sentence_mode_of(label), "{label}");
            assert!(sentence_labels().iter().any(|l| l == label), "{label} must be offered");
        }
        assert_eq!(3, sentence_labels().len(), "the table is the whole list");
    }

    /// Picking *Static region* stages the mode on the shared form, which
    /// is what Apply then writes and the daemon reads.
    #[test]
    fn picking_the_static_region_mode_stages_it_on_the_form() {
        let dir = scratch("srmode");
        let mut app = app(&dir);
        assert_eq!(SentenceMode::Line, app.form.sentence_mode, "the shipped default");

        let _ = update(&mut app, Message::SentenceModePicked("Static region".to_string()));
        assert_eq!(SentenceMode::Static, app.form.sentence_mode);

        let _ = update(&mut app, Message::ShowStaticOverlay(false));
        assert!(!app.form.show_static_overlay);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Windows hides its region rows unless the mode is Static, and so
    /// does this - except the chord row, because the region can be set in
    /// any mode and hiding the only way to bind the key until the user
    /// had already switched would be a chicken and egg. Row *count* is
    /// the observable: iced widgets are opaque, and what matters is which
    /// rows exist.
    #[test]
    fn the_outline_checkbox_is_static_only_but_the_chord_row_always_shows() {
        let dir = scratch("srrows");
        let mut app = app(&dir);

        // Picker, chord, bind text: three rows and no checkbox.
        assert_eq!(3, sentence_rows(&app).len(), "not static: no region checkbox");

        let _ = update(&mut app, Message::SentenceModePicked("Static region".to_string()));
        // Plus the checkbox and its layer-shell caption.
        assert_eq!(5, sentence_rows(&app).len(), "static: the checkbox joins");

        // And the chord row is still there in the non-static case, which
        // is the deliberate divergence from Windows.
        let _ = update(&mut app, Message::SentenceModePicked("All lines".to_string()));
        let _ = update(&mut app, Message::StaticRegionKey("ALT+R".to_string()));
        assert!(
            app.static_region_bind_snippet().is_some(),
            "the region can be set in any mode, so its bind is offered in any mode"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The ticket: until this window could add a row and pick a source,
    /// `shot::plan` had nothing to find and a Linux user's only way to
    /// name the picture's field was hand-editing the TOML. Asserted the
    /// way `plan` reads it - the first row whose source is `screenshot`,
    /// and the Anki field named on that row.
    #[test]
    fn adding_a_row_can_finally_name_the_screenshot_field() {
        let dir = scratch("fmadd");
        let mut app = app(&dir);
        assert!(
            !form_rows(&app).iter().any(|m| m.source == "screenshot"),
            "the shipped default_field_map has no screenshot row - that is the gap"
        );

        let _ = update(&mut app, Message::FieldMapAdd);
        let at = form_rows(&app).len() - 1;
        // The seed is a pickable source, so the row means something
        // before the user has touched the picker at all.
        assert_eq!(Some(NEW_ROW_SOURCE), field_source_of(&form_rows(&app)[at].source));
        let _ = update(&mut app, Message::FieldMapAnki(at, "Picture".to_string()));

        assert_eq!(
            Some("Picture".to_string()),
            form_rows(&app)
                .iter()
                .find(|m| m.source == "screenshot")
                .map(|m| m.anki_field.clone()),
            "this is the row and the field name `shot::plan` looks for"
        );

        // And the picker moves that row to any other source without the
        // typed field name following it.
        let _ = update(&mut app, Message::FieldMapSource(at, "sentence".to_string()));
        assert_eq!("sentence", form_rows(&app)[at].source);
        assert_eq!("Picture", form_rows(&app)[at].anki_field);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An off-by-one here silently destroys a mapping the user still
    /// wants, so the survivors are asserted by name and not by count.
    #[test]
    fn removing_a_row_takes_the_one_that_was_pressed() {
        let dir = scratch("fmremove");
        let mut app = app(&dir);
        app.form.field_map = Some(Vec::new());
        for name in ["First", "Middle", "Last"] {
            let _ = update(&mut app, Message::FieldMapAdd);
            let at = form_rows(&app).len() - 1;
            let _ = update(&mut app, Message::FieldMapAnki(at, name.to_string()));
        }

        let _ = update(&mut app, Message::FieldMapRemove(1));
        let names: Vec<&str> = form_rows(&app).iter().map(|m| m.anki_field.as_str()).collect();
        assert_eq!(vec!["First", "Last"], names);

        // A press that arrived after its row was already gone is inert
        // rather than a `Vec::remove` panic in the UI thread.
        let _ = update(&mut app, Message::FieldMapRemove(9));
        assert_eq!(2, form_rows(&app).len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A row the user added and never named cannot be stored: Anki has
    /// no field called "", so such a row is a mapping that looks set and
    /// can never place anything. Driven through Apply, the one place
    /// that filter lives, and read back off the file Apply wrote.
    #[test]
    fn a_row_with_no_anki_field_never_reaches_the_saved_config() {
        let dir = scratch("fmblank");
        let mut app = app(&dir);
        let shipped = form_rows(&app).len();

        let _ = update(&mut app, Message::FieldMapAdd);
        let named = form_rows(&app).len() - 1;
        let _ = update(&mut app, Message::FieldMapAnki(named, "Picture".to_string()));
        // One row the user opened and walked away from, and one they
        // filled with spaces, which is no more a field name than "".
        let _ = update(&mut app, Message::FieldMapAdd);
        let _ = update(&mut app, Message::FieldMapAdd);
        let blank = form_rows(&app).len() - 1;
        let _ = update(&mut app, Message::FieldMapAnki(blank, "   ".to_string()));

        let _ = update(&mut app, Message::Apply);
        let saved = chibipop::config::load_or_create(&app.config_path).expect("Apply wrote it");
        assert_eq!(
            shipped + 1,
            saved.anki.field_map.len(),
            "the named row landed and the two nameless ones did not: {:?}",
            saved.anki.field_map
        );
        assert_eq!(
            Some("Picture".to_string()),
            saved
                .anki
                .field_map
                .iter()
                .find(|m| m.source == "screenshot")
                .map(|m| m.anki_field.clone()),
            "the sibling rows are untouched and the screenshot row is the saved one"
        );
        // The window shows what the file holds, the same way Apply
        // re-reads the clamped capture size.
        assert_eq!(shipped + 1, form_rows(&app).len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Ticket 20: removing every row is an answer, not a window that
    /// never had rows to show, so Apply writes the empty map instead of
    /// silently keeping the shipped one. Read off the file Apply wrote,
    /// because the guard this replaced was invisible from inside the
    /// form - the save reported success either way.
    #[test]
    fn removing_every_row_saves_an_empty_map() {
        let dir = scratch("fmempty");
        let mut app = app(&dir);
        assert!(!form_rows(&app).is_empty(), "the shipped map is what gets emptied");
        for _ in 0..form_rows(&app).len() {
            let _ = update(&mut app, Message::FieldMapRemove(0));
        }

        let _ = update(&mut app, Message::Apply);
        let saved = chibipop::config::load_or_create(&app.config_path).expect("Apply wrote it");
        assert!(saved.anki.field_map.is_empty(), "the user removed every row; the file says so");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The vocabulary is core's [`FIELD_SOURCES`] and this window keeps
    /// no second copy: every item it offers round-trips to itself, and a
    /// source the set does not hold never reaches the form -
    /// `anki::mapped_fields` drops such a row without a word, so a
    /// picker that accepted one would be showing a mapping that is not.
    #[test]
    fn the_source_picker_only_ever_yields_a_source_core_understands() {
        let dir = scratch("fmvocab");
        let mut app = app(&dir);
        for source in FIELD_SOURCES {
            assert_eq!(Some(source), field_source_of(source), "{source} must be offered");
        }
        // Windows' combo prepends this for "unmapped": that one UI's
        // idiom for no row, never a storable source.
        assert_eq!(None, field_source_of("(none)"));
        assert_eq!(None, field_source_of("sceenshot"));

        let _ = update(&mut app, Message::FieldMapAdd);
        let at = form_rows(&app).len() - 1;
        let _ = update(&mut app, Message::FieldMapSource(at, "sceenshot".to_string()));
        assert_eq!(
            NEW_ROW_SOURCE, form_rows(&app)[at].source,
            "an unlisted source leaves the row on the one it had"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Row *count* is the observable, as with the sentence rows: iced
    /// widgets are opaque, and what matters is that every mapping is
    /// rendered and that the affordances the section grew - Add and its
    /// caption - are there even when the list is empty, which is the
    /// state a user who removed everything is left in.
    #[test]
    fn every_mapping_is_rendered_and_the_add_control_always_is() {
        let dir = scratch("fmrows");
        let mut app = app(&dir);
        let shipped = form_rows(&app).len();
        assert_eq!(
            shipped + 2,
            field_map_rows(&app).len(),
            "one row per mapping, plus Add and its caption"
        );

        let _ = update(&mut app, Message::FieldMapAdd);
        assert_eq!(shipped + 3, field_map_rows(&app).len(), "the new row renders at once");

        app.form.field_map = Some(Vec::new());
        assert_eq!(2, field_map_rows(&app).len(), "an emptied map still offers Add");

        // The whole window builds with a screenshot row in the map: the
        // picker is real widgetry, not just a lookup.
        let _ = update(&mut app, Message::FieldMapAdd);
        let _ = view(&app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("chibipop_settings_app_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("library")).unwrap();
        std::fs::create_dir_all(dir.join("run")).unwrap();
        dir
    }

    /// The regression: the preview label used to render in iced's
    /// default font no matter what the combo said.
    #[test]
    fn picking_an_installed_family_repaints_the_preview_with_it() {
        let dir = scratch("font");
        let mut app = app(&dir);
        app.fonts = font_items("DejaVu Sans");
        let picked = app
            .fonts
            .iter()
            .find(|f| matches!(f, Cow::Borrowed(_)))
            .expect("no offered family to pick")
            .clone();

        let _ = update(&mut app, Message::FontPicked(picked.clone()));

        assert_eq!(picked.as_ref(), app.form.font);
        assert_eq!(
            Font::with_name(match picked {
                Cow::Borrowed(name) => name,
                Cow::Owned(_) => unreachable!("filtered to Borrowed above"),
            }),
            preview_font(selected_family(&app)),
            "the preview must name the family the combo shows"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A font the config names but no face carries stays selectable, and
    /// the preview says so by falling back instead of naming a family
    /// iced cannot resolve.
    #[test]
    fn an_uninstalled_configured_family_is_offered_and_previews_as_default() {
        let dir = scratch("font_absent");
        let mut app = app(&dir);
        app.form.font = "No Such Family 12345".to_string();
        app.fonts = font_items(&app.form.font);

        assert_eq!(Some(&Cow::Owned(app.form.font.clone())), selected_family(&app));
        assert_eq!(Font::DEFAULT, preview_font(selected_family(&app)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ADR-0005: the combo is populated from the JP-capable families,
    /// and it borrows the popup's classifier to decide - a Chinese pan-CJK
    /// face and a Latin "Gothic" are both out.
    #[test]
    fn the_font_combo_offers_only_families_that_draw_japanese() {
        let installed =
            ["DejaVu Sans", "Noto Sans CJK JP", "Noto Sans CJK SC", "IPAGothic", "Century Gothic"];

        assert_eq!(
            vec!["Noto Sans CJK JP".to_string(), "IPAGothic".to_string()],
            offered(installed.iter().map(|f| (*f).to_string()).collect()),
        );
    }

    /// No Japanese face installed is the popup's degrade-visibly case,
    /// not a reason to hand the user a combo with nothing in it.
    #[test]
    fn a_machine_with_no_japanese_face_is_offered_every_family() {
        let installed: Vec<String> =
            ["DejaVu Sans", "Liberation Serif"].iter().map(|f| (*f).to_string()).collect();

        assert_eq!(installed.clone(), offered(installed));
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

    /// The whole point of the Browse button: one dialog, many archives,
    /// all staged from a single answer.
    #[test]
    fn a_picked_selection_stages_every_archive_in_it() {
        let dir = scratch("picked");
        let mut app = app(&dir);
        app.picking = true;
        let _ = update(
            &mut app,
            Message::DictPicked(Ok(filechooser::Picked::Files(vec![
                fixture("terms.zip"),
                fixture("sweep.zip"),
            ]))),
        );

        assert!(!app.picking, "the Browse button has to come back");
        assert_eq!(2, app.form.staged_adds.len(), "{:?}", app.form.staged_adds);
        assert!(app.status.starts_with("2 archives staged"), "{}", app.status);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A mixed selection stages what it can and names what it did not,
    /// because a status line that only counted successes would leave the
    /// user hunting for the file that never arrived.
    #[test]
    fn a_picked_selection_names_the_files_it_refused() {
        let dir = scratch("picked-mixed");
        let mut app = app(&dir);
        let _ = update(
            &mut app,
            Message::DictPicked(Ok(filechooser::Picked::Files(vec![
                fixture("terms.zip"),
                dir.join("nope.zip"),
            ]))),
        );

        assert_eq!(1, app.form.staged_adds.len(), "{:?}", app.form.staged_adds);
        assert!(app.status.starts_with("1 archive staged"), "{}", app.status);
        assert!(app.status.contains("nope.zip"), "{}", app.status);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A dismissed dialog is not a failure, and it must not read as one.
    #[test]
    fn a_dismissed_dialog_stages_nothing_and_reopens_the_button() {
        let dir = scratch("picked-cancel");
        let mut app = app(&dir);
        app.picking = true;
        // Both halves of the Browse button's gate build a widget tree:
        // a shut button and a live one take different iced paths.
        let _ = view(&app);
        let _ = update(&mut app, Message::DictPicked(Ok(filechooser::Picked::Cancelled)));

        assert!(!app.picking);
        assert!(!app.form.has_staged());
        assert_eq!("No dictionary was chosen.", app.status);
        let _ = view(&app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rebuild can claim the library while the dialog is open, and
    /// staging into a form the builder is already reading is the race
    /// `busy` exists to refuse.
    #[test]
    fn a_selection_that_lands_during_a_rebuild_is_refused_rather_than_staged() {
        let dir = scratch("picked-busy");
        let mut app = app(&dir);
        app.rebuild_progress = Some("Importing…".to_string());
        let _ = update(
            &mut app,
            Message::DictPicked(Ok(filechooser::Picked::Files(vec![fixture("terms.zip")]))),
        );

        assert!(!app.form.has_staged());
        assert!(app.status.contains("rebuild is running"), "{}", app.status);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No portal on this desktop: the window says so in the one place
    /// the user is looking, and the typed path beside it still works.
    #[test]
    fn a_portal_failure_becomes_the_status_line_and_reopens_the_button() {
        let dir = scratch("picked-err");
        let mut app = app(&dir);
        app.picking = true;
        let _ = update(&mut app, Message::DictPicked(Err("no portal here".to_string())));

        assert!(!app.picking);
        assert_eq!("no portal here", app.status);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The expansion itself, on the pure function so no test has to move
    /// the process's `HOME` out from under its siblings.
    #[test]
    fn a_typed_tilde_path_expands_against_home() {
        use crate::paths::{expand_tilde, Typed};
        let home = Path::new("/home/u");
        let h = Some(home);

        assert_eq!(
            Typed::Path(PathBuf::from("/home/u/Downloads/jitendex.zip")),
            expand_tilde("~/Downloads/jitendex.zip", h)
        );
        // Bare `~` and `~/` are the home directory, with no stray
        // trailing separator and no panic.
        assert_eq!(Typed::Path(home.to_path_buf()), expand_tilde("~", h));
        assert_eq!(Typed::Path(home.to_path_buf()), expand_tilde("~/", h));
        // Nothing else is touched: an absolute path and a plain relative
        // one both pass through, and a `~` mid-path is a real file name.
        assert_eq!(Typed::Path(PathBuf::from("/tmp/a.zip")), expand_tilde("/tmp/a.zip", h));
        assert_eq!(Typed::Path(PathBuf::from("dl/a.zip")), expand_tilde("dl/a.zip", h));
        assert_eq!(Typed::Path(PathBuf::from("dl/~/a.zip")), expand_tilde("dl/~/a.zip", h));
        // The two refusals.
        assert_eq!(Typed::UserRelative, expand_tilde("~root/a.zip", h));
        assert_eq!(Typed::NoHome, expand_tilde("~/a.zip", None));
        assert_eq!(Typed::NoHome, expand_tilde("~", None));
    }

    /// End to end through the real message handler: a `~` path stages the
    /// same archive the absolute path does.
    #[test]
    fn adding_a_tilde_path_stages_the_same_archive_the_absolute_path_does() {
        let dir = scratch("add_tilde");
        let mut app = app(&dir);
        // The fixture tree stands in for a home directory; injecting it
        // beats mutating the shared process environment.
        app.home = Some(fixture("terms.zip").parent().unwrap().to_path_buf());

        let _ = update(&mut app, Message::AddPath("~/terms.zip".to_string()));
        let _ = update(&mut app, Message::DictAdd);

        assert!(app.form.dict_names.iter().any(|n| n == "FixtureTerms"), "{:?}", app.form.dict_names);
        assert!(app.form.has_staged(), "the add must wait for a rebuild");
        assert!(app.add_path.is_empty(), "the entry clears once the path is taken");
        assert!(app.status.contains("Rebuild"), "{}", app.status);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Bare `~` is a directory, not an archive: refused by the resolved
    /// path, which is what the message must name.
    #[test]
    fn adding_a_bare_tilde_is_refused_by_its_resolved_path() {
        let dir = scratch("add_bare_tilde");
        let mut app = app(&dir);
        app.home = Some(dir.clone());

        let _ = update(&mut app, Message::AddPath("~".to_string()));
        let _ = update(&mut app, Message::DictAdd);

        assert!(!app.form.has_staged());
        assert!(app.status.contains(&dir.display().to_string()), "{}", app.status);
        assert!(app.status.contains("not a readable dictionary archive"), "{}", app.status);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `~user/…` and a missing `$HOME` each say what is wrong instead of
    /// probing a literal `~` path.
    #[test]
    fn a_user_relative_tilde_and_a_missing_home_are_refused_with_a_reason() {
        let dir = scratch("add_tilde_bad");
        let mut app = app(&dir);
        app.home = Some(dir.clone());

        let _ = update(&mut app, Message::AddPath("~root/terms.zip".to_string()));
        let _ = update(&mut app, Message::DictAdd);
        assert!(!app.form.has_staged());
        assert!(app.status.contains("user-relative"), "{}", app.status);
        assert!(!app.add_path.is_empty(), "a refused path stays in the entry");

        app.home = None;
        let _ = update(&mut app, Message::AddPath("~/terms.zip".to_string()));
        let _ = update(&mut app, Message::DictAdd);
        assert!(!app.form.has_staged());
        assert!(app.status.contains("HOME is unset"), "{}", app.status);
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
            add_channel: HotkeyChannel::Native,
            library_dir: config_home.join("library"),
            db_path: config_home.join("chibipop.sqlite"),
            runtime_dir: config_home.join("run"),
            autostart: autostart::Target::resolve(&env),
            home: env.home.clone(),
            exe: PathBuf::from("/usr/bin/chibipop"),
            clipboard_rung: Some(clipboard::Rung::Wlr),
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
