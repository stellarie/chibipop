//! The Linux settings process owns this iced window.
//! The window renders values from the core `SettingsForm` and `LinuxFields`.
//! It sends changes to the settings process.
//!
//! The surface matches the field groups in the Windows settings window
//! (`crates/chibipop-windows/src/ui/settings_window.rs`).
//! It uses iced controls and hides `ocr.language`.
//! It exposes Linux fields and shows capture exclusion as a snippet.
//!
//! Dictionary controls stage changes in `SettingsForm`.
//! Only [`super::rebuild`] writes the library.
//! `rebuild_progress` marks a rebuild and supplies the status text.

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
    FieldMapping, LayoutMode, PopupLayer, SelectionButtons, SelectionSeparator, SentenceMode,
    TriggerMode, FIELD_SOURCES, MAX_HEIGHT_RANGE, MAX_WIDTH_RANGE, PASSES_RANGE, SUMMARY_RANGE,
};
use chibipop::dict::frequency::RankingStrategy;
use chibipop::library::Role;
use chibipop::present::DictInfo;
use chibipop::settings::{DictRow, SettingsForm};
use iced::widget::{
    button, checkbox, column, container, mouse_area, pick_list, radio, row, rule, scrollable,
    slider, space, stack, text, text_input,
};
use iced::{Element, Font, Length, Point, Task, Theme};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::path::PathBuf;
use std::sync::LazyLock;

/// `super::run` resolves every value before the window opens.
#[derive(Clone)]
pub struct Init {
    pub form: SettingsForm,
    pub linux: LinuxFields,
    pub config_path: PathBuf,
    pub socket_path: PathBuf,
    pub log_path: PathBuf,
    pub compositor: Compositor,
    /// The channel that owns the trigger bind.
    pub channel: HotkeyChannel,
    /// `add_channel` identifies the owner of the add-card bind.
    /// The portal reports one bind per id, so each row needs its own channel
    /// (`super::hotkey_channel`).
    pub add_channel: HotkeyChannel,
    /// The directory that holds dictionary archives. A rebuild edits this directory.
    pub library_dir: PathBuf,
    /// The database path. A rebuild renames the new database over this file.
    pub db_path: PathBuf,
    /// `dicts` contains the Dictionary identities in the database.
    /// `super::read_dicts` reads them before the window opens.
    /// Apply uses the exact names to build the enabled frequency list.
    pub dicts: Vec<DictInfo>,
    /// The directory for the library flock file.
    pub runtime_dir: PathBuf,
    /// `None` means that no XDG config root resolves. The row reports this state.
    pub autostart: Option<autostart::Target>,
    /// `$HOME` expands a typed `~` path. `None` means that HOME is unset.
    pub home: Option<PathBuf>,
    /// `exe` is the binary path for compositor snippets. `paths::exec_name`
    /// resolves it before the window opens. A pasted bind must name this
    /// daemon, not the `chibipop` that PATH finds.
    pub exe: PathBuf,
    /// `clipboard_rung` records the data-control protocol that the session
    /// advertises, if any (`clipboard::rung`). `None` means stock GNOME.
    /// The OCR-to-clipboard row reports this state. It does not show a chord
    /// that only logs a refusal.
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
        .subscription(subscription)
        .window_size((860.0, 760.0))
        .run()
        .context("running the settings window")
}

/// `subscription` handles releases that dictionary rows cannot receive.
///
/// [`mouse_area`] reports a release only under the cursor. If the user
/// releases outside the window, the drag would remain active. The insertion
/// line would then remain visible after the user releases the row.
///
/// A raw listener sees a release at every position. An unfocused window can
/// also lose the pointer during a drag. iced sends widget messages before
/// subscription events for one frame. A drop that the lists received already
/// changed the list, so this message has nothing to cancel.
fn subscription(_app: &App) -> iced::Subscription<Message> {
    iced::event::listen_with(|event, _status, _window| match event {
        iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left))
        | iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::DictReleased),
        _ => None,
    })
}

/// The selected row and the Role list that contains it.
///
/// One selection covers all three lists. Remove deletes a Dictionary from
/// every list (ARCHITECTURE.md#dictionary-and-lookup). A second highlighted
/// row would give Remove two answers. The role stays with the name because
/// Move up under Frequency must not reorder Terms. A name alone cannot
/// identify a list when a mixed archive appears in two lists.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Selected {
    role: Role,
    name: String,
}

/// The height of one dictionary row and the gap between two rows.
///
/// These values stay fixed and do not depend on text or checkbox layout.
/// A drag needs a row position. iced gives [`update`] no layout data, so this
/// window uses the geometry that it requests.
const ROW_HEIGHT: f32 = 28.0;
const ROW_SPACING: f32 = 2.0;

/// The distance from one row top to the next row top.
const ROW_PITCH: f32 = ROW_HEIGHT + ROW_SPACING;

/// The cursor distance that turns a row press into a drag.
/// `pane_grid` uses the same value (`DRAG_DEADBAND_DISTANCE`).
/// Without this guard, click movement would reorder a row.
const DRAG_DEADBAND: f32 = 10.0;

/// The last cursor position inside a dictionary list.
///
/// The window keeps this value while the button is down or up because a press
/// carries no position. [`mouse_area`] sends a plain `on_press` message.
/// The move event before that press supplies its position.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Hover {
    role: Role,
    y: f32,
}

/// The state of a row pointer gesture. This matches `pane_grid::state::Action`.
/// That toolkit type is the only equivalent drag state machine in iced 0.14.
/// iced has no reorderable list widget. `mouse_area` reports presses, moves,
/// and releases but no drag event, so this code combines those messages.
///
/// `role` stays with the held row because a drag can reorder only its source
/// list. `origin` stores the press position in that list. The deadband uses
/// `origin`.
#[derive(Debug, Clone, PartialEq)]
enum Drag {
    Idle,
    Dragging { role: Role, row: String, origin: f32 },
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
    dicts: Vec<DictInfo>,
    runtime_dir: PathBuf,
    autostart: Option<autostart::Target>,
    home: Option<PathBuf>,
    exe: PathBuf,
    /// `clipboard_rung` tells whether this session has a clipboard protocol.
    /// See [`Init::clipboard_rung`].
    clipboard_rung: Option<clipboard::Rung>,
    /// `autostart_on` mirrors the `.desktop` file. The window reads that file after each toggle.
    autostart_on: bool,
    /// The font combo items. See [`font_items`].
    fonts: Vec<Cow<'static, str>>,
    /// Text input keeps numbers as text until Apply parses them.
    capture_w: String,
    capture_h: String,
    /// The row that all three lists select, if any.
    selected: Option<Selected>,
    /// `hover` stores the last cursor position in a dictionary list. See
    /// [`Hover`].
    hover: Option<Hover>,
    /// `drag` stores the row that the pointer holds, if any.
    drag: Drag,
    /// The path in the Add row. This entry stays beside Browse because the
    /// portal does not exist on every desktop. `filechooser::explain` reports
    /// its absence. The entry always shows the file path that it uses.
    add_path: String,
    /// A Browse dialog is open. The portal call waits for a person, so a
    /// separate thread makes the portal call. This flag prevents a second
    /// dialog. Without it, a second click could open a dialog over the first.
    picking: bool,
    /// The last progress line from a rebuild. Empty means idle.
    /// This value also marks the busy state. The window rejects Rebuild and
    /// Apply while it has a value.
    rebuild_progress: Option<String>,
    /// An update check from a click is active. The button stays disabled until
    /// the answer arrives, so a held Enter key cannot start ten checks.
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
            dicts: init.dicts,
            runtime_dir: init.runtime_dir,
            autostart_on: init.autostart.as_ref().is_some_and(autostart::Target::is_enabled),
            autostart: init.autostart,
            home: init.home,
            exe: init.exe,
            clipboard_rung: init.clipboard_rung,
            fonts,
            selected: None,
            hover: None,
            drag: Drag::Idle,
            add_path: String::new(),
            picking: false,
            rebuild_progress: None,
            checking_update: false,
            status: String::new(),
        }
    }

    /// `bind_snippet` returns the trigger row's copyable press and release bind.
    fn bind_snippet(&self) -> String {
        snippets::bind_snippet(
            self.compositor,
            &self.linux.trigger_key_linux,
            &self.exe,
            snippets::Bind::Hold,
        )
    }

    /// The add-card row's copyable bind, or `None` when no chord exists.
    /// The row uses the same value for its button and text, so they cannot disagree.
    fn add_bind_snippet(&self) -> Option<String> {
        match self.add_control() {
            HotkeyControl::Snippet { text } => Some(text),
            HotkeyControl::Rebind { .. } | HotkeyControl::NoChord => None,
        }
    }

    /// The add-card chord row's control. The add action uses a control-socket
    /// verb, so the native rung binds it like the trigger
    /// (ARCHITECTURE.md#input-ladders, 2026-08-26 addendum).
    fn add_control(&self) -> HotkeyControl {
        self.add_channel.control(
            self.compositor,
            &self.linux.add_key_linux,
            &self.exe,
            snippets::Bind::Press(Verb::AnkiAdd),
        )
    }

    /// The static-region chord row's control.
    ///
    /// This method always uses [`HotkeyChannel::Native`]. Decision D1 keeps
    /// this action out of the two GlobalShortcuts ids, so the compositor bind
    /// is its only global channel. If this method read `self.channel`, it
    /// would show the trigger's portal key under this chord. That row would
    /// claim a key that no one assigned to it.
    fn static_region_control(&self) -> HotkeyControl {
        HotkeyChannel::Native.control(
            self.compositor,
            &self.linux.static_region_key_linux,
            &self.exe,
            snippets::Bind::Press(Verb::StaticRegion),
        )
    }

    /// The static-region row's copyable bind, or `None` for a blank chord.
    /// The button exists only for `Some`, so a cleared chord cannot copy an old bind.
    fn static_region_bind_snippet(&self) -> Option<String> {
        match self.static_region_control() {
            HotkeyControl::Snippet { text } => Some(text),
            HotkeyControl::Rebind { .. } | HotkeyControl::NoChord => None,
        }
    }

    /// The mining screenshot chord row's control.
    ///
    /// This method always uses [`HotkeyChannel::Native`] for the reason above.
    /// No portal action registers this chord, so the compositor bind is its
    /// only global channel.
    fn screenshot_control(&self) -> HotkeyControl {
        HotkeyChannel::Native.control(
            self.compositor,
            self.linux.screenshot_key_linux.as_deref().unwrap_or_default(),
            &self.exe,
            snippets::Bind::Press(Verb::Screenshot),
        )
    }

    /// `screenshot_bind_snippet` returns the row's copyable bind, or `None`
    /// when no chord exists.
    fn screenshot_bind_snippet(&self) -> Option<String> {
        match self.screenshot_control() {
            HotkeyControl::Snippet { text } => Some(text),
            HotkeyControl::Rebind { .. } | HotkeyControl::NoChord => None,
        }
    }

    /// The OCR-to-clipboard chord row's control. This action uses the native
    /// channel for the same reason as the methods above.
    fn ocr_clipboard_control(&self) -> HotkeyControl {
        HotkeyChannel::Native.control(
            self.compositor,
            self.linux.ocr_clipboard_key_linux.as_deref().unwrap_or_default(),
            &self.exe,
            snippets::Bind::Press(Verb::OcrClipboard),
        )
    }

    /// The OCR-to-clipboard row's copyable bind.
    /// `None` means that no chord exists or that this compositor has no
    /// clipboard protocol. A bind that only logs a refusal is invalid, so
    /// this window never gives the user that bind.
    fn ocr_clipboard_bind_snippet(&self) -> Option<String> {
        // `None` means no rung and no bind. The `?` enforces that guard.
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
        // Apply removes a field-map row without an Anki field.
        // Anki has no field named "", so core would search for that name on
        // every add and store nothing.
        // Add creates this row because only the user's note type knows its fields.
        // The empty row remains while the user types.
        // Apply removes it before it reaches the file, not in core or after each
        // keystroke. A half-typed name is normal text-box state and needs no early
        // cleanup.
        if let Some(rows) = self.form.field_map.as_mut() {
            rows.retain(|mapping| !mapping.anki_field.trim().is_empty());
        }
        match apply::apply(
            &self.form,
            &self.linux,
            &self.config_path,
            &self.socket_path,
            &self.db_path,
            &self.dicts,
        ) {
            Ok(applied) => {
                // The file now holds clamped values. The window shows them.
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

    /// A rebuild writes the library, so library edits could race.
    fn busy(&self) -> bool {
        self.rebuild_progress.is_some()
    }

    /// Stage the typed path for import. Expand `~` first because the entry
    /// accepts shell-style text. The Browse button gives absolute paths and
    /// skips expansion.
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
            // stage_add refuses an unreadable archive or a source it already
            // holds. Report only the file name.
            None => {
                self.status = format!(
                    "{} is not a readable dictionary archive, or is already staged.",
                    source.display()
                );
            }
        }
    }

    /// Open the desktop file dialog.
    ///
    /// The portal call waits for a person, so an iced executor cannot block on
    /// it. A separate thread makes the portal call. The window receives the
    /// answer through a one-shot channel. The rebuild uses the same channel
    /// shape for progress lines.
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
        // A panic in the thread drops the sender. A cancelled dialog still
        // returns `Ok`. The button must return in both cases, so a channel
        // error also gives an answer.
        Task::perform(rx, |sent| {
            Message::DictPicked(sent.unwrap_or_else(|_| {
                Err("The file dialog stopped without answering.".to_string())
            }))
        })
    }

    /// Stage each path from the dialog in the order shown.
    ///
    /// This method reports one status line instead of one line per file. The
    /// button can select twelve archives at once. Twelve lines would show only
    /// the last file.
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
        // A rebuild can claim the library while the dialog stays open.
        // `busy` rejects a form change while the builder reads the form.
        if self.busy() {
            self.status = "A rebuild is running; add those archives once it finishes.".to_string();
            return;
        }
        let mut staged = 0usize;
        let mut refused = Vec::new();
        for source in &sources {
            match self.form.stage_add(source) {
                Some(_) => staged += 1,
                // stage_add refuses an unreadable archive or a source it
                // already holds. Report only the file name. This loop
                // collects those names.
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

    /// Stage the selected row for removal from every list that contains it.
    ///
    /// One archive is one Dictionary. A row selected in any section names the
    /// whole Dictionary (`stage_remove`). This includes an unreadable archive.
    /// The Terms section lists such an archive because it has no role to enable.
    fn remove_dictionary(&mut self) {
        let Some(Selected { name, .. }) = self.selected.take() else {
            self.status = "Select a dictionary to remove first.".to_string();
            return;
        };
        self.form.stage_remove(&name);
        self.status = format!("{name} is staged for removal; press Rebuild to apply it.");
    }

    /// Claim the library lock and start the rebuild.
    fn start_rebuild(&mut self) -> Task<Message> {
        let plan = rebuild::Plan {
            library_dir: self.library_dir.clone(),
            out: self.db_path.clone(),
            runtime_dir: self.runtime_dir.clone(),
            socket: self.socket_path.clone(),
        };
        // The builder's output rate bounds this channel. The window drains it
        // every frame, so the unbounded channel does not grow.
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

    /// `took_progress` handles one message from the rebuild thread.
    fn took_progress(&mut self, progress: rebuild::Progress) {
        match &progress {
            // Show only lines that the shared renderer can explain. The user
            // did not ask for the other builder output.
            rebuild::Progress::Line(line) => {
                if let Some(text) = chibipop::dict::progress::friendly(line) {
                    self.rebuild_progress = Some(text);
                }
                return;
            }
            rebuild::Progress::Done { .. } => {
                // The library now matches the form.
                self.form.clear_staged();
                self.selected = None;
            }
            // The rebuild returned the archives. The form still describes the
            // user's request, so staged edits remain staged.
            rebuild::Progress::Failed(_) => {}
        }
        self.rebuild_progress = None;
        self.status = rebuild::describe(&progress);
    }
}

/// Return the picked archive's file name, not its full path. One status line
/// cannot show many absolute paths, and the user already saw the directory.
fn name_of(source: &std::path::Path) -> String {
    source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| source.display().to_string())
}

/// The `s` in "2 archives". Empty for one archive.
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
    EdgeAutoscroll(bool),
    SidePanel(bool),
    LayerPicked(String),
    /// The layout-mode picker label. [`LAYOUT_MODES`] maps the label back,
    /// so the call site does not compare strings or indexes.
    LayoutModePicked(String),
    /// Whether `popup.dictionary_styling` sends Dictionary `style` declarations
    /// and `styles.css` to the panel.
    DictStyling(bool),
    ShowExamples(bool),
    ShowAttributions(bool),
    ShowImages(bool),
    ShowPartOfSpeech(bool),
    /// A row press in one of the three lists. The role identifies the list that
    /// received it. A mixed archive can appear in two lists, so the name alone
    /// cannot identify the list.
    ///
    /// A press also starts a hold. The deadband decides whether the hold becomes
    /// a drag or a click ([`press_row`]).
    DictSelected(Role, String),
    /// A cursor move over a row. The fields identify the list, row, and local point.
    /// [`mouse_area`] reports a point local to its wrapped one-row widget.
    /// [`list_y`] adds the row index to recover the list position.
    DictHover(Role, usize, Point),
    /// A pointer release over the three lists. The held row moves to the insertion
    /// line unless the press stayed inside the deadband.
    DictDropped,
    /// A left-button release outside the list area. The window cancels the drag
    /// because no list received a position ([`subscription`]).
    DictReleased,
    /// Move the selected row up in this section. The role belongs to the button,
    /// not the selection, so the message reorders only this list.
    DictUp(Role),
    /// Move the selected row down in this section.
    DictDown(Role),
    /// A row's enabled state for one role. Each role has its own state
    /// (ARCHITECTURE.md#dictionary-and-lookup), so this message changes only
    /// the list with the checkbox.
    DictEnabled(Role, String, bool),
    /// The ranking strategy label above the Frequency list. [`RANKING_STRATEGIES`]
    /// maps the label back, so the call site does not compare strings or indexes.
    RankingPicked(String),
    AddPath(String),
    DictAdd,
    DictRemove,
    /// `super::filechooser` opens the desktop file dialog.
    DictBrowse,
    /// The answer from the dialog thread.
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
    /// The selection button-mode picker label. [`SELECTION_BUTTONS`] maps it back.
    SelectionButtonsPicked(String),
    /// The selection separator picker label. [`SELECTION_SEPARATORS`] maps it back.
    SelectionSeparatorPicked(String),
    /// Whether an add carries a mining picture. The core gate is
    /// `chibipop::shot::plan_add`.
    IncludeScreenshot(bool),
    /// The mining screenshot chord. Empty text becomes `None` in the config field,
    /// and this arm stores that value.
    ScreenshotKey(String),
    ScreenshotSaveDir(String),
    /// The OCR-to-clipboard chord. Empty text becomes `None` in the config field,
    /// and this arm stores that value.
    OcrClipboardKey(String),
    /// The sentence-capture picker label. [`SENTENCE_MODES`] maps it back, so the
    /// call site does not compare strings or indexes.
    SentenceModePicked(String),
    ShowStaticOverlay(bool),
    StaticRegionKey(String),
    /// An Anki field name from a field-map row. The field is free text because
    /// only the user's note type knows its fields. This window does not query
    /// Anki for them (see [`field_map_rows`]).
    FieldMapAnki(usize, String),
    /// A field-map row's source label. [`field_source_of`] maps it through
    /// [`FIELD_SOURCES`]. The vocabulary is closed.
    /// `anki::mapped_fields` drops an unknown source without a warning.
    FieldMapSource(usize, String),
    /// Append a field-map row. The new row starts with [`NEW_ROW_SOURCE`].
    /// Before this message, Linux users could use only the shipped `field_map`.
    /// That map has no row for `screenshot`.
    FieldMapAdd,
    /// Remove the field-map row at this index.
    FieldMapRemove(usize),
    /// Copy the trigger chord's press and release bind.
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
        Message::EdgeAutoscroll(on) => app.form.edge_autoscroll = on,
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
        Message::DictSelected(role, name) => press_row(app, role, name),
        Message::DictHover(role, at, point) => {
            app.hover = Some(Hover { role, y: list_y(at, point.y) });
        }
        Message::DictDropped => drop_held(app),
        // The pointer releases its row here. A drop that commits the row
        // already passed through [`subscription`].
        Message::DictReleased => app.drag = Drag::Idle,
        Message::DictUp(role) => move_selected(app, role, -1),
        Message::DictDown(role) => move_selected(app, role, 1),
        Message::DictEnabled(role, name, on) => set_enabled(app, role, &name, on),
        // `settings::dictionary_work` reads strategy, order, and Frequency enabled
        // state from the saved config. This arm stores the picked strategy. Apply
        // compares the current file with the next file and runs a reindex when one
        // value changes (`super::apply`).
        Message::RankingPicked(label) => app.form.ranking_strategy = ranking_strategy_of(&label),
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
        Message::SelectionButtonsPicked(label) => {
            app.form.selection_buttons = selection_buttons_of(&label);
        }
        Message::SelectionSeparatorPicked(label) => {
            app.form.selection_separator = selection_separator_of(&label);
        }
        Message::IncludeScreenshot(on) => app.form.include_screenshot = on,
        // This is the only place where empty text becomes `None`. The config field
        // uses `Option`, so absence has a distinct value. An empty chord would be a
        // sentinel that the daemon would need to interpret.
        Message::ScreenshotKey(v) => {
            app.linux.screenshot_key_linux = (!v.trim().is_empty()).then_some(v);
        }
        Message::ScreenshotSaveDir(v) => app.linux.screenshot_save_dir = v,
        // The OCR-to-clipboard key uses the same empty-text rule. The Windows
        // counterpart also rejects this sentinel.
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
        // The picker returns only [`FIELD_SOURCES`] entries, so the UI cannot produce
        // an invalid source. This arm still checks the value because it is the only
        // path from this message to the form. `anki.rs`'s `mapped_fields` silently
        // drops an unknown source, which would leave a row that looks configured but
        // does nothing.
        Message::FieldMapSource(i, v) => {
            let row = app.form.field_map.as_mut().and_then(|rows| rows.get_mut(i));
            if let (Some(source), Some(m)) = (field_source_of(&v), row) {
                m.source = source.to_string();
            }
        }
        // The Anki field starts empty because only the user's note type knows its
        // field names. `App::apply` removes the row if the field stays empty. The
        // field-map list can be `None`, so this arm creates the list and records the row.
        Message::FieldMapAdd => {
            app.form.field_map.get_or_insert_with(Vec::new).push(FieldMapping {
                anki_field: String::new(),
                source: NEW_ROW_SOURCE.to_string(),
            });
        }
        // The index comes from the list that the last frame rendered, so it can be
        // stale and make `Vec::remove` panic. This arm checks the bound, so
        // message order is not enough.
        Message::FieldMapRemove(i) => {
            if let Some(rows) = app.form.field_map.as_mut() {
                if i < rows.len() {
                    rows.remove(i);
                }
            }
        }
        Message::CopyBind => return iced::clipboard::write(app.bind_snippet()),
        // The button exists only when a snippet exists, so `None` cannot come from
        // the UI. This arm remains a no-op for other callers and does not copy an
        // old bind.
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
            // The handler writes or removes the file, then reads it again. The file
            // is the state, so the widget shows filesystem state instead of the
            // click result.
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
            // ureq blocks, and this code runs on the UI thread. The code uses one
            // click, one thread, and one status line. This matches the rebuild row
            // but sends one message instead of a stream.
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

/// Move the row from `from` to `to`. Each row between them shifts one place.
///
/// This is the only function that changes list order. A move button supplies
/// an adjacent index, and a drop supplies the pointer position. Both paths
/// call this function, so both use the same order rule.
fn move_row(app: &mut App, role: Role, from: usize, to: usize) {
    let rows = app.form.list_mut(role);
    if from == to || from >= rows.len() || to >= rows.len() {
        return;
    }
    let row = rows.remove(from);
    rows.insert(to, row);
}

/// Move the selected Dictionary one place in this role's list.
///
/// `role` comes from the pressed button, not the selection. A Frequency button
/// therefore does nothing when the user selects a Terms row. Each section has its own
/// order, and a row cannot enter a list without its role
/// (ARCHITECTURE.md#dictionary-and-lookup).
///
/// A move changes one place. Other rows keep their positions. A list end has no
/// destination, so the move does not wrap. This is the keyboard path, and the
/// drag path uses `move_row`.
fn move_selected(app: &mut App, role: Role, delta: i32) {
    let Some(selected) = &app.selected else { return };
    if selected.role != role {
        return;
    }
    let rows = app.form.list(role);
    let Some(at) = rows.iter().position(|row| row.name == selected.name) else { return };
    let to = if delta < 0 {
        at.checked_sub(1)
    } else {
        (at + 1 < rows.len()).then_some(at + 1)
    };
    if let Some(to) = to {
        move_row(app, role, at, to);
    }
}

/// Return the list position for row `index` and local offset `at`.
///
/// [`mouse_area`] reports a point for one row widget. The row index and local
/// offset restore the cursor position in the full list.
fn list_y(index: usize, at: f32) -> f32 {
    index as f32 * ROW_PITCH + at
}

/// Return the drop boundary for a cursor `y` in a list of `len` rows.
/// The top half of a row inserts before it. The bottom half inserts after it.
/// Values above or below the list clamp to its ends.
///
/// This function has no iced code. Tests can check this arithmetic without a
/// window, renderer, or font.
fn drop_index(y: f32, len: usize) -> usize {
    (y / ROW_PITCH).round().clamp(0.0, len as f32) as usize
}

/// Whether the cursor moved far enough from the grab point to make the press
/// a drag instead of a click.
///
/// This check uses only vertical distance because the list order is vertical.
/// Horizontal distance does not affect the row's destination.
fn past_deadband(origin: f32, cursor: f32) -> bool {
    (cursor - origin).abs() > DRAG_DEADBAND
}

/// Whether a held press has crossed the deadband.
///
/// A cursor outside the source list counts as past the deadband. Lists have
/// different heights, so they cannot share a second position measure.
fn left_the_deadband(held: Role, origin: f32, hover: Hover) -> bool {
    hover.role != held || past_deadband(origin, hover.y)
}

/// Return the drop boundary for a row held from `held` when the cursor is at
/// `hover`, in a list of `len` rows.
///
/// A cursor in another role's list maps to the end that it crossed. Sections
/// follow [`Role::EVERY`] order. A later section means the bottom, and an earlier
/// section means the top. The held row cannot enter another list.
fn drop_at(held: Role, hover: Hover, len: usize) -> usize {
    match hover.role.cmp(&held) {
        Ordering::Less => 0,
        Ordering::Greater => len,
        Ordering::Equal => drop_index(hover.y, len),
    }
}

/// Return the press position for `name` in the `role` list.
///
/// [`mouse_area`]'s `on_press` has no position. A cursor must first report a
/// move before it can press a row, so `hover` normally supplies the position.
/// If no hover exists, use the row center to keep the deadband inside the row.
fn grab_origin(app: &App, role: Role, name: &str) -> Option<f32> {
    if let Some(hover) = app.hover.filter(|hover| hover.role == role) {
        return Some(hover.y);
    }
    let at = app.form.list(role).iter().position(|row| row.name == name)?;
    Some(list_y(at, ROW_HEIGHT / 2.0))
}

/// Select a row and hold it for a possible drag.
///
/// The deadband decides whether the hold becomes a drag. Before the cursor
/// crosses it, no drop line appears and release changes no order. A click only
/// changes the selection.
fn press_row(app: &mut App, role: Role, name: String) {
    app.drag = match grab_origin(app, role, &name) {
        Some(origin) => Drag::Dragging { role, row: name.clone(), origin },
        // The name is not in any list that this window drew. Keep the selection, but
        // do not hold a row that does not exist.
        None => Drag::Idle,
    };
    app.selected = Some(Selected { role, name });
}

/// Release the held row.
///
/// A press inside the deadband is a click and changes no order. This rule
/// leaves the list unchanged after a small pointer move.
fn drop_held(app: &mut App) {
    let Drag::Dragging { role, row, origin } = std::mem::replace(&mut app.drag, Drag::Idle) else {
        return;
    };
    let Some(hover) = app.hover.filter(|hover| left_the_deadband(role, origin, *hover)) else {
        return;
    };
    let rows = app.form.list(role);
    let Some(from) = rows.iter().position(|dict| dict.name == row) else { return };
    // The drop boundary counts rows above it. After the code removes the row,
    // every lower boundary shifts up by one.
    let at = drop_at(role, hover, rows.len());
    move_row(app, role, from, if at > from { at - 1 } else { at });
}

/// Return the drop boundary for `role`, or `None` when no drag holds a row
/// from that list.
///
/// The drop line is the only drag feedback. A row that follows the pointer would need
/// `with_translation` and `with_layer`, but iced 0.14 clips it inside
/// `scrollable`. The row could disappear at a section edge. A line inside the
/// list avoids that clip.
fn drop_line(app: &App, role: Role) -> Option<usize> {
    let Drag::Dragging { role: held, origin, .. } = &app.drag else { return None };
    if *held != role {
        return None;
    }
    let hover = app.hover.filter(|hover| left_the_deadband(role, *origin, *hover))?;
    Some(drop_at(role, hover, app.form.list(role).len()))
}

/// Return the vertical position of the drop line before row `index` in `len`.
///
/// The gap between rows equals the line thickness. The line fills the gap.
/// It does not move a row. At either end, no outside gap exists. The line stays
/// inside the list because the stacked layer has no outside space.
fn line_top(index: usize, len: usize) -> f32 {
    let height = (len as f32 * ROW_PITCH - ROW_SPACING).max(0.0);
    (index as f32 * ROW_PITCH - ROW_SPACING).clamp(0.0, (height - ROW_SPACING).max(0.0))
}

/// Set one role's enabled state.
///
/// The checkbox changes only its own list. A mixed archive can keep frequency
/// data when the user disables its Terms row, because each role has separate
/// state (ARCHITECTURE.md#dictionary-and-lookup). The row keeps its position
/// because order and enabled state are separate.
///
/// This code uses the row name instead of its index. A rebuild or removal can
/// change the list before the next press.
fn set_enabled(app: &mut App, role: Role, name: &str, on: bool) {
    if let Some(row) = app.form.list_mut(role).iter_mut().find(|row| row.name == name) {
        row.enabled = on;
    }
}

fn view(app: &App) -> Element<'_, Message> {
    let content = column![
        // `設定` tests the Japanese fallback. The first window line contains kanji,
        // and cosmic-text renders it.
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
        // The legacy `hold-shift` alias means HoldKey, as in the Windows radio controls.
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
        // This is the portal rung. The window cannot offer an in-app rebind because
        // the portal owns the bind. The portal dialog and the desktop shortcut editor
        // change the key. The chord above is the value this window gives the portal at
        // the next start.
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
            // Kanji and kana use the selected family. This preview confirms that the family
            // renders Japanese.
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
            checkbox(app.form.edge_autoscroll)
                .label("Auto-scroll while dragging at the popup edge")
                .on_toggle(Message::EdgeAutoscroll),
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

/// The layout-mode picker items, in display order.
///
/// One ordered table supplies both directions of the UI map, like
/// [`SENTENCE_MODES`]. The table provides labels to the picker and receives the
/// selected mode without a second map.
const LAYOUT_MODES: [(LayoutMode, &str); 2] = [
    (LayoutMode::Roomy, "Roomy (one item per line)"),
    (LayoutMode::Compact, "Compact (one line per dictionary)"),
];

/// The button-mode picker items, in display order.
const SELECTION_BUTTONS: [(SelectionButtons, &str); 2] = [
    (SelectionButtons::PrimaryAdditive, "Primary additive"),
    (SelectionButtons::PrimaryReplacing, "Primary replacing"),
];

fn selection_button_labels() -> Vec<String> {
    SELECTION_BUTTONS.iter().map(|&(_, label)| label.to_string()).collect()
}

fn selection_button_label(buttons: SelectionButtons) -> &'static str {
    SELECTION_BUTTONS
        .iter()
        .find(|&&(value, _)| value == buttons)
        .map_or(SELECTION_BUTTONS[0].1, |&(_, label)| label)
}

fn selection_buttons_of(label: &str) -> SelectionButtons {
    SELECTION_BUTTONS
        .iter()
        .find(|&&(_, value)| value == label)
        .map_or(SelectionButtons::PrimaryAdditive, |&(buttons, _)| buttons)
}

/// The separator picker items, in display order.
const SELECTION_SEPARATORS: [(SelectionSeparator, &str); 4] = [
    (SelectionSeparator::Ellipsis, "Ellipsis (…)"),
    (SelectionSeparator::Space, "Space"),
    (SelectionSeparator::LineBreak, "Line break"),
    (SelectionSeparator::ListItems, "List items"),
];

fn selection_separator_labels() -> Vec<String> {
    SELECTION_SEPARATORS.iter().map(|&(_, label)| label.to_string()).collect()
}

fn selection_separator_label(separator: SelectionSeparator) -> &'static str {
    SELECTION_SEPARATORS
        .iter()
        .find(|&&(value, _)| value == separator)
        .map_or(SELECTION_SEPARATORS[0].1, |&(_, label)| label)
}

fn selection_separator_of(label: &str) -> SelectionSeparator {
    SELECTION_SEPARATORS
        .iter()
        .find(|&&(_, value)| value == label)
        .map_or(SelectionSeparator::Ellipsis, |&(separator, _)| separator)
}

/// The picker's items, in table order.
fn layout_labels() -> Vec<String> {
    LAYOUT_MODES.iter().map(|&(_, label)| label.to_string()).collect()
}

/// The label for a layout mode. Every `LayoutMode` appears in the table, so the
/// fallback cannot occur.
fn layout_mode_label(mode: LayoutMode) -> &'static str {
    LAYOUT_MODES.iter().find(|&&(m, _)| m == mode).map_or(LAYOUT_MODES[0].1, |&(_, l)| l)
}

/// The layout mode for a picked label. Only labels from this table return from
/// the UI, so the fallback cannot occur there.
fn layout_mode_of(label: &str) -> LayoutMode {
    LAYOUT_MODES.iter().find(|&&(_, l)| l == label).map_or(LayoutMode::Roomy, |&(m, _)| m)
}

/// The controls that decide entry content.
///
/// This group sits apart from Popup because these fields decide entry content,
/// while the rows above decide panel size. The Windows window uses the same
/// group.
///
/// Every field is a portable field. Each platform must preserve all of them,
/// so one config file has the same meaning on Windows and Linux.
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

/// The ranking strategy picker items, in display order.
///
/// One ordered table supplies both directions of the map, like
/// [`SENTENCE_MODES`] and [`LAYOUT_MODES`]. The Windows window uses the same
/// labels, so both settings windows show one value.
const RANKING_STRATEGIES: [(RankingStrategy, &str); 3] = [
    (RankingStrategy::BestRank, "Best rank (rank by highest frequency out of all freq dicts)"),
    (RankingStrategy::Priority, "Priority (rank using highest prioritized freq dict available)"),
    (RankingStrategy::Median, "Median (rank by median freq)"),
];

/// The picker's items, in table order.
fn ranking_labels() -> Vec<String> {
    RANKING_STRATEGIES.iter().map(|&(_, label)| label.to_string()).collect()
}

/// The label for a `RankingStrategy`. Every strategy appears in the table, so
/// the fallback cannot occur.
fn ranking_strategy_label(strategy: RankingStrategy) -> &'static str {
    RANKING_STRATEGIES
        .iter()
        .find(|&&(s, _)| s == strategy)
        .map_or(RANKING_STRATEGIES[0].1, |&(_, l)| l)
}

/// The strategy for a picked label. Only labels from this table return from the
/// UI, so the fallback cannot occur there.
fn ranking_strategy_of(label: &str) -> RankingStrategy {
    RANKING_STRATEGIES
        .iter()
        .find(|&&(_, l)| l == label)
        .map_or(RankingStrategy::BestRank, |&(s, _)| s)
}

/// The caption for one role's list.
///
/// Each section has its own checkbox purpose, so the UI uses three captions
/// instead of one "Dictionaries" list. This function states that difference
/// (ARCHITECTURE.md#dictionary-and-lookup).
fn role_caption(role: Role) -> &'static str {
    match role {
        Role::Terms => "Term Dictionaries (priority order)",
        Role::Frequency => "Frequency Dictionaries",
        Role::Pitch => "Pitch Dictionaries",
    }
}

/// The style for a selected row.
///
/// This row uses a styled container instead of a button. `iced_widget::button`
/// captures the press before [`mouse_area`] can see it. A button row could
/// receive clicks but could not start a drag.
fn picked_row(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(palette.primary.weak.color.into()),
        text_color: Some(palette.primary.weak.text),
        border: iced::border::rounded(2),
        ..container::Style::default()
    }
}

/// The style for the drop line.
///
/// The line uses the accent color instead of the default divider gray. It shows
/// the pointer's drop position, not a separator, so it must stand out in a
/// long Dictionary list.
fn insertion_line(theme: &Theme) -> rule::Style {
    rule::Style { color: theme.extended_palette().primary.base.color, ..rule::default(theme) }
}

/// Render the rows for one role. Each row has an enabled checkbox and a name
/// that serves as the selection and drag target.
///
/// The list preserves row order and keeps a name that no installed Dictionary
/// answers. It keeps that name in the file, so an absent archive cannot
/// remove it.
///
/// Each row uses [`mouse_area`] because iced 0.14 has no reorderable list
/// widget. `mouse_area` reports the press and pointer moves. [`update`] turns
/// those messages into a drag. The checkbox handles its own press, so it
/// toggles the row and does not start a drag. `line` gives the drop boundary.
/// The window draws it as a stacked rule, so it uses no layout space and does
/// not move the rows.
fn dict_rows<'a>(
    role: Role,
    rows: &'a [DictRow],
    selected: Option<&'a str>,
    line: Option<usize>,
) -> Element<'a, Message> {
    if rows.is_empty() {
        return text("(none)").size(14).into();
    }
    let list = column(rows.iter().enumerate().map(|(index, dict)| {
        let picked = Some(dict.name.as_str()) == selected;
        let name = dict.name.clone();
        let label = container(text(dict.name.clone()).size(14))
            .padding([0, 6])
            .width(Length::Fill)
            .height(Length::Fill)
            .align_y(iced::Center)
            .style(if picked { picked_row } else { container::transparent });
        mouse_area(
            row![
                checkbox(dict.enabled)
                    .on_toggle(move |on| Message::DictEnabled(role, name.clone(), on)),
                label,
            ]
            .spacing(8)
            .height(ROW_HEIGHT)
            .align_y(iced::Center),
        )
        .interaction(iced::mouse::Interaction::Grab)
        .on_press(Message::DictSelected(role, dict.name.clone()))
        .on_move(move |at| Message::DictHover(role, index, at))
        .into()
    }))
    .spacing(ROW_SPACING);
    let Some(index) = line else { return list.into() };
    stack![
        list,
        column![
            space().height(line_top(index, rows.len())),
            rule::horizontal(ROW_SPACING).style(insertion_line),
        ]
        .width(Length::Fill),
    ]
    .into()
}

/// The rule that reduces enabled frequency lists to one rank.
///
/// This control sits above the list because the strategy applies to the whole
/// section. A per-row picker would imply a per-Dictionary value.
fn ranking_row(app: &App) -> Element<'_, Message> {
    row![
        text("Ranking").size(14),
        pick_list(
            ranking_labels(),
            Some(ranking_strategy_label(app.form.ranking_strategy).to_string()),
            Message::RankingPicked,
        ),
    ]
    .spacing(10)
    .align_y(iced::Center)
    .into()
}

/// Render one role section with its caption, list, and move buttons.
///
/// The buttons carry the role, so each press reorders only this list. A row
/// cannot move to a section without that role. The buttons provide keyboard
/// moves and remain when the section gains more controls.
///
/// `above` contains the control between the caption and list. Frequency passes
/// the ranking-strategy picker. Other roles pass nothing. This helper does not
/// depend on the role that owns the picker.
fn role_section<'a>(
    app: &'a App,
    role: Role,
    above: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let selected = app.selected.as_ref().filter(|s| s.role == role).map(|s| s.name.as_str());
    let mut list = column![text(role_caption(role)).size(14)].spacing(6);
    if let Some(above) = above {
        list = list.push(above);
    }
    list = list.push(dict_rows(role, app.form.list(role), selected, drop_line(app, role)));
    row![
        list.width(Length::Fill),
        column![
            button("Move up").on_press(Message::DictUp(role)).width(Length::Fill),
            button("Move down").on_press(Message::DictDown(role)).width(Length::Fill),
        ]
        .spacing(6)
        .width(140),
    ]
    .spacing(16)
    .into()
}

fn dictionaries_section(app: &App) -> Element<'_, Message> {
    // Browse appears first because it opens the picker. The adjacent entry supports
    // desktops without a portal. All three controls stay disabled during a rebuild,
    // and Browse also stays disabled while its dialog is open.
    //
    // Remove stays on this row instead of beside move buttons. Add and Remove are
    // the two library operations. Remove deletes a Dictionary from every section,
    // so it belongs to none of them.
    let library = row![
        button("Browse…")
            .on_press_maybe((!app.busy() && !app.picking).then_some(Message::DictBrowse)),
        text_input("or type a path to a Yomitan .zip", &app.add_path)
            .on_input_maybe((!app.busy()).then_some(Message::AddPath))
            .width(Length::Fill),
        button("Add").on_press_maybe((!app.busy()).then_some(Message::DictAdd)),
        button("Remove").on_press_maybe((!app.busy()).then_some(Message::DictRemove)),
    ]
    .spacing(8)
    .align_y(iced::Center);

    // The three sections share one release area. A drag past a list end leaves the
    // pointer outside every row, but the row must still land at that end. A release
    // elsewhere or outside the window reaches [`subscription`] and cancels.
    let lists = mouse_area(
        column![
            role_section(app, Role::Terms, None),
            role_section(app, Role::Frequency, Some(ranking_row(app))),
            role_section(app, Role::Pitch, None),
        ]
        .spacing(12),
    )
    .on_release(Message::DictDropped);

    let body = column![
        lists,
        library,
        text(
            "Names match exactly and position is priority inside its own section. A \
             checkbox turns a dictionary on for the section it sits in and leaves the \
             others alone. Adds and removals are staged: Rebuild imports them and \
             rebuilds the database."
        )
        .size(13),
        rebuild_row(app),
    ]
    .spacing(12);
    section("Dictionaries", body)
}

/// The Rebuild button and the latest progress line.
///
/// Only one rebuild can run at a time. `rebuild_progress` disables the button
/// and supplies its status line.
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
    // Hide `ocr.language` on Linux
    // (ARCHITECTURE.md#settings-and-config). meikiocr supports Japanese only, so
    // whole-struct saves preserve the stored value.
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
            // OCR-to-clipboard stays here because it uses the same engine and settings
            // as the controls above. It differs only in its text destination.
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

/// The OCR-to-clipboard bind, or the reason that no bind exists.
///
/// No bind can mean that the user typed no chord or that the compositor has no
/// clipboard protocol. Check the protocol first. A pasted bind that can only
/// log a refusal cannot work, so this window does not provide it.
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
        // This arm cannot occur because `ocr_clipboard_control` always uses
        // `HotkeyChannel::Native`. Keep the fallback honest. Do not use an unwrap.
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

/// The sentence-capture picker items, in display order.
///
/// One ordered table supplies both directions of the UI map. The labels go
/// to the picker and the mode comes back. iced returns a label, while the
/// Windows combo returns an index. Both windows use one table instead of a
/// string match at the call site.
const SENTENCE_MODES: [(SentenceMode, &str); 3] = [
    (SentenceMode::Line, "Current line"),
    (SentenceMode::All, "All lines"),
    (SentenceMode::Static, "Static region"),
];

/// The picker items in table order.
fn sentence_labels() -> Vec<String> {
    SENTENCE_MODES.iter().map(|&(_, label)| label.to_string()).collect()
}

/// The label for a `SentenceMode`. Every mode appears in the table, so the
/// fallback cannot occur. The first item remains the default if a future mode
/// lacks a table entry.
fn sentence_mode_label(mode: SentenceMode) -> &'static str {
    SENTENCE_MODES.iter().find(|&&(m, _)| m == mode).map_or(SENTENCE_MODES[0].1, |&(_, l)| l)
}

/// The sentence mode for a picked label. Only labels from this table return
/// from the UI, so the fallback cannot occur there.
fn sentence_mode_of(label: &str) -> SentenceMode {
    SENTENCE_MODES.iter().find(|&&(_, l)| l == label).map_or(SentenceMode::Line, |&(m, _)| m)
}

/// The static-region row's copyable bind, or the reason that no bind exists.
///
/// The caption always says "native channel" because this action has no portal
/// id. A compositor bind is the only path. The row must not suggest a portal
/// consent dialog for this action.
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
        // This arm cannot occur because `static_region_control` always uses
        // `HotkeyChannel::Native`. Keep the fallback honest. Do not use an unwrap.
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

/// The sentence-capture rows. They choose the Anki sentence field and, in
/// Static mode, show the static region controls.
///
/// Windows hides region rows outside Static, and Linux does the same. The chord
/// row remains visible in every mode because the user must set it before the mode
/// changes to Static.
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

/// The mining screenshot row's copyable bind, or the reason that no bind exists.
///
/// This action has no portal id, so the compositor bind is its only path. The
/// row uses "Native channel" text for the same reason as `static_region_bind`.
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
        // This arm cannot occur because `screenshot_control` always uses
        // `HotkeyChannel::Native`. Keep the fallback honest. Do not use an unwrap.
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

/// The mining screenshot rows. They control inclusion on add, the save folder,
/// and the standalone screenshot chord.
///
/// Show every row in every state. The folder and chord also affect the
/// standalone screenshot action, so `include_on_add` does not control them.
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

/// Return the source for a picked label, or `None` when core does not know it.
///
/// [`FIELD_SOURCES`] supplies both picker items and returned sources. `None`
/// cannot come from the picker. It represents a source written directly in
/// TOML, so the row stays unset. Core ignores an unknown map.
fn field_source_of(picked: &str) -> Option<&'static str> {
    FIELD_SOURCES.iter().copied().find(|&source| source == picked)
}

/// The source for a new field-map row.
///
/// Use `screenshot` instead of the first vocabulary item. `default_field_map`
/// has no row for `glossary_html`, `sentence`, or `screenshot`. Only the absent
/// `screenshot` row can make `actions.screenshot.include_on_add` inert.
/// `shot::plan_add` reads the picture field name from this row. Add therefore
/// chooses this source, which saves one picker change.
const NEW_ROW_SOURCE: &str = "screenshot";

/// Render the field-map group. Each row has an Anki field and a source picker.
///
/// Windows gets row names from a live AnkiConnect `modelFieldNames` call
/// (`ui/settings_window.rs`). Linux keeps the rows from config instead. A live
/// query could remove a saved row when the user closes Anki or selects another
/// note type. Add and Remove keep config rows stable. The user types the Anki
/// field name, and the picker uses core's closed source vocabulary.
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
    // The add-card row uses the same hotkey control as the trigger row. On the
    // native rung, the compositor bind is the only path to the add
    // (ARCHITECTURE.md#input-ladders, rung 2 plus its 2026-08-26 addendum).
    // Without a copyable bind, a typed chord could never reach the add.
    let add_bind: Element<'_, Message> = match app.add_control() {
        HotkeyControl::Snippet { text: snippet } => column![
            text("Native channel: your compositor owns this binding. Paste this into its config:"),
            container(text(snippet).font(Font::MONOSPACE).size(13)).padding(8),
            button("Copy add-card bind").on_press(Message::CopyAddBind),
        ]
        .spacing(6)
        .into(),
        // The portal published the key for the *add-card* id. Use the trigger row's
        // status vocabulary because both values describe portal ownership. `current:
        // None` means that the desktop reported no key or did not answer for this id.
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
        // Windows labels this field "First dictionary only" (`ui/settings_window.rs`).
        // The daemon reads `anki.first_dict_only`, so this row lets the user change it
        // without a TOML edit.
        checkbox(app.form.first_dict_only)
            .label("First dictionary only")
            .on_toggle(Message::FirstDictOnly),
        labeled(
            "Selection buttons",
            pick_list(
                selection_button_labels(),
                Some(selection_button_label(app.form.selection_buttons).to_string()),
                Message::SelectionButtonsPicked,
            ),
        ),
        labeled(
            "Selection separator",
            pick_list(
                selection_separator_labels(),
                Some(selection_separator_label(app.form.selection_separator).to_string()),
                Message::SelectionSeparatorPicked,
            ),
        ),
        column(screenshot_rows(app)).spacing(10),
        column(sentence_rows(app)).spacing(10),
        text("Field mappings").size(14),
        column(field_map_rows(app)).spacing(10),
    ]
    .spacing(10);
    section("Anki", body)
}

/// The autostart row uses the XDG `.desktop` file as its state
/// (ARCHITECTURE.md#settings-and-config). The checkbox changes the file
/// immediately. No Apply action or TOML field stores this state.
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

/// The Updates row matches the Windows group and the Linux packaging rule
/// (ARCHITECTURE.md#packaging-and-ci). Linux only reports an update. It does
/// not replace the binary, so the row identifies the asset to fetch and the
/// process that owns the binary.
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

/// The installed font families that the combo offers. `fontdb` supplies
/// JP-capable names, and this list sorts and deduplicates them
/// (ARCHITECTURE.md#settings-and-config). [`popup::jp_capable`] provides the
/// classifier, so the combo and popup use the same Japanese test.
///
/// `FAMILIES` is static because iced's [`iced::font::Family::Name`] requires an
/// `&'static str`. The preview must name the selected family, so these names
/// provide that source.
static FAMILIES: LazyLock<Vec<String>> = LazyLock::new(|| {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let mut names: Vec<String> = db.faces().map(|face| face.families[0].0.clone()).collect();
    names.sort();
    names.dedup();
    offered(names)
});

/// The names that [`FAMILIES`] keeps and the fallback when no family draws Japanese.
///
/// If no Japanese face exists, the function returns every family. An empty
/// combo gives the user no usable control. The popup also paints and reports
/// the absent package and does not reject the choice.
/// This function stays pure, so tests can check the filter without a font stack.
fn offered(all: Vec<String>) -> Vec<String> {
    let (jp, rest): (Vec<String>, Vec<String>) =
        all.into_iter().partition(|name| popup::jp_capable(name));
    if jp.is_empty() {
        rest
    } else {
        jp
    }
}

/// The font combo items.
///
/// `Borrowed` names come from [`FAMILIES`], so iced can paint the preview with
/// them. `Owned` stores a configured name that the combo would otherwise omit.
/// The name can lack an installed face or Japanese glyphs. The combo still offers
/// and selects it (ARCHITECTURE.md#settings-and-config). The preview uses iced's
/// default for that name.
fn font_items(configured: &str) -> Vec<Cow<'static, str>> {
    let mut items: Vec<Cow<'static, str>> =
        FAMILIES.iter().map(|f| Cow::Borrowed(f.as_str())).collect();
    if !items.iter().any(|f| f == configured) {
        items.insert(0, Cow::Owned(configured.to_string()));
    }
    items
}

/// The combo item that matches the form font.
///
/// [`font_items`] always includes the font used when the window opens. Later
/// writes to `form.font` come from picked items, so `None` only means that no
/// preview family exists.
fn selected_family(app: &App) -> Option<&Cow<'static, str>> {
    app.fonts.iter().find(|f| f.as_ref() == app.form.font.as_str())
}

/// The preview uses the selected family when the combo offers it. Otherwise it
/// uses iced's default for the configured literal ([`font_items`]).
fn preview_font(selected: Option<&Cow<'static, str>>) -> Font {
    match selected {
        Some(Cow::Borrowed(name)) => Font::with_name(name),
        _ => Font::DEFAULT,
    }
}

/// The dictionary controls receive one [`Message`] at a time through [`update`].
/// No window opens here. `view` builds widgets from the same state, so the
/// tests check the state after each press.
#[cfg(test)]
mod tests {
    use super::*;
    use chibipop::settings::DictionaryWork;
    use crate::paths::Env;
    use std::path::Path;

    /// The field-map rows that the window edits. Linux always has an answer, while
    /// `None` represents the Windows-only state where AnkiConnect did not return
    /// field names (`chibipop::settings::SettingsForm::field_map`).
    fn form_rows(app: &App) -> &[FieldMapping] {
        app.form.field_map.as_deref().expect("a Linux form always has field-map rows")
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/yomitan")
            .join(name)
    }

    /// A list with every row enabled. This matches an untouched list.
    fn rows(names: &[&str]) -> Vec<DictRow> {
        names
            .iter()
            .map(|name| DictRow { name: (*name).to_string(), enabled: true })
            .collect()
    }

    /// The names that one section renders, in order.
    fn listed(app: &App, role: Role) -> Vec<String> {
        app.form.list(role).iter().map(|row| row.name.clone()).collect()
    }

    /// The names and enabled state that one section renders.
    fn checked(app: &App, role: Role) -> Vec<(String, bool)> {
        app.form.list(role).iter().map(|row| (row.name.clone(), row.enabled)).collect()
    }

    /// The config that the form will save. Reindex and lookup read the file, not
    /// the form, so this captures a press's actual effect.
    fn saved(app: &App) -> chibipop::config::Config {
        chibipop::settings::apply_to(&app.form, &chibipop::config::Config::default())
    }

    /// The work that Apply needs beyond the file write.
    ///
    /// Core defines this rule in `settings::dictionary_work`. The helper compares
    /// the config before and after the press, as `apply.rs` does.
    fn work(opened: &chibipop::config::Config, app: &App) -> DictionaryWork {
        chibipop::settings::dictionary_work(opened, &saved(app))
    }

    /// Set the hover position for row `index` in `role` at `at` pixels.
    /// This matches the point that the row's [`mouse_area`] reports.
    /// The x coordinate has no effect because the list moves rows vertically.
    fn hover(app: &mut App, role: Role, index: usize, at: f32) {
        let _ = update(app, Message::DictHover(role, index, Point::new(4.0, at)));
    }

    /// The window state without fontdb or filesystem access.
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
            dicts: Vec::new(),
            runtime_dir: dir.join("run"),
            autostart: None,
            home: None,
            exe: PathBuf::from("/usr/bin/chibipop"),
            // This session can copy, so tests exercise the OCR-to-clipboard chord.
            // The no-protocol case sets this field to `None`.
            clipboard_rung: Some(clipboard::Rung::Wlr),
            autostart_on: false,
            fonts: vec![Cow::Borrowed("Noto Sans")],
            capture_w: "480".to_string(),
            capture_h: "160".to_string(),
            selected: None,
            hover: None,
            drag: Drag::Idle,
            add_path: String::new(),
            picking: false,
            rebuild_progress: None,
            checking_update: false,
            status: String::new(),
        }
    }

    /// The add-card row uses the trigger row's status vocabulary. On the portal
    /// rung, it names the key published for `anki-add` and offers no snippet.
    /// A pasted compositor bind does not own that key.
    #[test]
    fn the_add_card_row_reports_the_portals_own_add_key() {
        let dir = scratch("addportalrow");
        let mut app = app(&dir);
        app.add_channel = HotkeyChannel::Portal { current_binding: Some("Meta+A".into()) };

        assert_eq!(
            HotkeyControl::Rebind { current: Some("Meta+A".into()) },
            app.add_control()
        );
        // No copy button: a portal row must not offer a bind that differs from the
        // active key.
        assert_eq!(None, app.add_bind_snippet());

        // The trigger row keeps its own key. The two rows use two channels.
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
        // The full window still builds both portal rows. The status block is a widget
        // tree, not only a control value.
        let _ = view(&app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// When the daemon publishes no key, the add row offers a pasteable bind.
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

    /// The checkbox matters only if it reaches the config. This toggle writes
    /// `anki.first_dict_only`, which the Linux daemon already reads.
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

    #[test]
    fn selection_controls_round_trip_into_the_config() {
        let dir = scratch("selectioncontrols");
        let mut app = app(&dir);
        let cfg = chibipop::config::Config::default();
        let _ = update(
            &mut app,
            Message::SelectionButtonsPicked("Primary replacing".to_string()),
        );
        let _ = update(
            &mut app,
            Message::SelectionSeparatorPicked("List items".to_string()),
        );
        let out = chibipop::settings::apply_to(&app.form, &cfg);
        assert_eq!(
            chibipop::config::SelectionButtons::PrimaryReplacing,
            out.anki.selection_buttons
        );
        assert_eq!(
            chibipop::config::SelectionSeparator::ListItems,
            out.anki.selection_separator
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn edge_autoscroll_toggle_round_trips_into_the_config() {
        let dir = scratch("edgeautoscroll");
        let mut app = app(&dir);
        let cfg = chibipop::config::Config::default();
        let _ = update(&mut app, Message::EdgeAutoscroll(false));
        assert!(!chibipop::settings::apply_to(&app.form, &cfg).popup.edge_autoscroll);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// On the native rung, this row turns the typed chord into a bind for the
    /// binary in use and the `ocr-clipboard` verb. It never uses a portal key
    /// because this action has no portal id.
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

    /// A cleared box stores an absent `Option`, not a `""` chord. The config field
    /// keeps absence typed, so the row offers no bind.
    #[test]
    fn a_cleared_ocr_clipboard_chord_is_absent_rather_than_an_empty_string() {
        let dir = scratch("ocrclipclear");
        let mut app = app(&dir);

        let _ = update(&mut app, Message::OcrClipboardKey("ALT+C".to_string()));
        assert_eq!(Some("ALT+C".to_string()), app.linux.ocr_clipboard_key_linux);

        let _ = update(&mut app, Message::OcrClipboardKey("   ".to_string()));
        assert_eq!(None, app.linux.ocr_clipboard_key_linux, "whitespace is not a chord");
        assert_eq!(None, app.ocr_clipboard_bind_snippet());
        // The copy action does nothing, so it cannot paste an old bind.
        let _ = update(&mut app, Message::CopyOcrClipboardBind);
        assert_eq!(None, app.ocr_clipboard_bind_snippet());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Stock GNOME has no usable clipboard protocol. This window offers no
    /// OCR-to-clipboard bind because such a chord would only log a refusal
    /// (ARCHITECTURE.md#settings-and-config).
    #[test]
    fn a_session_with_no_clipboard_protocol_offers_no_ocr_clipboard_bind() {
        let dir = scratch("ocrclipnoproto");
        let mut app = app(&dir);
        let _ = update(&mut app, Message::OcrClipboardKey("ALT+C".to_string()));
        assert!(app.ocr_clipboard_bind_snippet().is_some(), "a session that can copy offers one");

        app.clipboard_rung = None;

        assert_eq!(None, app.ocr_clipboard_bind_snippet());
        // The chord remains in the form. A user who later moves to a compositor with
        // data control keeps the value they typed.
        assert_eq!(Some("ALT+C".to_string()), app.linux.ocr_clipboard_key_linux);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// On the native rung, this row turns the typed chord into a pasteable bind
    /// for the binary in use and the `anki-add` verb. The action has no portal
    /// id, and rung 2 is the only rung for a sway session
    /// (ARCHITECTURE.md#input-ladders).
    #[test]
    fn the_add_card_chord_offers_a_pasteable_bind_for_the_typed_chord() {
        let dir = scratch("addbind");
        let mut app = app(&dir);

        let _ = update(&mut app, Message::AnkiAddKey("CTRL+SHIFT+A".to_string()));
        let snippet = app.add_bind_snippet().expect("a chord has a bind");

        assert_eq!("bind = CTRL SHIFT, A, exec, /usr/bin/chibipop ctl anki-add", snippet);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A cleared chord has no bind. The row must not offer `bind = , A, …`.
    #[test]
    fn a_cleared_add_card_chord_offers_no_bind_at_all() {
        let dir = scratch("addnobind");
        let mut app = app(&dir);

        let _ = update(&mut app, Message::AnkiAddKey(String::new()));

        assert_eq!(HotkeyControl::NoChord, app.add_control());
        assert_eq!(None, app.add_bind_snippet());
        // The copy action does nothing, so it cannot paste an old bind.
        let _ = update(&mut app, Message::CopyAddBind);
        assert_eq!(None, app.add_bind_snippet());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The trigger row still produces the default press and release pair.
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

    /// On the native rung, this row turns the typed chord into a bind for the
    /// binary in use and the `static-region` verb. The action has no portal rung,
    /// so the chord has no other bind path.
    #[test]
    fn the_static_region_chord_offers_a_pasteable_bind_for_the_typed_chord() {
        let dir = scratch("srbind");
        let mut app = app(&dir);

        let _ = update(&mut app, Message::StaticRegionKey("ALT+R".to_string()));
        let snippet = app.static_region_bind_snippet().expect("a chord has a bind");

        assert_eq!("bind = ALT, R, exec, /usr/bin/chibipop ctl static-region", snippet);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Decision D1 covers the user-visible case. This action never registers with
    /// the portal, so the row uses `HotkeyChannel::Native` regardless of the
    /// trigger channel. If it read `app.channel`, it would show the trigger's
    /// portal key under the static-region chord.
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

    /// The default `anki.static_region_key_linux` value is empty. The row therefore
    /// offers no bind instead of `bind = , R, …`.
    #[test]
    fn an_unset_static_region_chord_offers_no_bind_at_all() {
        let dir = scratch("srnobind");
        let mut app = app(&dir);

        assert_eq!("", app.linux.static_region_key_linux, "the shipped default is unset");
        assert_eq!(HotkeyControl::NoChord, app.static_region_control());
        assert_eq!(None, app.static_region_bind_snippet());
        // The copy action does nothing, so it cannot paste an old bind.
        let _ = update(&mut app, Message::CopyStaticRegionBind);
        assert_eq!(None, app.static_region_bind_snippet());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The screenshot row uses an `Option` chord. Only the text box maps `""` to
    /// `None`, so a typed chord is `Some` and a cleared chord is absent, never an
    /// empty-string sentinel in config.
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
        // The copy action does nothing, so it cannot paste an old bind.
        let _ = update(&mut app, Message::CopyScreenshotBind);
        assert_eq!(None, app.screenshot_bind_snippet());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Decision D1 also covers this row. It never registers with the portal, so
    /// it uses `HotkeyChannel::Native` regardless of the trigger channel.
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

    /// These rows apply the inclusion flag and folder. An empty folder uses the
    /// default folder instead of the data directory itself. A relative `save_dir`
    /// joins that directory, so `""` would scatter PNG files beside the database.
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

    /// The sentence picker uses one ordered table. Each `SentenceMode` must appear
    /// in the table, or the window cannot offer it and falls back to `Line`.
    #[test]
    fn every_sentence_mode_round_trips_through_its_own_label() {
        for mode in [SentenceMode::Line, SentenceMode::All, SentenceMode::Static] {
            let label = sentence_mode_label(mode);
            assert_eq!(mode, sentence_mode_of(label), "{label}");
            assert!(sentence_labels().iter().any(|l| l == label), "{label} must be offered");
        }
        assert_eq!(3, sentence_labels().len(), "the table is the whole list");
    }

    /// A *Static region* choice stores the mode on the shared form. Apply writes it,
    /// and the daemon reads it.
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

    /// Windows hides region rows outside Static, and Linux does the same. The chord
    /// row remains visible because the user can set the region before the mode
    /// changes. Row count is the observable because iced hides widget details.
    #[test]
    fn the_outline_checkbox_is_static_only_but_the_chord_row_always_shows() {
        let dir = scratch("srrows");
        let mut app = app(&dir);

        // The picker, chord, and bind text make three rows without the checkbox.
        assert_eq!(3, sentence_rows(&app).len(), "not static: no region checkbox");

        let _ = update(&mut app, Message::SentenceModePicked("Static region".to_string()));
        // Static mode adds the checkbox and its layer-shell caption.
        assert_eq!(5, sentence_rows(&app).len(), "static: the checkbox joins");

        // The chord row remains in a non-static mode. This differs from Windows.
        let _ = update(&mut app, Message::SentenceModePicked("All lines".to_string()));
        let _ = update(&mut app, Message::StaticRegionKey("ALT+R".to_string()));
        assert!(
            app.static_region_bind_snippet().is_some(),
            "the region can be set in any mode, so its bind is offered in any mode"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// This test covers the absent screenshot map.
    /// Before this window could add a row and pick a source, `shot::plan` had no
    /// field to read. A Linux user had to edit TOML.
    /// The test checks the first `screenshot` row and its Anki field because `plan`
    /// reads those values.
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
        // The new row starts with a source that the user can pick.
        // It has a value before the user touches the picker.
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

        // The picker can change the source and preserve the typed Anki field.
        let _ = update(&mut app, Message::FieldMapSource(at, "sentence".to_string()));
        assert_eq!("sentence", form_rows(&app)[at].source);
        assert_eq!("Picture", form_rows(&app)[at].anki_field);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An invalid index could remove a field-map row that the user still needs.
    /// Check survivor names instead of only the row count.
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

        // A press that arrives after its row is gone does nothing. It cannot make
        // `Vec::remove` panic in the UI thread.
        let _ = update(&mut app, Message::FieldMapRemove(9));
        assert_eq!(2, form_rows(&app).len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unnamed row cannot reach the config. Anki has no field named "", so the row
    /// would look valid but place nothing. Apply owns this filter, so the test
    /// reads the file that Apply writes.
    #[test]
    fn a_row_with_no_anki_field_never_reaches_the_saved_config() {
        let dir = scratch("fmblank");
        let mut app = app(&dir);
        let shipped = form_rows(&app).len();

        let _ = update(&mut app, Message::FieldMapAdd);
        let named = form_rows(&app).len() - 1;
        let _ = update(&mut app, Message::FieldMapAnki(named, "Picture".to_string()));
        // Add one untouched row and one row that contains spaces. Spaces are no more
        // a field name than "".
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
        // The window shows the saved file state, as it does for the clamped capture size.
        assert_eq!(shipped + 1, form_rows(&app).len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// When the user removes every row, that is a valid answer. Apply must write an
    /// empty map, not the shipped map. Read the saved file
    /// because the old guard was invisible in the form and both paths reported
    /// success.
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

    /// The picker uses core's [`FIELD_SOURCES`] and keeps no duplicate vocabulary.
    /// Every offered item round-trips. An unknown source never reaches the form.
    /// `anki::mapped_fields` drops it without a message, so the picker must reject
    /// it.
    #[test]
    fn the_source_picker_only_ever_yields_a_source_core_understands() {
        let dir = scratch("fmvocab");
        let mut app = app(&dir);
        for source in FIELD_SOURCES {
            assert_eq!(Some(source), field_source_of(source), "{source} must be offered");
        }
        // Windows adds this value for "unmapped". Linux does not store this source.
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

    /// Row count is the observable because iced hides widget details. The function
    /// must render every field-map row and keep Add with its caption when the list
    /// is empty.
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

        // The full window builds with a screenshot row. The picker is a widget, not
        // only a lookup.
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

    /// The preview label previously ignored the combo font and used iced's default font.
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

    /// A configured font with no installed face remains selectable. The preview uses
    /// iced's default and does not name a family that iced cannot resolve.
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

    /// The combo uses JP-capable families from the popup classifier. A Chinese
    /// pan-CJK face and a Latin "Gothic" family do not pass.
    #[test]
    fn the_font_combo_offers_only_families_that_draw_japanese() {
        let installed =
            ["DejaVu Sans", "Noto Sans CJK JP", "Noto Sans CJK SC", "IPAGothic", "Century Gothic"];

        assert_eq!(
            vec!["Noto Sans CJK JP".to_string(), "IPAGothic".to_string()],
            offered(installed.iter().map(|f| (*f).to_string()).collect()),
        );
    }

    /// If no Japanese face exists, the popup's visible fallback returns every
    /// installed family instead of an empty combo.
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

        assert_eq!(
            vec![("FixtureTerms".to_string(), true)],
            checked(&app, Role::Terms),
            "a term archive lands at the bottom of the Terms section, checked",
        );
        assert!(listed(&app, Role::Frequency).is_empty(), "and in no other section");
        assert!(listed(&app, Role::Pitch).is_empty());
        assert!(app.form.has_staged(), "the add must wait for a rebuild");
        assert!(app.add_path.is_empty(), "the entry clears once the path is taken");
        assert!(app.status.contains("Rebuild"), "{}", app.status);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One Browse dialog can return many archives, and the window stages all of them.
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

    /// A mixed result stages readable archives and names refused files. A count of
    /// successes alone would make the user search for a refused file.
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

    /// A dismissed dialog is not an error. The window must not report it as one.
    #[test]
    fn a_dismissed_dialog_stages_nothing_and_reopens_the_button() {
        let dir = scratch("picked-cancel");
        let mut app = app(&dir);
        app.picking = true;
        // Both states of the Browse gate build a widget tree. A disabled button and an
        // enabled button use different iced paths.
        let _ = view(&app);
        let _ = update(&mut app, Message::DictPicked(Ok(filechooser::Picked::Cancelled)));

        assert!(!app.picking);
        assert!(!app.form.has_staged());
        assert_eq!("No dictionary was chosen.", app.status);
        let _ = view(&app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rebuild can claim the library while the dialog stays open. `busy` rejects
    /// a form update while the builder reads the form.
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

    /// If the desktop has no portal, the window reports that state and keeps the
    /// typed path available.
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

    /// Test the expansion function directly. This avoids a change to the process
    /// `HOME` for other tests.
    #[test]
    fn a_typed_tilde_path_expands_against_home() {
        use crate::paths::{expand_tilde, Typed};
        let home = Path::new("/home/u");
        let h = Some(home);

        assert_eq!(
            Typed::Path(PathBuf::from("/home/u/Downloads/jitendex.zip")),
            expand_tilde("~/Downloads/jitendex.zip", h)
        );
        // Bare `~` and `~/` resolve to the home directory without an extra separator
        // or a panic.
        assert_eq!(Typed::Path(home.to_path_buf()), expand_tilde("~", h));
        assert_eq!(Typed::Path(home.to_path_buf()), expand_tilde("~/", h));
        // Other paths remain unchanged. Absolute and relative paths keep their form, and
        // `~` in the middle is a literal file-name character.
        assert_eq!(Typed::Path(PathBuf::from("/tmp/a.zip")), expand_tilde("/tmp/a.zip", h));
        assert_eq!(Typed::Path(PathBuf::from("dl/a.zip")), expand_tilde("dl/a.zip", h));
        assert_eq!(Typed::Path(PathBuf::from("dl/~/a.zip")), expand_tilde("dl/~/a.zip", h));
        // These two forms return refusal states.
        assert_eq!(Typed::UserRelative, expand_tilde("~root/a.zip", h));
        assert_eq!(Typed::NoHome, expand_tilde("~/a.zip", None));
        assert_eq!(Typed::NoHome, expand_tilde("~", None));
    }

    /// Test the full message path. A `~` path stages the same archive as its absolute path.
    #[test]
    fn adding_a_tilde_path_stages_the_same_archive_the_absolute_path_does() {
        let dir = scratch("add_tilde");
        let mut app = app(&dir);
        // The test uses the fixture directory as the home path and leaves the shared
        // process environment unchanged.
        app.home = Some(fixture("terms.zip").parent().unwrap().to_path_buf());

        let _ = update(&mut app, Message::AddPath("~/terms.zip".to_string()));
        let _ = update(&mut app, Message::DictAdd);

        assert_eq!(vec![("FixtureTerms".to_string(), true)], checked(&app, Role::Terms));
        assert!(app.form.has_staged(), "the add must wait for a rebuild");
        assert!(app.add_path.is_empty(), "the entry clears once the path is taken");
        assert!(app.status.contains("Rebuild"), "{}", app.status);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Bare `~` resolves to a directory, not an archive. The message must name that
    /// resolved path.
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

    /// `~user/…` and an unset `$HOME` report their reasons. The code does not probe
    /// a literal `~` path.
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

    /// A non-dictionary path produces a refusal, not silent omission.
    #[test]
    fn adding_an_unreadable_path_says_so_and_stages_nothing() {
        let dir = scratch("add_bad");
        let mut app = app(&dir);
        let before = app.form.terms.clone();
        let _ = update(&mut app, Message::AddPath(dir.join("nope.zip").display().to_string()));
        let _ = update(&mut app, Message::DictAdd);

        assert_eq!(before, app.form.terms);
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
        app.form.terms = rows(&["Jitendex", "Daijirin"]);

        let _ = update(&mut app, Message::DictSelected(Role::Terms, "Jitendex".to_string()));
        let _ = update(&mut app, Message::DictRemove);

        assert_eq!(rows(&["Daijirin"]), app.form.terms);
        assert!(app.form.has_staged());
        assert_eq!(None, app.selected);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Each section renders its own role list. The test checks enabled, disabled,
    /// selected, and empty-list rows because iced hides widget details.
    #[test]
    fn each_section_renders_its_own_role_s_list() {
        let dir = scratch("sections");
        let mut app = app(&dir);
        app.form.terms = rows(&["Jitendex", "Daijirin"]);
        app.form.frequency = rows(&["JPDB"]);
        app.form.pitch = vec![DictRow { name: "NHK".to_string(), enabled: false }];

        assert_eq!(
            vec!["Jitendex".to_string(), "Daijirin".to_string()],
            listed(&app, Role::Terms)
        );
        assert_eq!(vec!["JPDB".to_string()], listed(&app, Role::Frequency));
        assert_eq!(vec![("NHK".to_string(), false)], checked(&app, Role::Pitch));

        for role in Role::EVERY {
            let name = listed(&app, role).remove(0);
            let _ = update(&mut app, Message::DictSelected(role, name));
            let _ = view(&app);
        }
        for role in Role::EVERY {
            app.form.list_mut(role).clear();
        }
        let _ = view(&app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pitch archive has no terms or frequency rank. It appears only in Pitch and
    /// still reaches the config.
    #[test]
    fn a_pitch_only_import_is_a_row_in_the_pitch_section_and_nowhere_else() {
        let dir = scratch("add_pitch");
        let mut app = app(&dir);
        let _ = update(&mut app, Message::AddPath(fixture("pitch.zip").display().to_string()));
        let _ = update(&mut app, Message::DictAdd);

        assert_eq!(vec![("FixturePitch".to_string(), true)], checked(&app, Role::Pitch));
        assert!(listed(&app, Role::Terms).is_empty(), "a pitch archive defines nothing");
        assert!(listed(&app, Role::Frequency).is_empty());
        assert_eq!(vec!["FixturePitch".to_string()], saved(&app).dictionaries.pitch);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An archive with two roles appears once in each section for its role and nowhere
    /// else. `both.zip` contains a term bank and pitch rows.
    #[test]
    fn a_mixed_import_is_one_row_in_each_of_its_sections() {
        let dir = scratch("add_both");
        let mut app = app(&dir);
        let _ = update(&mut app, Message::AddPath(fixture("both.zip").display().to_string()));
        let _ = update(&mut app, Message::DictAdd);

        assert_eq!(vec!["FixtureBoth".to_string()], listed(&app, Role::Terms));
        assert_eq!(vec!["FixtureBoth".to_string()], listed(&app, Role::Pitch));
        assert!(listed(&app, Role::Frequency).is_empty(), "it ranks nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One Dictionary can have independent positions in two sections. No fixture
    /// covers the terms and frequency pair, so the test seeds that state directly.
    /// It checks how each section handles the shared name.
    #[test]
    fn a_dictionary_in_two_sections_keeps_a_place_in_each() {
        let dir = scratch("mixed_order");
        let mut app = app(&dir);
        app.form.terms = rows(&["Jitendex", "大辞林"]);
        app.form.frequency = rows(&["JPDB", "大辞林"]);

        let count = |names: Vec<String>| names.iter().filter(|n| *n == "大辞林").count();
        assert_eq!(1, count(listed(&app, Role::Terms)));
        assert_eq!(1, count(listed(&app, Role::Frequency)));
        assert!(listed(&app, Role::Pitch).is_empty());

        let _ = update(&mut app, Message::DictSelected(Role::Frequency, "大辞林".to_string()));
        let _ = update(&mut app, Message::DictUp(Role::Frequency));

        assert_eq!(
            vec!["大辞林".to_string(), "JPDB".to_string()],
            listed(&app, Role::Frequency)
        );
        assert_eq!(
            vec!["Jitendex".to_string(), "大辞林".to_string()],
            listed(&app, Role::Terms),
            "its rank order is not its definition order",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A checkbox changes only its own section. It must not disable frequency data
    /// or pitch data for a mixed archive. The row keeps its position because order
    /// and enabled state are separate.
    #[test]
    fn a_checkbox_turns_off_only_the_role_of_its_own_section() {
        let dir = scratch("checkbox");
        let mut app = app(&dir);
        app.form.terms = rows(&["Jitendex", "大辞林"]);
        app.form.frequency = rows(&["大辞林"]);
        app.form.pitch = rows(&["大辞林"]);

        let _ = update(&mut app, Message::DictEnabled(Role::Terms, "大辞林".to_string(), false));

        assert_eq!(
            vec![("Jitendex".to_string(), true), ("大辞林".to_string(), false)],
            checked(&app, Role::Terms),
            "unchecked where it stood",
        );
        assert_eq!(vec![("大辞林".to_string(), true)], checked(&app, Role::Frequency));
        assert_eq!(vec![("大辞林".to_string(), true)], checked(&app, Role::Pitch));

        // Enable the row again. It keeps its position.
        let _ = update(&mut app, Message::DictEnabled(Role::Terms, "大辞林".to_string(), true));
        assert_eq!(rows(&["Jitendex", "大辞林"]), app.form.terms);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Move up and Move down change one section at a time. A list end has no
    /// destination.
    #[test]
    fn moving_a_row_reorders_only_its_own_section_and_stops_at_the_ends() {
        let dir = scratch("move");
        let mut app = app(&dir);
        app.form.terms = rows(&["Jitendex", "Daijirin", "Kenkyusha"]);
        app.form.frequency = rows(&["Daijirin", "JPDB"]);

        let _ = update(&mut app, Message::DictSelected(Role::Terms, "Kenkyusha".to_string()));
        let _ = update(&mut app, Message::DictUp(Role::Terms));

        assert_eq!(
            vec!["Jitendex".to_string(), "Kenkyusha".to_string(), "Daijirin".to_string()],
            listed(&app, Role::Terms),
        );
        assert_eq!(
            vec!["Daijirin".to_string(), "JPDB".to_string()],
            listed(&app, Role::Frequency),
            "a terms move is not a frequency move",
        );

        // Move down returns the row to its prior position.
        let _ = update(&mut app, Message::DictDown(Role::Terms));
        assert_eq!(
            vec!["Jitendex".to_string(), "Daijirin".to_string(), "Kenkyusha".to_string()],
            listed(&app, Role::Terms),
        );

        // At both ends, a button press does nothing. It does not wrap or enter the
        // next section.
        let _ = update(&mut app, Message::DictSelected(Role::Terms, "Jitendex".to_string()));
        let _ = update(&mut app, Message::DictUp(Role::Terms));
        let _ = update(&mut app, Message::DictSelected(Role::Terms, "Kenkyusha".to_string()));
        let _ = update(&mut app, Message::DictDown(Role::Terms));

        assert_eq!(
            vec!["Jitendex".to_string(), "Daijirin".to_string(), "Kenkyusha".to_string()],
            listed(&app, Role::Terms),
        );
        assert_eq!(vec!["Daijirin".to_string(), "JPDB".to_string()], listed(&app, Role::Frequency));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A section button responds only to a selection in that section. A Dictionary
    /// can occur in two sections, so a Frequency button must not reorder a name
    /// selected in Terms.
    #[test]
    fn a_move_button_does_nothing_while_the_selection_is_in_another_section() {
        let dir = scratch("move_elsewhere");
        let mut app = app(&dir);
        app.form.terms = rows(&["Jitendex", "大辞林"]);
        app.form.frequency = rows(&["Jitendex", "JPDB"]);

        let _ = update(&mut app, Message::DictSelected(Role::Terms, "Jitendex".to_string()));
        let _ = update(&mut app, Message::DictDown(Role::Frequency));

        assert_eq!(vec!["Jitendex".to_string(), "JPDB".to_string()], listed(&app, Role::Frequency));
        assert_eq!(vec!["Jitendex".to_string(), "大辞林".to_string()], listed(&app, Role::Terms));

        // A button in the selected section does move the row.
        let _ = update(&mut app, Message::DictDown(Role::Terms));
        assert_eq!(vec!["大辞林".to_string(), "Jitendex".to_string()], listed(&app, Role::Terms));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `drop_index` returns the drop boundary above the pointer. The top half of a
    /// row sets the boundary before it, and the bottom half sets it after it. Values
    /// outside the list clamp to its ends.
    ///
    /// The test checks only arithmetic. It does not need a window, renderer, or font.
    #[test]
    fn the_drop_index_counts_the_row_boundaries_above_the_cursor() {
        assert_eq!(0, drop_index(0.0, 3), "the very top of the list");
        assert_eq!(0, drop_index(ROW_HEIGHT / 2.0 - 1.0, 3), "the top half of the first row");
        assert_eq!(1, drop_index(ROW_PITCH / 2.0, 3), "and its bottom half");
        assert_eq!(1, drop_index(ROW_PITCH, 3), "the boundary itself");
        assert_eq!(2, drop_index(ROW_PITCH * 2.0, 3));
        assert_eq!(3, drop_index(ROW_PITCH * 9.0, 3), "far below the list, at its end");
        assert_eq!(0, drop_index(-ROW_PITCH * 9.0, 3), "far above it, at its start");
        assert_eq!(0, drop_index(ROW_PITCH * 9.0, 0), "an empty list has one place to be");
    }

    /// The drop line fills the gap between rows without a layout change. At either
    /// end, it stays inside the list because the stack has no space outside.
    #[test]
    fn the_insertion_line_fills_the_gap_it_marks_and_stays_inside_the_list() {
        assert_eq!(0.0, line_top(0, 3), "the top of the list, not two pixels above it");
        assert_eq!(ROW_HEIGHT, line_top(1, 3), "the gap under the first row");
        assert_eq!(ROW_PITCH + ROW_HEIGHT, line_top(2, 3));

        let height = 3.0 * ROW_PITCH - ROW_SPACING;
        assert_eq!(height - ROW_SPACING, line_top(3, 3), "the end of the list, still within it");
        assert_eq!(0.0, line_top(0, 0));
    }

    /// A click must not reorder a row. A pointer outside the source list counts as
    /// past the deadband because the lists use different heights.
    ///
    /// The test checks only arithmetic, like the `drop_index` test.
    #[test]
    fn a_press_only_becomes_a_drag_once_the_cursor_leaves_the_deadband() {
        assert!(!past_deadband(40.0, 40.0), "a press that has not moved at all");
        assert!(!past_deadband(40.0, 40.0 + DRAG_DEADBAND), "nor one right on the edge");
        assert!(past_deadband(40.0, 40.0 + DRAG_DEADBAND + 0.5));
        assert!(past_deadband(40.0, 40.0 - DRAG_DEADBAND - 0.5), "upwards counts the same");

        let terms = |y| Hover { role: Role::Terms, y };
        assert!(!left_the_deadband(Role::Terms, 40.0, terms(44.0)));
        assert!(left_the_deadband(Role::Terms, 40.0, terms(60.0)));
        assert!(left_the_deadband(Role::Terms, 40.0, Hover { role: Role::Pitch, y: 40.0 }));
    }

    /// This test sends the gesture messages in order. The pointer reaches a row,
    /// the press holds it, moves set the drop line, and release moves it.
    #[test]
    fn dragging_a_row_reorders_it_where_the_pointer_let_go() {
        let dir = scratch("drag");
        let mut app = app(&dir);
        app.form.terms = rows(&["Jitendex", "Daijirin", "Kenkyusha"]);

        hover(&mut app, Role::Terms, 0, ROW_HEIGHT / 2.0);
        let _ = update(&mut app, Message::DictSelected(Role::Terms, "Jitendex".to_string()));
        hover(&mut app, Role::Terms, 2, ROW_HEIGHT * 0.75);
        let _ = update(&mut app, Message::DictDropped);

        assert_eq!(
            vec!["Daijirin".to_string(), "Kenkyusha".to_string(), "Jitendex".to_string()],
            listed(&app, Role::Terms),
        );
        assert_eq!(Drag::Idle, app.drag, "the pointer is holding nothing after a drop");

        // Repeat the gesture upward. A drag can move both directions.
        hover(&mut app, Role::Terms, 2, ROW_HEIGHT / 2.0);
        let _ = update(&mut app, Message::DictSelected(Role::Terms, "Jitendex".to_string()));
        hover(&mut app, Role::Terms, 0, ROW_HEIGHT * 0.15);
        let _ = update(&mut app, Message::DictDropped);

        assert_eq!(
            vec!["Jitendex".to_string(), "Daijirin".to_string(), "Kenkyusha".to_string()],
            listed(&app, Role::Terms),
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A row that leaves its section moves to the end that it crossed. Sections
    /// follow `Role::EVERY` order, so a later section means the bottom and an earlier
    /// section means the top. The other section stays unchanged because a Dictionary
    /// has no cross-role order.
    #[test]
    fn a_drag_that_leaves_its_section_clamps_to_that_sections_ends() {
        let dir = scratch("drag_out");
        let mut app = app(&dir);
        app.form.terms = rows(&["Jitendex", "Daijirin", "Kenkyusha"]);
        app.form.frequency = rows(&["JPDB", "BCCWJ", "Innocent"]);
        app.form.pitch = rows(&["NHK", "大辞林"]);

        // Move down from Terms, across Frequency, and into Pitch.
        hover(&mut app, Role::Terms, 0, ROW_HEIGHT / 2.0);
        let _ = update(&mut app, Message::DictSelected(Role::Terms, "Jitendex".to_string()));
        hover(&mut app, Role::Pitch, 1, ROW_HEIGHT * 0.75);
        let _ = update(&mut app, Message::DictDropped);

        assert_eq!(
            vec!["Daijirin".to_string(), "Kenkyusha".to_string(), "Jitendex".to_string()],
            listed(&app, Role::Terms),
        );
        assert_eq!(
            vec!["NHK".to_string(), "大辞林".to_string()],
            listed(&app, Role::Pitch),
            "the list it was dragged over gains nothing",
        );

        // Move up from Frequency and into Terms.
        hover(&mut app, Role::Frequency, 2, ROW_HEIGHT / 2.0);
        let _ = update(&mut app, Message::DictSelected(Role::Frequency, "Innocent".to_string()));
        hover(&mut app, Role::Terms, 0, ROW_HEIGHT * 0.25);
        let _ = update(&mut app, Message::DictDropped);

        assert_eq!(
            vec!["Innocent".to_string(), "JPDB".to_string(), "BCCWJ".to_string()],
            listed(&app, Role::Frequency),
        );
        assert_eq!(
            vec!["Daijirin".to_string(), "Kenkyusha".to_string(), "Jitendex".to_string()],
            listed(&app, Role::Terms),
            "and the list it was dragged over is left exactly as it was",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A press and release inside the deadband selects the row and preserves order.
    /// A longer move reorders it. The test checks the deadband, not a no-op drop.
    #[test]
    fn a_press_and_release_inside_the_deadband_selects_and_moves_nothing() {
        let dir = scratch("drag_deadband");
        let mut app = app(&dir);
        app.form.terms = rows(&["Jitendex", "Daijirin", "Kenkyusha"]);

        hover(&mut app, Role::Terms, 1, ROW_HEIGHT / 2.0);
        let _ = update(&mut app, Message::DictSelected(Role::Terms, "Daijirin".to_string()));
        hover(&mut app, Role::Terms, 1, ROW_HEIGHT / 2.0 + DRAG_DEADBAND - 1.0);
        assert_eq!(None, drop_line(&app, Role::Terms), "no line, because this is still a click");
        let _ = update(&mut app, Message::DictDropped);

        assert_eq!(
            vec!["Jitendex".to_string(), "Daijirin".to_string(), "Kenkyusha".to_string()],
            listed(&app, Role::Terms),
        );
        assert_eq!(
            Some(Selected { role: Role::Terms, name: "Daijirin".to_string() }),
            app.selected,
            "a click is still a selection",
        );

        // The same held row moves after it crosses the deadband.
        hover(&mut app, Role::Terms, 1, ROW_HEIGHT / 2.0);
        let _ = update(&mut app, Message::DictSelected(Role::Terms, "Daijirin".to_string()));
        hover(&mut app, Role::Terms, 2, ROW_HEIGHT * 0.75);
        let _ = update(&mut app, Message::DictDropped);

        assert_eq!(
            vec!["Jitendex".to_string(), "Kenkyusha".to_string(), "Daijirin".to_string()],
            listed(&app, Role::Terms),
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The checkbox handles its own press before the row's [`mouse_area`] sees it.
    /// A role toggle does not hold the row, and the release has nothing to drop.
    /// The row keeps its position.
    #[test]
    fn toggling_a_checkbox_never_grabs_or_moves_its_row() {
        let dir = scratch("drag_checkbox");
        let mut app = app(&dir);
        app.form.terms = rows(&["Jitendex", "Daijirin", "Kenkyusha"]);

        hover(&mut app, Role::Terms, 1, ROW_HEIGHT / 2.0);
        let _ = update(&mut app, Message::DictEnabled(Role::Terms, "Daijirin".to_string(), false));
        let _ = update(&mut app, Message::DictReleased);

        assert_eq!(
            vec![
                ("Jitendex".to_string(), true),
                ("Daijirin".to_string(), false),
                ("Kenkyusha".to_string(), true),
            ],
            checked(&app, Role::Terms),
            "unchecked where it stood",
        );
        assert_eq!(Drag::Idle, app.drag, "a checkbox press never took hold of anything");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A release outside the list ends the drag and leaves the row in place. Otherwise
    /// the window would keep the drop line for a row that no pointer holds.
    #[test]
    fn a_drag_released_outside_the_window_cancels_and_moves_nothing() {
        let dir = scratch("drag_escape");
        let mut app = app(&dir);
        app.form.terms = rows(&["Jitendex", "Daijirin", "Kenkyusha"]);

        hover(&mut app, Role::Terms, 0, ROW_HEIGHT / 2.0);
        let _ = update(&mut app, Message::DictSelected(Role::Terms, "Jitendex".to_string()));
        hover(&mut app, Role::Terms, 2, ROW_HEIGHT * 0.75);
        assert_eq!(Some(3), drop_line(&app, Role::Terms), "the line is at the end of the list");

        let _ = update(&mut app, Message::DictReleased);

        assert_eq!(Drag::Idle, app.drag);
        assert_eq!(None, drop_line(&app, Role::Terms), "and the line goes with it");
        assert_eq!(
            vec!["Jitendex".to_string(), "Daijirin".to_string(), "Kenkyusha".to_string()],
            listed(&app, Role::Terms),
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The drop line belongs to the section that contains the row. Other sections
    /// draw no line, and the window still builds the widget tree.
    #[test]
    fn the_insertion_line_marks_the_drop_position_in_the_held_rows_section() {
        let dir = scratch("drag_line");
        let mut app = app(&dir);
        app.form.terms = rows(&["Jitendex", "Daijirin", "Kenkyusha"]);
        app.form.frequency = rows(&["JPDB", "BCCWJ"]);

        hover(&mut app, Role::Terms, 0, ROW_HEIGHT / 2.0);
        let _ = update(&mut app, Message::DictSelected(Role::Terms, "Jitendex".to_string()));
        hover(&mut app, Role::Terms, 1, ROW_HEIGHT * 0.75);

        assert_eq!(Some(2), drop_line(&app, Role::Terms), "under the second row");
        assert_eq!(None, drop_line(&app, Role::Frequency));
        assert_eq!(None, drop_line(&app, Role::Pitch));
        let _ = view(&app);

        // Outside its section, the line stays at the crossed end and remains in the
        // source section.
        hover(&mut app, Role::Frequency, 1, ROW_HEIGHT / 2.0);
        assert_eq!(Some(3), drop_line(&app, Role::Terms));
        assert_eq!(None, drop_line(&app, Role::Frequency), "the list it is over is not its list");
        let _ = view(&app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every role uses the same gesture path. The same three messages produce the
    /// same order in each section.
    #[test]
    fn every_section_drags_the_same_way() {
        let dir = scratch("drag_every");
        for role in Role::EVERY {
            let mut app = app(&dir);
            *app.form.list_mut(role) = rows(&["one", "two", "three"]);

            hover(&mut app, role, 0, ROW_HEIGHT / 2.0);
            let _ = update(&mut app, Message::DictSelected(role, "one".to_string()));
            hover(&mut app, role, 2, ROW_HEIGHT * 0.75);
            let _ = update(&mut app, Message::DictDropped);

            assert_eq!(
                vec!["two".to_string(), "three".to_string(), "one".to_string()],
                listed(&app, role),
                "{role:?} reorders like the other two",
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One archive is one Dictionary. A row selected in any section names the whole Dictionary.
    #[test]
    fn removing_a_dictionary_drops_it_from_every_section() {
        let dir = scratch("remove_all");
        let mut app = app(&dir);
        app.form.terms = rows(&["Jitendex", "大辞林"]);
        app.form.frequency = rows(&["大辞林", "JPDB"]);
        app.form.pitch = rows(&["大辞林"]);

        let _ = update(&mut app, Message::DictSelected(Role::Pitch, "大辞林".to_string()));
        let _ = update(&mut app, Message::DictRemove);

        assert_eq!(vec!["Jitendex".to_string()], listed(&app, Role::Terms));
        assert_eq!(vec!["JPDB".to_string()], listed(&app, Role::Frequency));
        assert!(listed(&app, Role::Pitch).is_empty());
        assert!(app.form.has_staged());
        assert_eq!(None, app.selected);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unreadable archive has no role and appears in no role section.
    /// `settings::with_library` still lists it in Terms so the user can remove it.
    /// The config arrays do not contain it while it stays unreadable.
    #[test]
    fn an_unreadable_archive_is_listed_in_terms_and_can_still_be_removed() {
        let dir = scratch("unreadable_row");
        let mut app = app(&dir);
        app.form.terms = rows(&["Jitendex"]);
        app.form.terms.push(DictRow { name: "broken.zip".to_string(), enabled: false });
        app.form.unreadable = vec!["broken.zip".to_string()];

        assert_eq!(
            vec![("Jitendex".to_string(), true), ("broken.zip".to_string(), false)],
            checked(&app, Role::Terms),
        );
        let cfg = saved(&app);
        assert_eq!(vec!["Jitendex".to_string()], cfg.dictionaries.terms);
        assert!(cfg.dictionaries.terms_disabled.is_empty(), "a listed file is not a Dictionary");
        let _ = view(&app);

        let _ = update(&mut app, Message::DictSelected(Role::Terms, "broken.zip".to_string()));
        let _ = update(&mut app, Message::DictRemove);

        assert_eq!(vec!["Jitendex".to_string()], listed(&app, Role::Terms));
        assert!(app.form.has_staged(), "the removal waits for a rebuild");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_ranking_strategy_round_trips_through_its_own_label() {
        for strategy in
            [RankingStrategy::BestRank, RankingStrategy::Priority, RankingStrategy::Median]
        {
            let label = ranking_strategy_label(strategy);
            assert_eq!(strategy, ranking_strategy_of(label), "{label}");
        }
        assert_eq!(RANKING_STRATEGIES.len(), ranking_labels().len());
    }

    /// The picker writes the form field that the Frequency reindex uses. This is
    /// the only effect of that control.
    #[test]
    fn picking_a_ranking_strategy_stages_it_on_the_form() {
        let dir = scratch("ranking");
        let mut app = app(&dir);
        assert_eq!(RankingStrategy::BestRank, app.form.ranking_strategy, "the shipped default");

        let _ = update(
            &mut app,
            Message::RankingPicked(ranking_strategy_label(RankingStrategy::Median).to_string()),
        );

        assert_eq!(RankingStrategy::Median, app.form.ranking_strategy);
        assert_eq!(RankingStrategy::Median, saved(&app).dictionaries.ranking_strategy);
        let _ = view(&app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A Frequency order change makes stored ranks stale. Apply therefore runs an
    /// in-place reindex before it sends `reload`.
    #[test]
    fn reordering_the_frequency_list_reaches_the_reindex_seam() {
        let dir = scratch("seam_order");
        let mut app = app(&dir);
        app.form.frequency = rows(&["JPDB", "Innocent"]);
        let opened = saved(&app);

        let _ = update(&mut app, Message::DictSelected(Role::Frequency, "Innocent".to_string()));
        let _ = update(&mut app, Message::DictUp(Role::Frequency));

        assert_eq!(DictionaryWork::Reindex, work(&opened, &app));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_frequency_checkbox_reaches_the_reindex_seam() {
        let dir = scratch("seam_check");
        let mut app = app(&dir);
        app.form.frequency = rows(&["JPDB", "Innocent"]);
        let opened = saved(&app);

        let _ =
            update(&mut app, Message::DictEnabled(Role::Frequency, "Innocent".into(), false));

        assert_eq!(DictionaryWork::Reindex, work(&opened, &app));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn choosing_a_ranking_strategy_reaches_the_reindex_seam() {
        let dir = scratch("seam_strategy");
        let mut app = app(&dir);
        app.form.frequency = rows(&["JPDB"]);
        let opened = saved(&app);

        let _ = update(
            &mut app,
            Message::RankingPicked(ranking_strategy_label(RankingStrategy::Priority).to_string()),
        );

        assert_eq!(DictionaryWork::Reindex, work(&opened, &app));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A Terms or Pitch edit writes config and sends `reload`. It does not change
    /// Frequency ranks, so it does not need a reindex.
    #[test]
    fn a_terms_or_pitch_change_never_reaches_the_reindex_seam() {
        let dir = scratch("seam_none");
        let mut app = app(&dir);
        app.form.terms = rows(&["Jitendex", "大辞林"]);
        app.form.frequency = rows(&["JPDB"]);
        app.form.pitch = rows(&["NHK", "Kanjium"]);
        let opened = saved(&app);

        let _ = update(&mut app, Message::DictSelected(Role::Terms, "大辞林".to_string()));
        let _ = update(&mut app, Message::DictUp(Role::Terms));
        let _ = update(&mut app, Message::DictEnabled(Role::Terms, "Jitendex".into(), false));
        let _ = update(&mut app, Message::DictSelected(Role::Pitch, "Kanjium".to_string()));
        let _ = update(&mut app, Message::DictUp(Role::Pitch));
        let _ = update(&mut app, Message::DictEnabled(Role::Pitch, "NHK".into(), false));

        let after = saved(&app);
        assert_ne!(opened.dictionaries.terms, after.dictionaries.terms, "the edits landed");
        assert_ne!(opened.dictionaries.pitch, after.dictionaries.pitch);
        assert_eq!(DictionaryWork::None, work(&opened, &app));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The checkbox changes the form and the config that the lookup pipeline reads.
    /// The pipeline uses the exact Dictionary name, not a substring.
    #[test]
    fn an_unchecked_terms_row_never_reaches_the_lookup_pipeline() {
        let dir = scratch("unchecked_terms");
        let mut app = app(&dir);
        let installed = vec![
            chibipop::present::DictInfo { dict_id: 1, name: "Jitendex".to_string() },
            chibipop::present::DictInfo { dict_id: 2, name: "Daijirin".to_string() },
        ];
        app.form.terms = rows(&["Jitendex", "Daijirin"]);

        let _ = update(&mut app, Message::DictEnabled(Role::Terms, "Daijirin".into(), false));
        let present = saved(&app).present_config(&installed);

        assert_eq!(vec!["Jitendex".to_string()], present.terms);
        assert!(chibipop::present::keeps_dict("Jitendex", &present.terms));
        assert!(!chibipop::present::keeps_dict("Daijirin", &present.terms), "parked, so not read");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The shared renderer shows lines for the user and hides raw lines.
    #[test]
    fn progress_lines_render_through_the_shared_renderer() {
        let dir = scratch("progress");
        let mut app = app(&dir);
        let line = |l: &str| Message::RebuildProgress(rebuild::Progress::Line(l.to_string()));

        let _ = update(&mut app, line("progress  12500 / 768636"));
        assert_eq!(Some("12,500 of 768,636 entries…".to_string()), app.rebuild_progress);
        assert!(app.busy(), "a live rebuild keeps the library controls shut");

        // The last raw line must not replace the visible progress line.
        let _ = update(&mut app, line("wrote /tmp/x.sqlite.building: 3 entries"));
        assert_eq!(Some("12,500 of 768,636 entries…".to_string()), app.rebuild_progress);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_finished_rebuild_clears_the_staged_edits_and_reopens_the_controls() {
        let dir = scratch("done");
        let mut app = app(&dir);
        app.form.terms = rows(&["Jitendex"]);
        let _ = update(&mut app, Message::DictSelected(Role::Terms, "Jitendex".to_string()));
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

    /// A failed rebuild returns the archives, so staged edits remain available for a retry.
    #[test]
    fn a_failed_rebuild_keeps_the_staged_edits() {
        let dir = scratch("failed");
        let mut app = app(&dir);
        app.form.terms = rows(&["Jitendex"]);
        let _ = update(&mut app, Message::DictSelected(Role::Terms, "Jitendex".to_string()));
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

    /// If another process holds the library lock, Rebuild reports that process and
    /// starts no build.
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
            dicts: Vec::new(),
            runtime_dir: config_home.join("run"),
            autostart: autostart::Target::resolve(&env),
            home: env.home.clone(),
            exe: PathBuf::from("/usr/bin/chibipop"),
            clipboard_rung: Some(clipboard::Rung::Wlr),
        }
    }

    /// The checkbox handler writes the `.desktop` file and a new window reads its
    /// state from that file. It writes no config file
    /// (ARCHITECTURE.md#settings-and-config).
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

        // The handler applies the toggle. The returned task is a no-op because the
        // handler writes the filesystem directly.
        let _ = update(&mut app, Message::Autostart(true));
        assert!(app.autostart_on);
        assert!(entry.is_file(), "the click wrote {}", entry.display());
        assert!(std::fs::read_to_string(&entry).unwrap().starts_with("[Desktop Entry]"));

        // A new App uses the same paths and reads the checkbox state from the file, not
        // from prior state.
        assert!(App::new(init_at(&home)).autostart_on, "a reopened window sees the entry");

        let _ = update(&mut app, Message::Autostart(false));
        assert!(!app.autostart_on);
        assert!(!entry.exists(), "the second click removed the entry");
        assert!(!App::new(init_at(&home)).autostart_on);

        assert_eq!(form_before, app.form, "autostart touches no config field");
        assert!(!init.config_path.exists(), "autostart writes no config file");

        let _ = std::fs::remove_dir_all(&home);
    }

    /// Without a config root, the row stays inactive and reports the reason. The
    /// checkbox stays clear.
    #[test]
    fn autostart_without_a_config_root_reports_instead_of_toggling() {
        let mut init = init_at(&std::env::temp_dir());
        init.autostart = None;
        let mut app = App::new(init);

        let _ = update(&mut app, Message::Autostart(true));
        assert!(!app.autostart_on);
        assert!(app.status.contains("XDG config directory"), "{}", app.status);
    }

    /// A completed update check supplies a status line and enables the button.
    /// Only the click arm uses the network. This test checks the returned line.
    /// [`super::super::update`] defines each result.
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
