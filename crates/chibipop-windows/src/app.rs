//! The Windows platform bin uses a pump thread, a Worker thread, and an analysis thread.

use crate::anki;
use crate::config::{
    resolve_engine, Config, EngineChoice, SelectionButtons, SelectionSeparator, TripleClick,
};
use crate::controller::{
    Button, Command, Controller, ControllerConfig, Event, LookupOutcome, PopupView, RequestId,
    TrayAction,
};
use chibipop::select::TextAddr;
use crate::geom::{place_popup, PhysPoint, PhysRect, ScanDisplay};
use crate::input::hooks::Hooks;
use crate::library::{Library, Pending, Role, Roles};
use crate::lock::LibraryLock;
use crate::lookup::deconj::Deconjugator;
use crate::lookup::engine::LookupEngine;
use crate::lookup::model::Dictionary;
use crate::lookup::rules::load_rules;
use crate::lookup::sqlite::SqliteDictionary;
use crate::plugin::manifest::Manifest;
use crate::plugin::text::PluginText;
use crate::plugin::{discover, host};
use crate::present::{DictInfo, PresentConfig};
use crate::rebuild::{self, Progress};
use crate::settings::{self, SettingsForm};
use crate::text::capture::{CaptureGuard, CaptureGuardMsg, WinCapture, WM_APP_CAPTURE_GUARD};
use crate::text::layout::CaptureSize;
use crate::text::mask::CaptureMask;
use crate::text::ocr::{recogniser_available, WinrtOcr};
use crate::ui::layout::anki_button_label;
use crate::ui::overlay::Overlay;
use crate::ui::render::{Renderer, SceneInputs};
use crate::ui::settings_window::{ApplyMode, SettingsClick, SettingsOutcome, SettingsWindow};
use crate::ui::static_overlay::StaticRegionOverlay;
use crate::ui::theme::Theme;
use crate::ui::tray::{Tray, TrayCommand};
use crate::ui::window::{AnkiButton, CaptureExclusion, Popup};
use crate::update;
use crate::worker::{
    Hover, SentenceProbe, Trigger, TriggerKind, Worker, WorkerParts, WorkerResult, WorkerSettings,
};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use windows::Win32::Foundation::{LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetCursorPos, GetMessageW, IsDialogMessageW, IsWindowVisible, KillTimer,
    LoadCursorW, PostQuitMessage, PostThreadMessageW, SetCursor, SetTimer, ShowWindow,
    TranslateMessage, IDC_HAND, MSG, SW_HIDE, SW_SHOWNOACTIVATE, WM_APP, WM_KEYDOWN, WM_SYSKEYDOWN,
    WM_TIMER,
};

/// The Worker posts this message after it pushes a result.
const WM_APP_RESULT: u32 = WM_APP + 1;

/// The duplicate check posts this message after it finishes.
const WM_APP_ANKI: u32 = WM_APP + 4;

/// The add-note operation posts this message after it finishes.
const WM_APP_ADD_NOTE: u32 = WM_APP + 5;

/// The settings operation posts this message after it finishes.
const WM_APP_SETTINGS: u32 = WM_APP + 6;

/// Anki deck/model detection posts this message after it finishes.
const WM_APP_ANKI_DETECT: u32 = WM_APP + 7;

/// A background save posts this message after it finishes.
const WM_APP_SAVED: u32 = WM_APP + 9;

/// The screenshot worker posts this message after it finishes.
const WM_APP_SCREENSHOT_DONE: u32 = WM_APP + 11;

/// The analysis service posts this message after it pushes a result.
const WM_APP_ANALYSIS: u32 = WM_APP + 12;
/// The interval for the cursor poll, in milliseconds.
const DISPATCH_TICK_MS: u32 = 20;

/// The gap from the anchor to the popup, in physical pixels.
const POPUP_GAP: i32 = 40;

/// The interval for the rebuild progress poll, in milliseconds.
const REBUILD_TICK_MS: u32 = 100;

/// The maximum delay before Apply visibly stalls.
const APPLY_BUDGET_MS: u128 = 50;
/// Keep every add-time sentence result. Keep only the newest other result.
///
/// Retained results stay in arrival order. This lets a sentence complete an add
/// even when a newer hover result is in the same queue.
fn route_results(results: Vec<WorkerResult>) -> Vec<WorkerResult> {
    let mut routed = Vec::with_capacity(results.len());
    let mut freshest_other = None;
    for result in results {
        if matches!(&result.outcome, LookupOutcome::Sentence(_)) {
            routed.push(Some(result));
        } else {
            if let Some(index) = freshest_other {
                routed[index] = None;
            }
            freshest_other = Some(routed.len());
            routed.push(Some(result));
        }
    }
    routed.into_iter().flatten().collect()
}

/// Returns the feedback that completes a sentence request when the Worker is gone.
fn sentence_send_feedback(
    id: RequestId,
    sent: std::result::Result<(), mpsc::SendError<Trigger>>,
) -> Option<Event> {
    sent.err().map(|_| Event::LookupResult {
        id,
        outcome: LookupOutcome::Sentence(None),
    })
}

/// Builds the Events that the timer sends for one hook poll.
fn route_hook_events(
    press: Option<PhysPoint>,
    moved: Option<PhysPoint>,
) -> [Option<Event>; 2] {
    [
        press.map(|pos| Event::TriggerPressed { pos }),
        moved.map(|pos| Event::CursorMoved { pos }),
    ]
}

/// The result of one duplicate check.
struct AnkiDupeResult {
    gen: u64,
    checked: Vec<String>,
    /// `None` means that the connection failed.
    dupes: Option<HashSet<String>>,
}

/// Separates cached duplicate references from references that need a check.
fn partition_dupes(
    exprs: Vec<String>,
    cache: &HashMap<String, bool>,
) -> (HashSet<String>, Vec<String>, bool) {
    let mut seen = HashSet::new();
    let mut dupes = HashSet::new();
    let mut uncached = Vec::new();
    let mut cached_any = false;
    for expr in exprs {
        if !seen.insert(expr.clone()) {
            continue;
        }
        match cache.get(&expr) {
            Some(true) => {
                cached_any = true;
                dupes.insert(expr);
            }
            Some(false) => {
                cached_any = true;
            }
            None => uncached.push(expr),
        }
    }
    (dupes, uncached, cached_any)
}

/// The result of one add-note operation.
struct AddNoteResult {
    expr: String,
    err: Option<String>,
}

struct SettingsStatus {
    gen: Option<u64>,
    text: String,
}

impl SettingsStatus {
    fn any(text: String) -> Self {
        Self { gen: None, text }
    }

    fn anki(gen: u64, text: String) -> Self {
        Self {
            gen: Some(gen),
            text,
        }
    }

    fn matches(&self, current_gen: u64) -> bool {
        self.gen.is_none_or(|gen| gen == current_gen)
    }
}

/// Runs the settings window without a tray.
pub fn settings_only(
    cfg: Config,
    dicts: &[DictInfo],
    config_path: &Path,
    dict_path: &Path,
) -> Result<()> {
    let library = library_dir();
    let form = form_with_library(&cfg, dicts, &library);
    let stale = settings::stale_order_entries(&cfg, dicts);
    let window = SettingsWindow::open(&form, &stale, ApplyMode::Standalone)
        .context("opening the settings window")?;

    let mut rebuild: Option<InFlight> = None;
    let mut pending: Option<Config> = None;
    let mut tick = 0usize;
    let mut css_editor_so: Option<crate::ui::editor::CssEditor> = None;
    let (settings_tx, settings_rx) = mpsc::channel::<SettingsStatus>();
    let (detect_tx, detect_rx) = mpsc::channel::<AnkiDetect>();
    let mut detect_gen = 0u64;
    // SAFETY: This FFI call has no preconditions.
    let tid = unsafe { GetCurrentThreadId() };

    let mut msg = MSG::default();
    // SAFETY: `msg` is this loop's stack storage, and `window` stays alive for
    // the whole loop. It drops only after this function returns.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        // The settings window has no hooks, so there is nothing to disarm.
        window.pump(|| {});

        if matches!(msg.message, WM_KEYDOWN | WM_SYSKEYDOWN)
            && window.handle_capture_key(msg.wParam.0 as u16)
        {
            continue;
        }

        if msg.message == WM_APP_SETTINGS {
            while let Ok(status) = settings_rx.try_recv() {
                if status.matches(detect_gen) {
                    window.set_status(&status.text);
                }
            }
        }

        if msg.message == WM_APP_ANKI_DETECT {
            while let Ok(result) = detect_rx.try_recv() {
                apply_anki_detect(&window, detect_gen, result);
            }
        }

        // Handle dialog keys first, as `run` does.
        if !unsafe { IsDialogMessageW(window.hwnd(), &msg) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        service_settings_click(
            &window,
            &settings_tx,
            &detect_tx,
            &mut detect_gen,
            tid,
            &mut css_editor_so,
        );

        // A tab switch starts deck, model, and field detection.
        if let Some(tab) = window.take_tab_change() {
            window.switch_tab(tab);
            if tab == 3 {
                spawn_detect(
                    next_anki_detect_generation(&mut detect_gen),
                    window.anki_url(),
                    window.anki_model(),
                    detect_tx.clone(),
                    tid,
                );
            }
        }
        if window.take_field_map_toggle() {
            window.toggle_field_map();
        }
        if window.take_anki_model_change() {
            spawn_detect_fields(
                next_anki_detect_generation(&mut detect_gen),
                window.anki_url(),
                window.anki_model(),
                detect_tx.clone(),
                tid,
            );
        }

        if rebuild.is_some() {
            // Ignore window outcomes while the child writes.
            let _ = window.take_outcome();
            // Read the result only after the child finishes.
            let Some(built) = rebuild.as_ref().and_then(|f| pump_rebuild(&f.rx, &window)) else {
                continue;
            };
            let Some(flight) = rebuild.take() else {
                continue;
            };
            // SAFETY: `tick` is this loop's timer, which `SetTimer` sets below.
            unsafe {
                let _ = KillTimer(None, tick);
            }
            window.set_busy(false);
            match built {
                Ok(()) => {
                    keep_apply(&flight, &window);
                    let updated = pending.take().unwrap_or_else(|| cfg.clone());
                    updated
                        .save(config_path)
                        .with_context(|| format!("saving settings to {}", config_path.display()))?;
                    println!("chibipop: rebuilt {}.", dict_path.display());
                    println!("chibipop: settings saved to {}.", config_path.display());
                    // Start the popup process with the new Dictionary.
                    match start_run(config_path, dict_path) {
                        Ok(()) => println!("chibipop: starting."),
                        Err(e) => {
                            eprintln!("chibipop: could not start chibipop: {e:#}");
                            eprintln!("chibipop: the dictionary is ready - start it yourself.");
                        }
                    }
                    return Ok(());
                }
                Err(e) => {
                    undo_apply(&flight, &e);
                    report_failed_rebuild(&window, &e);
                }
            }
            continue;
        }

        match window.take_outcome() {
            // Without a tray, the window's X acts like Quit.
            Some(SettingsOutcome::Cancel) | Some(SettingsOutcome::Quit) => return Ok(()),
            Some(SettingsOutcome::Apply) => {
                let edited = window.read(&form);
                let updated = settings::apply_to(&edited, &cfg);
                // A font change does not need a rebuild.
                if !edited.has_staged() {
                    updated
                        .save(config_path)
                        .with_context(|| format!("saving settings to {}", config_path.display()))?;
                    println!("chibipop: settings saved to {}.", config_path.display());
                    println!("chibipop: restart chibipop for them to take effect.");
                    return Ok(());
                }
                match start_rebuild(&edited, &library, dict_path) {
                    Err(e) => refuse_apply(&window, &e),
                    Ok(flight) => {
                        begin_rebuild(&window);
                        // SAFETY: This thread timer is killed after every rebuild exit, as in
                        // `run`.
                        tick = unsafe { SetTimer(None, 0, REBUILD_TICK_MS, None) };
                        pending = Some(updated);
                        rebuild = Some(flight);
                    }
                }
            }
            None => {}
        }
    }
    Ok(())
}

/// Returns the folder that contains Dictionary archives.
pub(crate) fn library_dir() -> PathBuf {
    crate::paths::beside_exe("library")
}

/// Builds a `SettingsForm` with the current library entries.
pub(crate) fn form_with_library(cfg: &Config, dicts: &[DictInfo], dir: &Path) -> SettingsForm {
    let form = settings::from_config(cfg, dicts);
    match Library::load(dir) {
        Ok(lib) => settings::with_library(form, &lib),
        Err(e) => {
            eprintln!("chibipop: reading {} failed: {e:#}", dir.display());
            form
        }
    }
}

const STATUS_REBUILD_FAILED: &str = "The rebuild failed. Your dictionary is unchanged.";

/// Stores one staged change, its progress receiver, and its library lock.
struct InFlight {
    pending: Pending,
    rx: mpsc::Receiver<Progress>,
    _lock: LibraryLock,
}

/// Acquires the library lock, stages the change, and starts the rebuild.
fn start_rebuild(form: &SettingsForm, dir: &Path, out: &Path) -> Result<InFlight> {
    let lock = LibraryLock::acquire(dir)?;
    let (pending, rx) = stage_and_spawn(form, dir, out)?;
    Ok(InFlight {
        pending,
        rx,
        _lock: lock,
    })
}

/// Stages the change, then starts the rebuild.
fn stage_and_spawn(
    form: &SettingsForm,
    dir: &Path,
    out: &Path,
) -> Result<(Pending, mpsc::Receiver<Progress>)> {
    let pending = settings::stage_into_library(form, dir)?;
    match rebuild::spawn(dir, out) {
        Ok(rx) => Ok((pending, rx)),
        Err(e) => {
            undo_apply_pending(&pending, &e);
            Err(e)
        }
    }
}

/// Restores every archive after a failed Apply.
fn undo_apply(flight: &InFlight, why: &anyhow::Error) {
    undo_apply_pending(&flight.pending, why);
}

/// Restores every archive after a failed Apply.
fn undo_apply_pending(pending: &Pending, why: &anyhow::Error) {
    match pending.rollback() {
        Ok(()) => eprintln!("chibipop: {why:#} - your dictionary archives were put back."),
        Err(e) => {
            eprintln!("chibipop: {why:#}");
            eprintln!("chibipop: putting your archives back failed: {e:#}");
            eprintln!("chibipop: they are in the library's .removed folder.");
        }
    }
}

/// Commits archive removals after a successful rebuild.
fn keep_apply(flight: &InFlight, w: &SettingsWindow) {
    if let Err(e) = flight.pending.commit() {
        eprintln!("chibipop: clearing the library's .removed folder failed: {e:#}");
    }
    w.clear_staged();
}

/// Reads rebuild progress with a nonblocking call.
///
/// Returns `None` while the rebuild is active.
fn pump_rebuild(rx: &mpsc::Receiver<Progress>, w: &SettingsWindow) -> Option<Result<()>> {
    loop {
        match rx.try_recv() {
            Ok(Progress::Line(line)) => {
                println!("chibipop: {line}");
                // Do not print the raw .tmp line.
                if let Some(text) = crate::dict::progress::friendly(&line) {
                    w.set_status(&text);
                }
            }
            Ok(Progress::Done(_)) => return Some(Ok(())),
            Ok(Progress::Failed(why)) => return Some(Err(anyhow::anyhow!(why))),
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => {
                return Some(Err(anyhow::anyhow!("the rebuild ended without reporting")));
            }
        }
    }
}

/// Starts a rebuild and reports its status.
fn begin_rebuild(w: &SettingsWindow) {
    w.set_busy(true);
    w.set_status("Rebuilding your dictionary. This can take a few minutes.");
}

/// Marks the settings window busy while files copy.
fn begin_apply(w: &SettingsWindow) {
    w.set_busy(true);
    w.set_status("Applying your changes\u{2026}");
}

/// Reports why Apply did not run.
fn refuse_apply(w: &SettingsWindow, e: &anyhow::Error) {
    w.set_status(&format!("Not applied: {e}"));
    eprintln!("chibipop: not applied: {e:#}");
}

/// Reports the active OCR engine.
fn engine_status_line(cfg: &Config) -> String {
    match resolve_engine(&cfg.ocr.engine, &cfg.plugins.enabled) {
        EngineChoice::Builtin => "Engine: Built-in (Windows OCR)".to_string(),
        EngineChoice::Plugin(name) => format!("Engine: {name}"),
        EngineChoice::FellBack(name) => {
            format!("Engine: {name} (not found — using Built-in)")
        }
    }
}

/// Returns recent plugin stderr lines.
fn adapter_status_line(cfg: &Config) -> String {
    if !matches!(
        resolve_engine(&cfg.ocr.engine, &cfg.plugins.enabled),
        EngineChoice::Plugin(_)
    ) {
        return "Adapter log: no plugin engine active".to_string();
    }
    let log = host::engine_log_lines();
    let start = log.len().saturating_sub(5);
    let tail = &log[start..];
    if tail.is_empty() {
        "Adapter log: (no output yet)".to_string()
    } else {
        format!("Adapter log:\r\n{}", tail.join("\r\n"))
    }
}

/// Reports when the library and database drift.
fn notice_drift(w: &SettingsWindow, dir: &Path, db: &Path) {
    match drifted(dir, db) {
        Err(e) => eprintln!("chibipop: checking for drift failed: {e:#}"),
        Ok(None) => {}
        Ok(Some(text)) => w.set_status(&text),
    }
}

/// Builds a drift notice when the library and database differ.
fn drifted(dir: &Path, db: &Path) -> Result<Option<String>> {
    let sources = read_source_hashes(db)?;
    let lib = Library::load(dir).with_context(|| format!("reading {}", dir.display()))?;
    Ok(settings::drift_notice(sources.as_deref(), &lib, dir, db))
}

/// Reads the source hashes that the database records, when present.
fn read_source_hashes(db: &Path) -> Result<Option<String>> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {} to read its source list", db.display()))?;
    conn.query_row("SELECT v FROM meta WHERE k = 'source_hashes'", [], |r| {
        r.get(0)
    })
    .optional()
    .with_context(|| format!("reading source_hashes from {}", db.display()))
}

/// Reports that the rebuild failed and left the Dictionary unchanged.
fn report_failed_rebuild(w: &SettingsWindow, e: &anyhow::Error) {
    w.set_status(STATUS_REBUILD_FAILED);
    eprintln!("chibipop: the rebuild failed: {e:#}");
    eprintln!("chibipop: the dictionary in use was not touched.");
}

/// Stores the name, Dictionary row, archive file, and roles for one removal.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Removal {
    name: String,
    dict_id: Option<i64>,
    file: Option<String>,
    /// The roles that the library entry supplies. `None` means that no entry
    /// has this name, so no archive remains to delete.
    roles: Option<Roles>,
}

/// Describes every edit that one Apply must do.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EditPlan {
    removals: Vec<Removal>,
    additions: Vec<crate::settings::StagedAdd>,
}

/// Reports every change from one Apply.
#[derive(Debug, Default)]
struct EditReport {
    /// Each added name keeps its Roles because configuration registration needs both values.
    /// One archive can enter each Dictionary list that matches its Roles.
    added: Vec<(String, Roles)>,
    removed: Vec<String>,
    freq_added: Vec<String>,
    freq_removed: Vec<String>,
    failed: Vec<String>,
    dicts: Vec<DictInfo>,
}

/// Carries progress or the completed edit report.
enum EditMsg {
    Status(String),
    Done(Result<Box<EditReport>>),
}

/// Stores an in-place edit and its library lock.
struct EditFlight {
    rx: mpsc::Receiver<EditMsg>,
    _lock: LibraryLock,
}

/// Opens a read-write connection.
/// The database must use WAL journal mode.
fn open_writer(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .with_context(|| format!("opening {} for writing", path.display()))?;
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .with_context(|| format!("reading the journal mode of {}", path.display()))?;
    if !mode.eq_ignore_ascii_case("wal") {
        anyhow::bail!(
            "{} is in {mode} journal mode, not WAL - changing dictionaries in place needs \
             WAL. Rebuild the dictionary to convert it.",
            path.display()
        );
    }
    Ok(conn)
}

/// Recomputes every Frequency rank from claims already stored in the database.
///
/// This is the settings path for frequency changes. It applies the enabled
/// frequency Dictionaries, their order, and the selected strategy to stored claims.
/// It does not open an archive. `reapply_frequencies` handles changes to the library.
///
/// Returns the number of `term` rows that it updates. This function reports progress
/// directly to the window because the pump thread runs the settings-only Apply.
fn reindex_ranks(
    db: &Path,
    cfg: &Config,
    dicts: &[DictInfo],
    w: &SettingsWindow,
) -> Result<u64> {
    let mut conn = open_writer(db)?;
    let enabled = cfg.dictionaries.enabled(Role::Frequency, dicts);
    w.set_status("Updating frequency rankings\u{2026}");
    crate::dict::reindex::reindex(
        &mut conn,
        &enabled,
        cfg.dictionaries.ranking_strategy,
        &|text| w.set_status(text),
    )
}

/// Lists the rows and files that Apply changes.
fn plan_edits(form: &SettingsForm, dicts: &[DictInfo], lib: &Library) -> EditPlan {
    let removals = form
        .staged_removes
        .iter()
        .map(|name| {
            let entry = lib
                .entries
                .iter()
                .find(|e| &e.name == name || &e.file == name)
                .cloned();
            let roles = entry.as_ref().map(|e| e.roles);
            Removal {
                name: name.clone(),
                // Each archive supplies one `dict` row. A frequency archive therefore has
                // one row to remove, even when its name has no library entry.
                // (ARCHITECTURE.md#dictionary-and-lookup).
                dict_id: dicts.iter().find(|d| &d.name == name).map(|d| d.dict_id),
                file: entry.as_ref().map(|e| e.file.clone()),
                roles,
            }
        })
        .collect();
    EditPlan {
        removals,
        additions: form.staged_adds.clone(),
    }
}

/// Converts progress counts for one Dictionary.
///
/// Builder IDs are absolute.
fn rebased(line: &str, base: i64) -> String {
    let Some(rest) = line.strip_prefix("progress") else {
        return line.to_string();
    };
    let Some((n, total)) = rest.trim().split_once('/') else {
        return line.to_string();
    };
    let Ok(n) = n.trim().parse::<i64>() else {
        return line.to_string();
    };
    format!("progress  {} / {}", (n - base + 1).max(1), total.trim())
}

/// Summarizes the result of one edit.
fn edit_status(report: &EditReport) -> String {
    let mut parts = Vec::new();
    if !report.added.is_empty() {
        let names: Vec<&str> = report.added.iter().map(|(name, _)| name.as_str()).collect();
        parts.push(format!("Added {}.", names.join(", ")));
    }
    if !report.freq_added.is_empty() {
        parts.push(format!("Added frequency {}.", report.freq_added.join(", ")));
    }
    if !report.removed.is_empty() {
        parts.push(format!("Removed {}.", report.removed.join(", ")));
    }
    if !report.freq_removed.is_empty() {
        parts.push(format!(
            "Removed frequency {}.",
            report.freq_removed.join(", ")
        ));
    }
    if !report.failed.is_empty() {
        parts.push(format!("Not applied: {}.", report.failed.join("; ")));
    }
    if parts.is_empty() {
        return "No dictionary changed.".to_string();
    }
    parts.join(" ")
}

/// Returns true when an archive reports only Reported frequencies.
///
/// A Dictionary with the Terms role or the Pitch role also owns a `dict` row for those records.
/// A frequency-only archive stores its claims separately. The frequency pass
/// uses those claims and does not add term or Entry rows
/// (ARCHITECTURE.md#dictionary-and-lookup).
///
/// Roles replaced the old single-valued kind. This check therefore tests the
/// full Role set. An archive with the Terms role and frequency data is a
/// Dictionary that also reports frequencies.
fn frequency_only(roles: Roles) -> bool {
    roles == Roles::only(&[Role::Frequency])
}

/// Applies edits to the live database.
///
/// Refuses before it moves any archive.
fn apply_edits(
    db: &Path,
    dir: &Path,
    form: &SettingsForm,
    tx: &mpsc::Sender<EditMsg>,
) -> Result<Box<EditReport>> {
    let mut conn = open_writer(db)?;
    let reader = SqliteDictionary::open(db)?;
    let mut lib = Library::load(dir).with_context(|| format!("reading {}", dir.display()))?;
    if settings::terms_after_apply(form, &lib) == 0 {
        anyhow::bail!("that would leave chibipop with no dictionary");
    }
    // Read identities from the file, not from a cache.
    let live = reader.dicts().context("reading dictionary identities")?;
    let plan = plan_edits(form, &live, &lib);

    let say = |text: String| {
        let _ = tx.send(EditMsg::Status(text));
    };
    let mut pending = Pending::new(dir, &lib);
    let mut report = EditReport::default();

    for removal in &plan.removals {
        say(format!("Removing {}\u{2026}", removal.name));
        match remove_one(&mut conn, &mut lib, dir, &mut pending, removal) {
            Ok(()) if removal.roles.is_some_and(frequency_only) => {
                report.freq_removed.push(removal.name.clone())
            }
            Ok(()) => report.removed.push(removal.name.clone()),
            Err(e) => report.failed.push(format!("{}: {e:#}", removal.name)),
        }
    }

    let freqs = lib.freq_paths(dir);
    for add in &plan.additions {
        say(format!("Reading {}\u{2026}", add.name));
        match add_one(&mut conn, &mut lib, dir, &freqs, add, tx) {
            Ok((name, roles)) if roles.is_empty() => {
                report.failed.push(format!("{}: {name} is unreadable", add.name))
            }
            Ok((name, roles)) if frequency_only(roles) => report.freq_added.push(name),
            Ok(added) => report.added.push(added),
            Err(e) => report.failed.push(format!("{}: {e:#}", add.name)),
        }
    }

    lib.save(dir)
        .with_context(|| format!("saving {}", dir.display()))?;
    pending.commit()?;
    report.dicts = reader.dicts().context("re-reading dictionary identities")?;
    Ok(Box::new(report))
}

/// Reapplies Frequency ranks after archive edits.
fn apply_edits_with_frequencies(
    db: &Path,
    dir: &Path,
    form: &SettingsForm,
    tx: &mpsc::Sender<EditMsg>,
) -> Result<Box<EditReport>> {
    if form.freq_changed {
        validate_frequency_inputs(dir, form)?;
    }
    let report = apply_edits(db, dir, form, tx)?;
    if !form.freq_changed {
        return Ok(report);
    }

    let mut conn = open_writer(db)?;
    let lib = Library::load(dir).with_context(|| format!("reading {}", dir.display()))?;
    let freqs = lib.freq_paths(dir);
    let _ = tx.send(EditMsg::Status(
        "Updating frequency rankings...".to_string(),
    ));
    crate::dict::edit::reapply_frequencies(&mut conn, &freqs, &|text| {
        let _ = tx.send(EditMsg::Status(text.to_string()));
    })?;
    Ok(report)
}

/// Validates frequency archives before an Apply changes them.
fn validate_frequency_inputs(dir: &Path, form: &SettingsForm) -> Result<()> {
    let lib = Library::load(dir).with_context(|| format!("reading {}", dir.display()))?;
    let removed = settings::removed_files(form, &lib);
    let mut freqs: Vec<PathBuf> = lib
        .freq_paths(dir)
        .into_iter()
        .filter(|path| {
            let Some(file) = path.file_name().and_then(|name| name.to_str()) else {
                return true;
            };
            !removed.iter().any(|removed| removed == file)
        })
        .collect();
    freqs.extend(
        form.staged_adds
            .iter()
            .filter(|add| crate::library::roles_of(&add.source).has(Role::Frequency))
            .map(|add| add.source.clone()),
    );
    crate::dict::build::load_freqs(&freqs).map(|_| ())
}

/// Removes one Dictionary from the database and the library.
fn remove_one(
    conn: &mut Connection,
    lib: &mut Library,
    dir: &Path,
    pending: &mut Pending,
    removal: &Removal,
) -> Result<()> {
    if let Some(dict_id) = removal.dict_id {
        let archive = removal
            .file
            .as_ref()
            .map(|f| dir.join(f))
            .unwrap_or_default();
        let done = crate::dict::edit::remove_dictionary(conn, dict_id, &archive)?;
        if done.dicts == 0 {
            anyhow::bail!("dictionary {dict_id} was no longer in the database");
        }
    }
    if let Some(file) = &removal.file {
        if removal.roles.is_some_and(frequency_only) {
            crate::dict::edit::forget_source(conn, &dir.join(file))?;
        }
        lib.quarantine(dir, file)
            .with_context(|| format!("removing {file}"))?;
        pending.held(file.clone());
    }
    Ok(())
}

/// Imports one archive into the database and library.
fn add_one(
    conn: &mut Connection,
    lib: &mut Library,
    dir: &Path,
    freqs: &[PathBuf],
    add: &crate::settings::StagedAdd,
    tx: &mpsc::Sender<EditMsg>,
) -> Result<(String, Roles)> {
    let entry = lib
        .import(dir, &add.source)
        .with_context(|| format!("importing {}", add.source.display()))?;
    let path = dir.join(&entry.file);
    if frequency_only(entry.roles) {
        return match crate::dict::edit::record_source(conn, &path) {
            Ok(()) => Ok((entry.name, entry.roles)),
            Err(e) => {
                lib.entries.retain(|x| x.file != entry.file);
                let _ = std::fs::remove_file(&path);
                Err(e)
            }
        };
    }
    let base = crate::dict::edit::next_entry_id(conn)?;
    let on_progress = |line: &str| {
        if let Some(text) = crate::dict::progress::friendly(&rebased(line, base)) {
            let _ = tx.send(EditMsg::Status(text));
        }
    };
    match crate::dict::edit::add_dictionary(conn, &path, freqs, &on_progress) {
        Ok(done) => Ok((done.name, entry.roles)),
        Err(e) => {
            lib.entries.retain(|x| x.file != entry.file);
            let _ = std::fs::remove_file(&path);
            Err(e)
        }
    }
}

/// Reads edit progress with a nonblocking call.
///
/// Returns `None` while the edit continues.
/// This function never writes progress to stdout.
fn pump_edit(rx: &mpsc::Receiver<EditMsg>, w: &SettingsWindow) -> Option<Result<Box<EditReport>>> {
    loop {
        match rx.try_recv() {
            Ok(EditMsg::Status(text)) => w.set_status(&text),
            Ok(EditMsg::Done(done)) => return Some(done),
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => {
                return Some(Err(anyhow!(
                    "the dictionary change ended without reporting"
                )));
            }
        }
    }
}

struct AnkiDetect {
    gen: u64,
    url: String,
    model: String,
    decks: Option<Vec<String>>,
    models: Option<Vec<String>>,
    fields: Vec<String>,
}

impl AnkiDetect {
    fn full(
        gen: u64,
        url: String,
        model: String,
        decks: Vec<String>,
        models: Vec<String>,
        fields: Vec<String>,
    ) -> Self {
        Self {
            gen,
            url,
            model,
            decks: Some(decks),
            models: Some(models),
            fields,
        }
    }

    fn fields(gen: u64, url: String, model: String, fields: Vec<String>) -> Self {
        Self {
            gen,
            url,
            model,
            decks: None,
            models: None,
            fields,
        }
    }
}

fn next_anki_detect_generation(gen: &mut u64) -> u64 {
    *gen = gen.checked_add(1).unwrap_or(1);
    *gen
}

fn anki_detect_matches(gen: u64, url: &str, model: &str, result: &AnkiDetect) -> bool {
    result.gen == gen && result.url == url && result.model == model
}

fn reachable_message(model: &str, fields: &[String]) -> String {
    if fields.is_empty() {
        return "AnkiConnect is reachable.".into();
    }
    format!(
        "AnkiConnect is reachable. \"{model}\" fields: {}",
        fields.join(", "),
    )
}

fn detect_all(gen: u64, url: String, model: String) -> AnkiDetect {
    let deck_url = url.clone();
    let model_url = url.clone();
    let field_url = url.clone();
    let requested_model = model.clone();
    let deck_names = thread::spawn(move || anki::deck_names(&deck_url).unwrap_or_default());
    let model_names = thread::spawn(move || anki::model_names(&model_url).unwrap_or_default());
    let field_names =
        thread::spawn(move || anki::model_field_names(&field_url, &model).unwrap_or_default());
    AnkiDetect::full(
        gen,
        url,
        requested_model,
        deck_names.join().unwrap_or_default(),
        model_names.join().unwrap_or_default(),
        field_names.join().unwrap_or_default(),
    )
}

fn apply_anki_detect(w: &SettingsWindow, gen: u64, result: AnkiDetect) {
    if !anki_detect_matches(gen, &w.anki_url(), &w.anki_model(), &result) {
        return;
    }
    match (result.decks, result.models) {
        (Some(decks), Some(models)) => w.populate_combos(&decks, &models, result.fields),
        _ => w.populate_fields(result.fields),
    }
}

/// Reads deck, model, and field names on a background thread.
fn spawn_detect(gen: u64, url: String, model: String, tx: mpsc::Sender<AnkiDetect>, tid: u32) {
    thread::spawn(move || {
        let _ = tx.send(detect_all(gen, url, model));
        // SAFETY: This call wakes the main loop.
        unsafe {
            let _ = PostThreadMessageW(tid, WM_APP_ANKI_DETECT, WPARAM(0), LPARAM(0));
        }
    });
}

fn spawn_detect_fields(
    gen: u64,
    url: String,
    model: String,
    tx: mpsc::Sender<AnkiDetect>,
    tid: u32,
) {
    thread::spawn(move || {
        let fields = anki::model_field_names(&url, &model).unwrap_or_default();
        let _ = tx.send(AnkiDetect::fields(gen, url, model, fields));
        // SAFETY: This call wakes the main loop.
        unsafe {
            let _ = PostThreadMessageW(tid, WM_APP_ANKI_DETECT, WPARAM(0), LPARAM(0));
        }
    });
}

/// Processes one settings click for Anki, update, or the CSS editor.
///
fn service_settings_click(
    w: &SettingsWindow,
    tx: &mpsc::Sender<SettingsStatus>,
    detect_tx: &mpsc::Sender<AnkiDetect>,
    detect_gen: &mut u64,
    tid: u32,
    css_editor: &mut Option<crate::ui::editor::CssEditor>,
) {
    match w.take_click() {
        Some(SettingsClick::AnkiTest) => {
            w.set_status("Testing\u{2026}");
            let url = w.anki_url();
            let model = w.anki_model();
            let gen = next_anki_detect_generation(detect_gen);
            let tx = tx.clone();
            let detect_tx = detect_tx.clone();
            thread::spawn(move || {
                let status = anki::check_connection(&url);
                let detect =
                    matches!(status, Ok(true)).then(|| detect_all(gen, url.clone(), model.clone()));
                let msg = match &status {
                    Ok(true) => detect.as_ref().map_or_else(
                        || "AnkiConnect is reachable.".into(),
                        |result| reachable_message(&model, &result.fields),
                    ),
                    Ok(false) => "AnkiConnect did not respond.".into(),
                    Err(e) => format!("Anki test failed: {e:#}"),
                };
                let _ = tx.send(SettingsStatus::anki(gen, msg));
                if let Some(result) = detect {
                    let _ = detect_tx.send(result);
                    // SAFETY: This call wakes the main loop.
                    unsafe {
                        let _ = PostThreadMessageW(tid, WM_APP_ANKI_DETECT, WPARAM(0), LPARAM(0));
                    }
                }
                // SAFETY: This call wakes the main loop.
                unsafe {
                    let _ = PostThreadMessageW(tid, WM_APP_SETTINGS, WPARAM(0), LPARAM(0));
                }
            });
        }
        Some(SettingsClick::CheckUpdate) => {
            w.set_status("Checking\u{2026}");
            let tx = tx.clone();
            thread::spawn(move || {
                let msg = match update::check(env!("CARGO_PKG_VERSION")) {
                    Ok(None) => "You already have the latest version.".into(),
                    Ok(Some(release)) => match update::download_and_replace(&release) {
                        Ok(()) => format!("Updated to {}. Restart to use it.", release.tag,),
                        Err(e) => format!("Update to {} failed: {e:#}", release.tag,),
                    },
                    Err(e) => format!("Update check failed: {e:#}"),
                };
                let _ = tx.send(SettingsStatus::any(msg));
                // SAFETY: This call wakes the main loop.
                unsafe {
                    let _ = PostThreadMessageW(tid, WM_APP_SETTINGS, WPARAM(0), LPARAM(0));
                }
            });
        }
        Some(SettingsClick::CssEditor) => {
            let css_path = crate::paths::beside_exe("popup.css");
            let theme_name = w.read_theme_name();
            let font = w.read_font_name();
            match crate::ui::editor::CssEditor::open(&css_path, &theme_name, &font) {
                Ok(ed) => *css_editor = Some(ed),
                Err(e) => eprintln!("chibipop: CSS editor: {e:#}"),
            }
        }
        None => {}
    }
}
/// Builds the Windows `WorkerParts` on the Worker thread.
/// Capture and OCR backends are thread-affine. COM and the per-thread DXGI
/// cache needs that thread, so the closure creates them when `Worker` starts.
fn worker_open(
    dict_path: PathBuf,
    rules_path: PathBuf,
    language: String,
    guard: CaptureGuard,
    // The value is "builtin" or a plugin name.
    ocr_engine: String,
    // The plugins that this Worker can run.
    enabled_plugins: Vec<String>,
    // One-off OCR jobs (OCR-to-clipboard), handled between lookups.
    ocr_request_rx: mpsc::Receiver<crate::action::OcrRequest>,
) -> impl FnOnce() -> Result<WorkerParts> + Send + 'static {
    move || {
        // Resolve the engine once. Do not save this choice.
        let ocr: Box<dyn chibipop::text::OcrEngine> =
            match resolve_plugin_engine(&ocr_engine, &enabled_plugins) {
                Some(plugin) => plugin,
                None => {
                    let fallback = crate::config::default_ocr_language();
                    let substitute =
                        startup_language(&language, &fallback, || recogniser_available(&language));
                    let language = match substitute {
                        Some(sub) => {
                            eprintln!(
                            "chibipop: no {language} OCR recogniser installed; starting with {sub}"
                        );
                            sub
                        }
                        None => language,
                    };
                    Box::new(WinrtOcr::new(&language).context("creating the OCR text source")?)
                }
            };
        // Contract 3 needs DPI before GDI.
        let capture = WinCapture::new(Some(guard)).context("preparing screen capture")?;
        let dict = SqliteDictionary::open(&dict_path).with_context(|| {
            format!(
                "opening {} - add dictionaries in the settings window",
                dict_path.display()
            )
        })?;
        let rules = load_rules(&rules_path)?;
        let engine = LookupEngine::new(Deconjugator::new(rules));
        Ok(WorkerParts {
            capture: Box::new(capture),
            ocr,
            dict: Box::new(dict),
            // A finished rebuild restarts this process through `start_run` on
            // `Progress::Done`. The Worker never outlives the database it opened,
            // so a reload does not reopen the database.
            reopen_dict: None,
            engine,
            // The OCR-to-clipboard request runs on this thread because the engine is
            // thread-affine. The closure uses the core facade because the OCR request
            // loop belongs to the core seam.
            serve: Some(Box::new(move |source| {
                while let Ok(request) = ocr_request_rx.try_recv() {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        source
                            .recognise(&request.bgra_buf, request.width, request.height)
                            .map_err(|e| format!("{e:#}"))
                    }))
                    .unwrap_or_else(|_| Err("OCR worker panicked".to_string()));
                    let _ = request.result_tx.send(result);
                }
            })),
        })
    }
}

/// Runs the Windows message loop until the user quits.
pub fn run(mut cfg: Config, dict_path: &Path, rules_path: &Path, config_path: &Path) -> Result<()> {
    let plugins_root = crate::paths::beside_exe("plugins");
    let found = crate::plugin::discover::discover(&plugins_root);
    for name in crate::plugin::discover::text_provider_names(&found) {
        if !cfg.plugins.enabled.contains(&name) {
            cfg.plugins.enabled.push(name);
        }
    }
    // The Dictionary or rules file does not exist yet.
    if !dict_path.exists() || !rules_path.exists() {
        return settings_only(cfg, &[], config_path, dict_path);
    }

    let library = library_dir();
    let db_path = dict_path.to_path_buf();
    let rules_path = rules_path.to_path_buf();
    // One-off OCR pixels for the Worker engine (OCR-to-clipboard).
    // Lookup requests use the core Worker's channels.
    let (ocr_tx, ocr_request_rx) = mpsc::channel::<crate::action::OcrRequest>();
    // This state is unknown until `Popup::create`.
    let capture_guard_active = Arc::new(AtomicBool::new(false));
    let (capture_guard_tx, capture_guard_rx) = mpsc::channel::<CaptureGuardMsg>();

    // SAFETY: This FFI call has no preconditions and always succeeds.
    // It returns the ID of the thread that calls it.
    let main_tid = unsafe { GetCurrentThreadId() };
    let mut live = derive(&cfg);
    // Do not join the Worker. `join` hangs.
    let (worker, mut dicts) = Worker::spawn(
        // `Worker::spawn` reads the file itself.
        worker_settings(&live, &[]),
        worker_open(
            db_path.clone(),
            rules_path.clone(),
            live.language.clone(),
            CaptureGuard {
                active: Arc::clone(&capture_guard_active),
                main_tid,
                request_tx: capture_guard_tx.clone(),
            },
            cfg.ocr.engine.clone(),
            cfg.plugins.enabled.clone(),
            ocr_request_rx,
        ),
        // The Worker posts a message after it pushes a result.
        move || unsafe {
            let _ = PostThreadMessageW(main_tid, WM_APP_RESULT, WPARAM(0), LPARAM(0));
        },
    )?;
    // The analysis service loads the bundled model only when the first Card needs it.
    let analysis_service = chibipop::analysis::Service::spawn(
        crate::paths::data_file(chibipop::analysis::MODEL_FILE),
        move || unsafe {
            let _ = PostThreadMessageW(main_tid, WM_APP_ANALYSIS, WPARAM(0), LPARAM(0));
        },
    );
    // Queue the pixels and wake the Worker in one operation.
    // The Worker waits for jobs instead of repeated checks.
    let ocr_jobs = crate::action::OcrJobs::new(ocr_tx, worker.serve_nudge());

    let popup = Popup::create(live.exclude_from_capture).context("creating the popup window")?;

    // Contract 2 needs a report for all three states.
    match popup.capture_exclusion() {
        CaptureExclusion::Excluded => {
            println!(
                "chibipop: capture exclusion active - the popup will not appear in its own OCR captures"
            );
        }
        CaptureExclusion::DeliberatelyNotExcluded => {
            println!(
                "chibipop: capture exclusion disabled (exclude_from_capture = false in the config)"
            );
            println!("chibipop: the popup IS recordable now - each capture briefly hides and reshows it,");
            println!("chibipop: so hovering keeps resolving the real text underneath, not its own");
        }
        CaptureExclusion::AttemptFailed => {
            eprintln!("chibipop: ============================================================");
            eprintln!("chibipop: WARNING: capture exclusion is NOT active for the popup window.");
            eprintln!(
                "chibipop: SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE) was not accepted,"
            );
            eprintln!("chibipop: even though exclude_from_capture = true. This was NOT requested.");
            eprintln!("chibipop: The capture guard below will still hide/reshow the popup around");
            eprintln!("chibipop: every capture, so lookups stay correct, at the cost of a flicker");
            eprintln!(
                "chibipop: this build did not expect to pay. Investigate why the OS refused."
            );
            eprintln!("chibipop: ============================================================");
        }
    }

    // A scan overlay failure is never fatal.
    //
    // When it exists, the overlay remains live and appears on demand.
    let overlay = match Overlay::create(live.exclude_from_capture) {
        Ok(o) => Some(o),
        Err(e) => {
            eprintln!(
                "chibipop: the scan overlay could not be created, continuing without it: {e:#}"
            );
            None
        }
    };
    let overlay_hwnd = overlay.as_ref().map(Overlay::hwnd);

    // The overlay result can differ from the popup result.
    if let Some(CaptureExclusion::AttemptFailed) = overlay.as_ref().map(Overlay::capture_exclusion)
    {
        eprintln!("chibipop: ============================================================");
        eprintln!(
            "chibipop: WARNING: capture exclusion is NOT active for the scan overlay window."
        );
        eprintln!("chibipop: SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE) was not accepted,");
        eprintln!("chibipop: even though exclude_from_capture = true. This was NOT requested.");
        eprintln!("chibipop: The capture guard below will still hide/reshow the overlay around");
        eprintln!("chibipop: every capture, so its outlines never land inside one, at the cost");
        eprintln!("chibipop: of a flicker this build did not expect to pay. Investigate why the OS refused.");
        eprintln!("chibipop: ============================================================");
    }

    // Create the static region outline.
    let static_overlay = match StaticRegionOverlay::create(live.exclude_from_capture) {
        Ok(o) => Some(o),
        Err(e) => {
            eprintln!("chibipop: the static region overlay could not be created: {e:#}");
            None
        }
    };
    if let (Some(ov), Some(region)) = (&static_overlay, live.static_overlay_region()) {
        if let Err(e) = ov.show(region) {
            eprintln!("chibipop: showing static region overlay failed: {e:#}");
        }
    }

    // An Anki button failure is never fatal.
    //
    // When it exists, the button remains live and appears on demand.
    let anki_button = match AnkiButton::create(live.exclude_from_capture) {
        Ok(b) => Some(b),
        Err(e) => {
            eprintln!(
                "chibipop: the Anki button could not be created, continuing without it: {e:#}"
            );
            None
        }
    };

    if let Some(CaptureExclusion::AttemptFailed) =
        anki_button.as_ref().map(AnkiButton::capture_exclusion)
    {
        eprintln!(
            "chibipop: capture exclusion is NOT active for the Anki button window - the \
             capture guard will hide it during captures instead"
        );
    }

    // `apply_live` recomputes this state.
    capture_guard_active.store(
        capture_guard_needed(
            popup.capture_exclusion(),
            overlay.as_ref().map(Overlay::capture_exclusion),
            anki_button.as_ref().map(AnkiButton::capture_exclusion),
        ),
        Ordering::SeqCst,
    );

    // The renderer opens its own read-only connection to the media store.
    // The Worker owns the Dictionary connection on another thread.
    let mut renderer = Renderer::new(popup.hwnd(), &db_path)
        .context("creating the D2D/DirectWrite renderer")?;
    let mut theme = theme_from_config(&live.popup);
    let alpha = (theme.opacity * 255.0).round().clamp(0.0, 255.0) as u8;
    popup.set_alpha(alpha);
    if live.show_lookup_log {
        crate::ui::console::show();
    }

    let mut hooks = Some(Hooks::install().context("installing the low-level input hooks")?);
    Hooks::set_mode(live.trigger_mode);
    if let Some(vk) = crate::config::parse_trigger_key(&live.trigger_key) {
        Hooks::set_trigger_key(vk);
    }
    if let Some(vk) = crate::config::parse_trigger_key(&live.anki_add_key) {
        Hooks::set_add_hotkey(vk);
    }
    if let Some((vk, mods)) = crate::config::parse_hotkey(&live.actions_screenshot_hotkey) {
        Hooks::set_action_hotkey(0, vk, mods);
    }
    match crate::config::parse_trigger_key(&live.static_region_key) {
        Some(vk) => Hooks::set_action_hotkey(1, vk, 0),
        None => Hooks::set_action_hotkey(1, 0, 0),
    }
    if let Some(vk) = live
        .actions_ocr_clipboard_hotkey
        .as_deref()
        .and_then(crate::config::parse_trigger_key)
    {
        Hooks::set_action_hotkey(2, vk, 0);
    }

    // The tray provides the control surface for this process.
    let tray = Tray::create(popup.hwnd()).context("creating the tray icon")?;

    // Use a thread timer without a window.
    let timer_id = unsafe { SetTimer(None, 0, DISPATCH_TICK_MS, None) };
    if timer_id == 0 {
        anyhow::bail!("SetTimer failed to install the dispatch tick");
    }

    println!("chibipop: running - hover Japanese text anywhere on screen.");
    println!("chibipop: right-click the tray icon to change mode or quit.");

    // The Worker started before the Dictionary identities were known.
    rescope_lookups(&mut live, &cfg, &dicts, worker.trigger());
    // Record visibility immediately before `Hide`.
    //
    // Other hide paths clear these values.
    let capture_guard_prev_visible = std::cell::Cell::new(false);
    // Visibility of the overlay itself.
    let overlay_prev_visible = std::cell::Cell::new(false);
    // Visibility of the Anki button.
    let btn_prev_visible = std::cell::Cell::new(false);
    // Send each Event through the state machine and handle each Command.
    let mut controller = Controller::new(controller_config(&live));
    // Defer OpenSettings to the message loop.
    let mut want_settings = false;
    // An authorized add that waits for a region.
    // See `PendingShot`.
    let mut pending_shot: Option<PendingShot> = None;
    // Track each key edge.
    let mut trigger_was_held = false;
    // Track popup pointer edges and the latest drag point.
    let mut pointer_buttons = 0u8;
    let mut last_pointer: Option<PhysPoint> = None;
    let mut last_pointer_text = None;
    // Visibility of the static overlay.
    let sr_prev_visible = std::cell::Cell::new(false);
    let sr_hwnd = static_overlay.as_ref().map(StaticRegionOverlay::hwnd);
    let (anki_tx, anki_rx) = mpsc::channel::<AnkiDupeResult>();
    // Cache Anki duplicate answers by `expr`.
    let mut dupe_cache: HashMap<String, bool> = HashMap::new();
    let (add_tx, add_rx) = mpsc::channel::<AddNoteResult>();
    let (settings_tx, settings_rx) = mpsc::channel::<SettingsStatus>();
    let (detect_tx, detect_rx) = mpsc::channel::<AnkiDetect>();
    let mut detect_gen = 0u64;
    let (save_tx, save_rx) = mpsc::channel::<Result<()>>();
    let mut css_editor: Option<crate::ui::editor::CssEditor> = None;
    let (screenshot_tx, screenshot_rx) = mpsc::channel::<crate::action::ScreenshotCommand>();
    let (screenshot_done_tx, screenshot_done_rx) =
        mpsc::channel::<crate::action::ScreenshotResult>();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let mut region_selection = crate::action::selection::RegionSelection::new()?;
    let mut action_registry = crate::action::ActionRegistry::new();
    action_registry.register(Box::new(crate::action::screenshot::MiningContextScreenshot));
    sync_ocr_clipboard_action(
        &mut action_registry,
        live.actions_ocr_clipboard_hotkey.as_deref(),
    );
    // Allow one writer at a time.
    let mut save_job: Option<thread::JoinHandle<()>> = None;
    // BACKLOG 7: this is the only entry point.
    let mut settings: Option<SettingsWindow> = match SettingsWindow::open(
        &form_with_library(&cfg, &dicts, &library),
        &settings::stale_order_entries(&cfg, &dicts),
        ApplyMode::Live,
    ) {
        // A startup settings-window failure is never fatal.
        Err(e) => {
            eprintln!("chibipop: opening settings at startup failed: {e:#}");
            None
        }
        Ok(w) => {
            notice_drift(&w, &library, &db_path);
            Some(w)
        }
    };
    // Store the active in-place edit here.
    let mut edit: Option<EditFlight> = None;
    let mut edit_cfg: Option<(Config, bool)> = None;

    // I4: keep all capture-guard code in one place.
    let drain_capture_guard = || {
        while let Ok(req) = capture_guard_rx.try_recv() {
            match req {
                CaptureGuardMsg::Hide { ack } => {
                    capture_guard_prev_visible.set(popup.is_visible());
                    let _ = popup.hide();
                    btn_prev_visible.set(anki_button.as_ref().is_some_and(|b| b.is_visible()));
                    if let Some(b) = &anki_button {
                        b.hide();
                    }
                    if let Some(hwnd) = overlay_hwnd {
                        // SAFETY: `hwnd` is `Overlay::hwnd()`'s own handle.
                        // The `Overlay` that owns it lives in `run`'s local `overlay` for this
                        // whole loop, so the window stays live here. Both calls only read or set
                        // visibility. No other precondition applies.
                        overlay_prev_visible.set(unsafe { IsWindowVisible(hwnd).as_bool() });
                        unsafe {
                            let _ = ShowWindow(hwnd, SW_HIDE);
                        }
                    }
                    if let Some(hwnd) = sr_hwnd {
                        // SAFETY: `sr_hwnd` stays live for the loop, like `overlay_hwnd`.
                        sr_prev_visible.set(unsafe { IsWindowVisible(hwnd).as_bool() });
                        unsafe {
                            let _ = ShowWindow(hwnd, SW_HIDE);
                        }
                    }
                    let _ = ack.send(());
                }
                CaptureGuardMsg::Restore => {
                    if capture_guard_prev_visible.get() {
                        let _ = popup.show_without_activating();
                    }
                    if btn_prev_visible.get() {
                        if let Some(b) = &anki_button {
                            b.show_without_activating();
                        }
                    }
                    if let Some(hwnd) = overlay_hwnd {
                        if overlay_prev_visible.get() {
                            // SAFETY: this call uses the same live handle as the hide path above.
                            unsafe {
                                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                            }
                        }
                    }
                    if let Some(hwnd) = sr_hwnd {
                        if sr_prev_visible.get() {
                            // SAFETY: this call uses the live `sr_hwnd` handle from the loop state.
                            unsafe {
                                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                            }
                        }
                    }
                }
            }
        }
    };

    // Drive one Event through the state machine and handle every Command.
    // Handle `ShowPopup` here so `PopupPlaced` or `PopupPlaceFailed` enters the queue at once.
    macro_rules! drive {
        ($event:expr) => {
            drive(
                &mut controller,
                $event,
                &mut Exec {
                    popup: &popup,
                    renderer: &mut renderer,
                    theme: &theme,
                    cfg: &cfg,
                    live: &live,
                    exe_dir: &exe_dir,
                    overlay: overlay.as_ref(),
                    anki_button: anki_button.as_ref(),
                    trigger_tx: worker.trigger(),
                    dicts: &dicts,
                    anki_tx: &anki_tx,
                    add_tx: &add_tx,
                    main_tid,
                    want_settings: &mut want_settings,
                    pending_shot: &mut pending_shot,
                    dupe_cache: &dupe_cache,
                    analysis: &analysis_service,
                    pointer_buttons: &mut pointer_buttons,
                    last_pointer: &mut last_pointer,
                    last_pointer_text: &mut last_pointer_text,
                },
            )
        };
    }

    // The Worker started before the Dictionary identities were known.
    drive!(Event::ConfigReloaded(Box::new(controller_config(&live))));

    // The screenshot worker uses pure Rust, so it needs no WinRT apartment.
    {
        let rx = screenshot_rx;
        let tx = screenshot_done_tx;
        let tid = main_tid;
        thread::spawn(move || {
            for cmd in rx {
                let result = handle_screenshot_save(cmd);
                let _ = tx.send(result);
                // SAFETY: This call wakes the main loop.
                unsafe {
                    let _ = PostThreadMessageW(tid, WM_APP_SCREENSHOT_DONE, WPARAM(0), LPARAM(0));
                }
            }
        });
    }
    let mut msg = MSG::default();

    loop {
        // Read window and thread messages.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break; // 0 means WM_QUIT. -1 means an error. Stop the message loop in either case.
        }

        // Route messages for the modeless settings window.
        if let Some(w) = &settings {
            // The region picker runs a nested message pump.
            w.pump(|| {
                Hooks::set_scroll_armed(false);
                Hooks::set_click_armed(false);
                drain_capture_guard();
            });

            if matches!(msg.message, WM_KEYDOWN | WM_SYSKEYDOWN)
                && w.handle_capture_key(msg.wParam.0 as u16)
            {
                continue;
            }

            // SAFETY: `w.hwnd()` stays live until `SettingsWindow` drops.
            // `msg` is this loop's stack storage.
            let handled = unsafe { IsDialogMessageW(w.hwnd(), &msg) }.as_bool();
            service_settings_click(
                w,
                &settings_tx,
                &detect_tx,
                &mut detect_gen,
                main_tid,
                &mut css_editor,
            );

            // Start deck, model, and field detection after a tab switch.
            if let Some(tab) = w.take_tab_change() {
                w.switch_tab(tab);
                if tab == 3 {
                    spawn_detect(
                        next_anki_detect_generation(&mut detect_gen),
                        w.anki_url(),
                        w.anki_model(),
                        detect_tx.clone(),
                        main_tid,
                    );
                }
            }
            if w.take_field_map_toggle() {
                w.toggle_field_map();
            }
            if w.take_anki_model_change() {
                spawn_detect_fields(
                    next_anki_detect_generation(&mut detect_gen),
                    w.anki_url(),
                    w.anki_model(),
                    detect_tx.clone(),
                    main_tid,
                );
            }

            if handled {
                continue;
            }
        }

        if msg.message == WM_TIMER && msg.wParam.0 == timer_id {
            // Read the popup's current rect.
            let cursor_pos = cursor_now();
            let button_h = anki_button
                .as_ref()
                .filter(|b| b.is_visible())
                .map_or(0, |b| b.height_phys());
            drive!(Event::Tick {
                cursor: cursor_pos,
                button_h
            });

            Hooks::set_outside_watch(
                matches!(live.trigger_mode, crate::config::TriggerMode::Press)
                    && controller.popup().is_some(),
            );
            if let Some(point) = Hooks::take_outside_click() {
                if let Some(view) = controller.popup() {
                    let popup = PhysRect { h: view.popup.h + button_h, ..view.popup };
                    if !popup.contains(point) {
                        drive!(Event::PointerDownOutside);
                    }
                }
            }

            let notches = Hooks::take_whole_notches();
            if notches != 0 {
                drive!(Event::Scrolled { notches });
            }

            let mut pointer_move = Hooks::take_pointer_move();
            for edge in Hooks::take_pointer_events() {
                let (button, bit) = match edge.button {
                    crate::input::hooks::PointerButton::Left => (Button::Primary, 1u8),
                    crate::input::hooks::PointerButton::Right => (Button::Secondary, 2u8),
                };
                if edge.down {
                    pointer_buttons |= bit;
                    if let Some((local, scroll)) = popup_local(&controller, edge.point) {
                        if anki_button_hit(&controller, anki_button.as_ref(), edge.point) {
                            if button == Button::Primary {
                                drive!(Event::AddRequested);
                            }
                            continue;
                        }
                        let hit = renderer.hit_test(local.x, local.y, scroll);
                        let text = popup_text_hit(&mut renderer, local, scroll);
                        last_pointer = Some(local);
                        last_pointer_text = text;
                        drive!(Event::PointerDown { local, button, hit, text });
                    }
                } else {
                    if pointer_buttons != 0 {
                        if let Some(point) = pointer_move.take() {
                            if let Some((local, scroll)) = popup_local(&controller, point) {
                                let text = popup_text_hit(&mut renderer, local, scroll);
                                last_pointer = Some(local);
                                last_pointer_text = text;
                                drive!(Event::PointerMoved { local, text });
                            }
                        }
                    }
                    pointer_buttons &= !bit;
                    let local = popup_local(&controller, edge.point)
                        .map(|(local, _)| local)
                        .or(last_pointer);
                    if let Some(local) = local {
                        drive!(Event::PointerUp { local, button });
                    }
                }
            }
            if pointer_buttons != 0 {
                if let Some(point) = pointer_move {
                    if let Some((local, scroll)) = popup_local(&controller, point) {
                        let text = popup_text_hit(&mut renderer, local, scroll);
                        last_pointer = Some(local);
                        last_pointer_text = text;
                        drive!(Event::PointerMoved { local, text });
                    }
                }
            }
            if controller.popup().is_none() {
                pointer_buttons = 0;
                last_pointer = None;
                last_pointer_text = None;
            }

            // Use direct WM_LBUTTONDOWN as a fallback.
            if anki_button.as_ref().is_some_and(|b| b.take_click()) {
                drive!(Event::AddRequested);
            }

            // Every add path sends one Event. The state machine chooses the result,
            // and `AddNote` handles any screenshot.
            if Hooks::take_add_hotkey() {
                drive!(Event::AddRequested);
            }

            // Handle the static-region hotkey in slot 1.
            // This hotkey works in every sentence mode.
            if Hooks::take_action_hotkey(1) {
                let had_popup = controller.popup().is_some();
                let _ = popup.hide();
                if let Some(b) = &anki_button {
                    b.hide();
                }
                if let Some(ov) = &static_overlay {
                    ov.hide();
                }
                let rect = region_selection.run();
                if let Some(rect) = rect {
                    live.static_region = Some(rect);
                    cfg.anki.static_region = Some([rect.x, rect.y, rect.w, rect.h]);
                    save_in_background(
                        &mut save_job,
                        cfg.clone(),
                        config_path.to_path_buf(),
                        save_tx.clone(),
                        main_tid,
                    );
                    if let Some(ov) = &static_overlay {
                        if let Err(e) = ov.show(rect) {
                            eprintln!("chibipop: showing static region overlay failed: {e:#}");
                        }
                    }
                    // `Controller` returns a reload with fresh `WorkerSettings`.
                    drive!(Event::ConfigReloaded(Box::new(controller_config(&live))));
                    eprintln!(
                        "chibipop: static region set to ({}, {}, {}x{})",
                        rect.x, rect.y, rect.w, rect.h
                    );
                }
                if had_popup {
                    let _ = popup.show_without_activating();
                    if let Some(b) = &anki_button {
                        b.show_without_activating();
                    }
                }
            }

            // Dispatch action hotkeys.
            for slot in 0..crate::input::hooks::MAX_ACTION_SLOTS {
                if slot == 1 {
                    continue; // Slot 1 was handled above.
                }
                if !Hooks::take_action_hotkey(slot) {
                    continue;
                }
                let had_popup = controller.popup().is_some();
                if had_popup {
                    let _ = popup.hide();
                    if let Some(b) = &anki_button {
                        b.hide();
                    }
                }

                let outcome = {
                    let view = controller.popup();
                    let state = crate::action::AppState {
                        popup_visible: had_popup,
                        presentation: view.as_ref().map(|v| v.presentation),
                        anchor: view.as_ref().map(|v| v.anchor),
                        anki_connected: view.as_ref().is_some_and(|v| v.anki.connected),
                    };
                    let mut ctx = crate::action::ActionContext {
                        selection: &mut region_selection,
                        config: &cfg.actions,
                        exe_dir: &exe_dir,
                        screenshot_tx: &screenshot_tx,
                        ocr_jobs: ocr_jobs.clone(),
                    };
                    action_registry.dispatch(slot, &state, &mut ctx)
                };

                match outcome {
                    Some(crate::action::ActionOutcome::ScreenshotCaptured {
                        bgra_buf,
                        width,
                        height,
                        save_dir,
                        target,
                    }) => {
                        // The mining screenshot uses its own card path.
                        // It ignores `include_on_add` and the add guards, so it uses the ungated plan.
                        if let Some(view) = controller.popup() {
                            if let Err(e) = persist_screenshot_target(
                                &mut cfg,
                                &target,
                                config_path,
                                &mut save_job,
                            ) {
                                eprintln!("chibipop: saving screenshot target failed: {e:#}");
                            }
                            if let Some(w) = &settings {
                                w.refresh_screenshot_targets(&cfg.actions.screenshot);
                            }
                            let cmd = crate::action::ScreenshotCommand {
                                bgra_buf,
                                width,
                                height,
                                plan: crate::shot::plan(
                                    &view,
                                    &cfg,
                                    &save_dir,
                                    crate::shot::epoch_secs(),
                                ),
                                anki: anki_snapshot(&cfg, &live),
                                anki_connected: view.anki.connected,
                            };
                            let _ = screenshot_tx.send(cmd);
                        }
                    }
                    Some(crate::action::ActionOutcome::Failed(msg)) => {
                        eprintln!("chibipop: action failed: {msg}");
                    }
                    Some(crate::action::ActionOutcome::TextCaptured { text }) => {
                        if let Err(e) = crate::clipboard::set_text(&text) {
                            eprintln!("chibipop: copying OCR text failed: {e:#}");
                        }
                    }
                    _ => {}
                }

                // Restore the windows after capture.
                if had_popup {
                    let _ = popup.show_without_activating();
                    if let Some(b) = &anki_button {
                        b.show_without_activating();
                    }
                }
                sync_anki_button(anki_button.as_ref(), controller.popup(), &theme);
            }

            if Hooks::take_back() {
                drive!(Event::BackRequested);
            }

            if let Some(ed) = &css_editor {
                if let Some(crate::ui::editor::EditorOutcome::Applied) = ed.take_outcome() {
                    theme = theme_from_config(&live.popup);
                    if let Some(v) = controller.popup() {
                        let selection = controller.selection();
                        let scroll = v.scroll;
                        let painted = renderer.paint(
                            SceneInputs {
                                presentation: v.presentation,
                                theme: &theme,
                                show_back: v.show_back,
                                side_panel: live.side_panel,
                                render: live.popup.render_settings(),
                                selection,
                            },
                            scroll,
                        );
                        // The view borrow ends here. A drag resolves the
                        // pointer against the repainted scene.
                        if painted.is_ok() && pointer_buttons != 0 {
                            if let Some(local) = last_pointer {
                                let text = popup_text_hit(&mut renderer, local, scroll);
                                if text != last_pointer_text {
                                    last_pointer_text = text;
                                    drive!(Event::PointerMoved { local, text });
                                }
                            }
                        }
                    }
                }
                if !ed.is_visible() {
                    css_editor = None;
                }
            }

            if let Some(w) = &settings {
                if edit.is_some() {
                    // Do not read window outcomes while the database changes.
                    let _ = w.take_outcome();
                    let done = edit.as_ref().and_then(|f| pump_edit(&f.rx, w));
                    if let Some(done) = done {
                        edit = None;
                        w.set_busy(false);
                        match done {
                            Err(e) => {
                                edit_cfg = None;
                                refuse_apply(w, &e);
                            }
                            Ok(report) => {
                                let report = *report;
                                let status = edit_status(&report);
                                let (mut updated, reset_screenshot_targets) = edit_cfg
                                    .take()
                                    .unwrap_or_else(|| (cfg.clone(), false));
                                // Apply removals first because Dictionary names can collide.
                                for name in &report.removed {
                                    settings::dictionary_removed(&mut updated, name);
                                }
                                for (name, roles) in &report.added {
                                    settings::dictionary_added(&mut updated, name, *roles);
                                }
                                // A Dictionary Apply can outlive a target capture. Preserve that
                                // target unless this Apply explicitly reset both target fields.
                                if !reset_screenshot_targets {
                                    updated.actions.screenshot.fixed_region =
                                        cfg.actions.screenshot.fixed_region;
                                    updated.actions.screenshot.fixed_window =
                                        cfg.actions.screenshot.fixed_window.clone();
                                }
                                // Replace the stale Dictionary identity cache.
                                dicts = report.dicts;
                                w.clear_staged();
                                w.clear_screenshot_reset_targets();
                                w.reseed_per_language(&updated.dictionaries.per_language);
                                cfg = updated.clone();
                                live = derive(&cfg);
                                live.present_cfg = cfg.present_config(&dicts);
                                sync_ocr_clipboard_action(
                                    &mut action_registry,
                                    live.actions_ocr_clipboard_hotkey.as_deref(),
                                );
                                apply_live(
                                    &live,
                                    &popup,
                                    overlay.as_ref(),
                                    anki_button.as_ref(),
                                    static_overlay.as_ref(),
                                    &mut theme,
                                    &capture_guard_active,
                                );
                                // Reload the Controller to discard stale results.
                                drive!(Event::ConfigReloaded(Box::new(controller_config(&live),)));
                                save_in_background(
                                    &mut save_job,
                                    updated,
                                    config_path.to_path_buf(),
                                    save_tx.clone(),
                                    main_tid,
                                );
                                w.set_status(&status);
                            }
                        }
                    }
                } else {
                    match w.take_outcome() {
                        // Keep the tray and hide only the settings window.
                        Some(SettingsOutcome::Cancel) => settings = None,
                        // The main thread handles this event directly.
                        Some(SettingsOutcome::Quit) => drive!(Event::Quit),
                        Some(SettingsOutcome::Apply) => {
                            let t0 = std::time::Instant::now();
                            let edited = w.read(&form_with_library(&cfg, &dicts, &library));
                            let updated = settings::apply_to(&edited, &cfg);
                            if edited.has_staged() {
                                match LibraryLock::acquire(&library) {
                                    Err(e) => refuse_apply(w, &e),
                                    Ok(lock) => {
                                        begin_apply(w);
                                        edit_cfg = Some((updated, edited.screenshot_reset_targets));
                                        let (etx, erx) = mpsc::channel::<EditMsg>();
                                        edit = Some(EditFlight {
                                            rx: erx,
                                            _lock: lock,
                                        });
                                        let db = db_path.clone();
                                        let dir = library.clone();
                                        thread::spawn(move || {
                                            let done = apply_edits_with_frequencies(
                                                &db, &dir, &edited, &etx,
                                            );
                                            let _ = etx.send(EditMsg::Done(done));
                                        });
                                        let ms = t0.elapsed().as_millis();
                                        if ms > APPLY_BUDGET_MS {
                                            eprintln!(
                                                "chibipop: Apply took {ms} ms \
                                                 (budget {APPLY_BUDGET_MS})"
                                            );
                                        }
                                    }
                                }
                            } else {
                                // Read Dictionary work before `cfg` becomes the new value.
                                // The old and new frequency inputs determine the work.
                                let work = settings::dictionary_work(&cfg, &updated);
                                live = derive(&updated);
                                live.present_cfg = updated.present_config(&dicts);
                                sync_ocr_clipboard_action(
                                    &mut action_registry,
                                    live.actions_ocr_clipboard_hotkey.as_deref(),
                                );
                                apply_live(
                                    &live,
                                    &popup,
                                    overlay.as_ref(),
                                    anki_button.as_ref(),
                                    static_overlay.as_ref(),
                                    &mut theme,
                                    &capture_guard_active,
                                );
                                drive!(Event::ConfigReloaded(Box::new(controller_config(&live),)));
                                let clamped = settings::clamp_notice(&edited, &updated);
                                w.reseed_per_language(&updated.dictionaries.per_language);
                                w.clear_screenshot_reset_targets();
                                cfg = updated.clone();
                                save_in_background(
                                    &mut save_job,
                                    updated,
                                    config_path.to_path_buf(),
                                    save_tx.clone(),
                                    main_tid,
                                );
                                let mut status_parts = Vec::new();
                                match &clamped {
                                    Some(notice) => {
                                        w.set_capture_fields(&cfg.ocr);
                                        status_parts.push(notice.clone());
                                    }
                                    None => status_parts.push("Settings applied.".to_string()),
                                }
                                // A strategy, order, or checkbox change updates `term.freq` in place.
                                // It never reads an archive. The database already stores the claims.
                                // An archive edit owns the archive pass, and a rebuild owns neither.
                                if work == settings::DictionaryWork::Reindex {
                                    w.set_busy(true);
                                    let done = reindex_ranks(&db_path, &cfg, &dicts, w);
                                    w.set_busy(false);
                                    status_parts.push(match done {
                                        Ok(rows) => {
                                            format!("Reranked {rows} term rows.")
                                        }
                                        Err(e) => {
                                            eprintln!(
                                                "chibipop: reranking failed: {e:#}"
                                            );
                                            format!("Frequency rankings not updated: {e}")
                                        }
                                    });
                                }
                                if cfg.debug.show_engine_log {
                                    status_parts.push(engine_status_line(&cfg));
                                }
                                if cfg.debug.show_adapter_log {
                                    status_parts.push(adapter_status_line(&cfg));
                                }
                                w.set_status(&status_parts.join("\r\n"));
                                let ms = t0.elapsed().as_millis();
                                if ms > APPLY_BUDGET_MS {
                                    eprintln!(
                                        "chibipop: Apply took {ms} ms (budget {APPLY_BUDGET_MS})"
                                    );
                                }
                            }
                        }
                        None => {}
                    }
                }
            }

            // A trigger-key release retracts the popup.
            let held = Hooks::trigger_held();
            if held != trigger_was_held {
                trigger_was_held = held;
                if held {
                    drive!(Event::TriggerDown);
                } else {
                    if !matches!(live.trigger_mode, crate::config::TriggerMode::Live) {
                        // Do not restore visibility because restore would show the popup again.
                        capture_guard_prev_visible.set(false);
                        overlay_prev_visible.set(false);
                        btn_prev_visible.set(false);
                    }
                    drive!(Event::TriggerUp);
                }
            }
            let press = Hooks::take_press().then_some(cursor_pos);
            let cursor = Hooks::take_pending().unwrap_or_else(|| {
                let pos = cursor_pos;
                let dominated = Hooks::poll_gate(pos);
                if dominated {
                    pos
                } else {
                    PhysPoint {
                        x: i32::MIN,
                        y: i32::MIN,
                    }
                }
            });
            for event in route_hook_events(
                press,
                (cursor.x != i32::MIN).then_some(cursor),
            )
            .into_iter()
            .flatten()
            {
                drive!(event);
            }

        } else if msg.message == WM_APP_RESULT {
            // Keep every add-time sentence result. Keep only the newest other
            // Worker result. Preserve the arrival order of retained results.
            let mut results = Vec::new();
            while let Ok(r) = worker.results().try_recv() {
                results.push(r);
            }
            for result in route_results(results) {
                drive!(Event::LookupResult {
                    id: result.id,
                    outcome: result.outcome
                });
            }
        } else if msg.message == WM_APP_ANALYSIS {
            while let Ok((generation, words)) = analysis_service.results().try_recv() {
                drive!(Event::AnalysisReady { generation, words });
            }
        } else if msg.message == WM_APP_ANKI {
            while let Ok(result) = anki_rx.try_recv() {
                if let Some(found) = &result.dupes {
                    for expr in &result.checked {
                        dupe_cache.insert(expr.clone(), found.contains(expr));
                    }
                }
                drive!(Event::DupesChecked {
                    generation: result.gen,
                    dupes: result.dupes
                });
            }
        } else if msg.message == WM_APP_ADD_NOTE {
            while let Ok(result) = add_rx.try_recv() {
                if let Some(e) = &result.err {
                    eprintln!("chibipop: add to Anki failed: {e}");
                } else {
                    dupe_cache.insert(result.expr.clone(), true);
                    if live.notify_on_add {
                        tray.notify("chibipop", &format!("{} added", result.expr));
                    }
                }
                let failed = result.err.is_some();
                drive!(Event::NoteAdded {
                    expr: result.expr,
                    failed
                });
            }
        } else if msg.message == WM_APP_SETTINGS {
            while let Ok(status) = settings_rx.try_recv() {
                if let Some(w) = &settings {
                    if status.matches(detect_gen) {
                        w.set_status(&status.text);
                    }
                }
            }
        } else if msg.message == WM_APP_ANKI_DETECT {
            while let Ok(result) = detect_rx.try_recv() {
                if let Some(w) = &settings {
                    apply_anki_detect(w, detect_gen, result);
                }
            }
        } else if msg.message == WM_APP_SAVED {
            while let Ok(result) = save_rx.try_recv() {
                if let Err(e) = result {
                    eprintln!(
                        "chibipop: could not save settings to {}: {e:#}",
                        config_path.display()
                    );
                    if let Some(w) = &settings {
                        w.set_status(
                            "Settings applied, but could not be saved - \
                             they will be lost on restart.",
                        );
                    }
                }
            }
        } else if msg.message == WM_APP_SCREENSHOT_DONE {
            while let Ok(result) = screenshot_done_rx.try_recv() {
                match &result.filed {
                    Ok(Some(_)) => {
                        // The word is now in the collection. Mark the cache so a cached `false`
                        // does not survive the add.
                        dupe_cache.insert(result.expr.clone(), true);
                        if live.notify_on_add {
                            tray.notify("chibipop", &format!("{} added", result.expr));
                        }
                    }
                    // Report the saved PNG instead of an "added" notification.
                    // The PNG on disk is the complete result.
                    Ok(None) => eprintln!(
                        "chibipop: screenshot saved to {} - no card to file it on",
                        result.dir.display()
                    ),
                    Err(e) => eprintln!("chibipop: screenshot failed: {e}"),
                }
                // Report the failure. `start_add` marked the popup for the add before
                // it sent this result here.
                if let Some(failed) = result.add_failed() {
                    drive!(Event::NoteAdded {
                        expr: result.expr,
                        failed
                    });
                }
            }
        } else if msg.message == WM_APP_CAPTURE_GUARD {
            // Drain every queued request on each wakeup.
            drain_capture_guard();
        } else if let Some(cmd) = tray.handle_message(msg.message, msg.lParam, || {
            // The menu consumes WM_TIMER messages.
            Hooks::set_scroll_armed(false);
            Hooks::set_click_armed(false);
            drain_capture_guard();
        }) {
            match cmd {
                TrayCommand::OpenSettings => drive!(Event::TrayAction(TrayAction::OpenSettings)),
                TrayCommand::Quit => drive!(Event::TrayAction(TrayAction::Quit)),
            }
            if std::mem::take(&mut want_settings) {
                if let Some(w) = &settings {
                    w.focus();
                } else {
                    let form = form_with_library(&cfg, &dicts, &library);
                    let stale = settings::stale_order_entries(&cfg, &dicts);
                    match SettingsWindow::open(&form, &stale, ApplyMode::Live) {
                        // A settings-window failure is never fatal.
                        Err(e) => eprintln!("chibipop: opening settings failed: {e:#}"),
                        Ok(w) => {
                            notice_drift(&w, &library, &db_path);
                            settings = Some(w);
                        }
                    }
                }
            }
        } else {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        // Handle the OS half of screenshot-on-add outside every Command batch.
        if let Some(pending) = pending_shot.take() {
            let _ = popup.hide();
            if let Some(b) = &anki_button {
                b.hide();
            }
            let selected = match crate::action::screenshot::select_target(
                &mut region_selection,
                &cfg.actions.screenshot,
            ) {
                Ok(Some(target)) => {
                    match crate::text::capture::capture_upscaled_by(target.rect(), 1) {
                        Ok(cap) => {
                            if let Err(e) = persist_screenshot_target(
                                &mut cfg,
                                &target,
                                config_path,
                                &mut save_job,
                            ) {
                                eprintln!("chibipop: saving screenshot target failed: {e:#}");
                            }
                            if let Some(w) = &settings {
                                w.refresh_screenshot_targets(&cfg.actions.screenshot);
                            }
                            Some((target, cap))
                        }
                        Err(e) => {
                            eprintln!("chibipop: grabbing the screenshot failed: {e:#}");
                            None
                        }
                    }
                }
                Ok(None) => None,
                Err(e) => {
                    eprintln!("chibipop: resolving the screenshot target failed: {e:#}");
                    None
                }
            };
            let view = screenshot_restore_view(&controller);
            if view.is_some() {
                let _ = popup.show_without_activating();
            }
            sync_anki_button(anki_button.as_ref(), view, &theme);
            match selected {
                Some((_target, cap)) => {
                    let _ = screenshot_tx.send(crate::action::ScreenshotCommand {
                        bgra_buf: cap.buf,
                        width: cap.w,
                        height: cap.h,
                        plan: pending.plan,
                        anki: anki_snapshot(&cfg, &live),
                        anki_connected: pending.anki_connected,
                    });
                }
                // If the user cancels or the grab fails, send the add without a screenshot.
                // The popup already marks the add, so this path must clear that state.
                None => {
                    let PendingShot { plan, .. } = pending;
                    spawn_add_note(plan.expr, plan.fields, &live, &add_tx, main_tid);
                }
            }
        }
    }

    // Shut down in the order from decision 5.
    unsafe {
        let _ = KillTimer(None, timer_id);
    }
    // Drop the hooks before shutdown can block.
    drop(hooks.take());
    // Disable capture-guard acks before shutdown can block.
    capture_guard_active.store(false, Ordering::SeqCst);
    // Wait for the save before `exit(0)`. Otherwise, the process can stop mid-write.
    join_save(&mut save_job);
    std::process::exit(0)
}

/// Finds a text-provider plugin by name.
fn find_text_plugin<'a>(
    found: &'a [(PathBuf, Result<Manifest>)],
    name: &str,
) -> Result<(&'a Path, &'a Manifest)> {
    let hit = found.iter().find(|(dir, parsed)| {
        parsed.as_ref().map(|m| m.name == name).unwrap_or(false)
            || dir.file_name().map(|f| f == name).unwrap_or(false)
    });
    let Some((dir, parsed)) = hit else {
        bail!("plugin \"{name}\" is not on disk");
    };
    let m = parsed
        .as_ref()
        .map_err(|e| anyhow!("plugin \"{name}\": {e:#}"))?;
    if !m.roles.contains(&crate::plugin::manifest::Role::TextProvider) {
        bail!("plugin \"{name}\" is not a text-provider");
    }
    Ok((dir.as_path(), m))
}

/// Starts one plugin process.
///
/// The function returns a concrete `PluginText`, so the caller chooses whether
/// to box it at the `OcrEngine` seam.
fn spawn_plugin_engine(name: &str) -> Result<Box<PluginText>> {
    let root = crate::paths::beside_exe("plugins");
    let found = discover::discover(&root);
    let (dir, m) = find_text_plugin(&found, name)?;
    let h = host::spawn(m, dir).with_context(|| format!("starting plugin \"{name}\""))?;
    Ok(Box::new(PluginText::new(h, m)))
}

/// Selects and starts the configured engine.
///
/// `None` selects the built-in engine.
fn resolve_plugin_engine(ocr_engine: &str, enabled: &[String]) -> Option<Box<PluginText>> {
    match resolve_engine(ocr_engine, enabled) {
        EngineChoice::Builtin => None,
        EngineChoice::Plugin(name) => match spawn_plugin_engine(&name) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("chibipop: OCR plugin \"{name}\" failed, falling back to builtin: {e:#}");
                None
            }
        },
        EngineChoice::FellBack(name) => {
            eprintln!("chibipop: OCR engine \"{name}\" is not enabled, falling back to builtin");
            None
        }
    }
}
/// Measures, places, shows, and paints the popup.
fn show_presentation(
    popup: &Popup,
    renderer: &mut Renderer,
    max_height_percent: i32,
    max_width_percent: i32,
    inputs: SceneInputs<'_>,
    anchor: PhysRect,
    scroll: i32,
) -> Result<(PhysRect, i32, i32)> {
    let monitor = monitor_rect_for(anchor);
    let max_w = ((monitor.w * max_width_percent) / 100).max(1);
    let max_h = ((monitor.h * max_height_percent) / 100).max(1);

    // Use `view_h` below, not `content_h`.
    let (w, view_h, content_h) = renderer
        .measure(inputs, (max_w, max_h))
        .context("measuring popup content")?;

    let rect = place_popup(anchor, (w, view_h), monitor, POPUP_GAP);
    popup.show_at(rect).context("moving/showing the popup")?;
    renderer
        .paint(inputs, scroll)
        .context("painting the popup")?;
    Ok((rect, content_h, view_h))
}

/// Returns the `[anki]` section that the pump uses.
/// It starts with the config values and applies `derive`'s empty-field-map fallback.
/// A screenshot add therefore uses the same fields as a plain add.
fn anki_snapshot(cfg: &Config, live: &LiveSettings) -> crate::config::AnkiConfig {
    crate::config::AnkiConfig {
        field_map: live.anki_field_map.clone(),
        ..cfg.anki.clone()
    }
}

/// Adds one note on a background thread.
fn spawn_add_note(
    expr: String,
    fields: HashMap<String, String>,
    live: &LiveSettings,
    add_tx: &mpsc::Sender<AddNoteResult>,
    main_tid: u32,
) {
    let url = live.anki_url.clone();
    let deck = live.anki_deck.clone();
    let model = live.anki_model.clone();
    let field_map = live.anki_field_map.clone();
    let tx = add_tx.clone();
    thread::spawn(move || {
        let err = anki::add_note(&url, &deck, &model, &fields, &field_map)
            .err()
            .map(|e| format!("{e:#}"));
        let _ = tx.send(AddNoteResult { expr, err });
        // SAFETY: This call wakes the main loop.
        unsafe {
            let _ = PostThreadMessageW(main_tid, WM_APP_ADD_NOTE, WPARAM(0), LPARAM(0));
        }
    });
}

/// Encodes, writes, and files the screenshot.
/// Core (`chibipop::shot`) owns every rule. This function runs off the pump.
fn handle_screenshot_save(
    cmd: crate::action::ScreenshotCommand,
) -> crate::action::ScreenshotResult {
    let filed = (|| -> anyhow::Result<Option<i64>> {
        let png = crate::image::encode_bgra_to_png(&cmd.bgra_buf, cmd.width, cmd.height)?;
        if cmd.anki_connected && !cmd.plan.expr.is_empty() {
            return crate::shot::save_and_add(&png, &cmd.plan, &cmd.anki).map(Some);
        }
        // If Anki is unreachable, save the screenshot without a card.
        // No card means that no add occurs.
        crate::shot::save(&png, &cmd.plan)?;
        Ok(None)
    })();
    crate::action::ScreenshotResult {
        dir: cmd
            .plan
            .path
            .parent()
            .unwrap_or(cmd.plan.path.as_path())
            .to_path_buf(),
        expr: cmd.plan.expr,
        filed: filed.map_err(|e| format!("{e:#}")),
    }
}

/// Starts the popup process again with this argv.
fn start_run(config_path: &Path, dict_path: &Path) -> Result<()> {
    let exe = std::env::current_exe().context("locating this executable")?;
    // Pass the configured paths explicitly. They can differ from the defaults.
    std::process::Command::new(exe)
        .arg("run")
        .arg("--config")
        .arg(config_path)
        .arg("--dict")
        .arg(dict_path)
        .spawn()
        .context("starting chibipop")?;
    Ok(())
}

/// Returns the live cursor position instead of the gated point.
fn cursor_now() -> PhysPoint {
    let mut pt = POINT::default();
    // SAFETY: This FFI call receives a pointer to local stack storage that
    // remains valid for the call. On failure, `pt` stays zeroed and the wheel
    // remains disarmed for one tick.
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    PhysPoint { x: pt.x, y: pt.y }
}
/// Convert a screen point into popup-local physical coordinates.
fn popup_local(controller: &Controller, screen: PhysPoint) -> Option<(PhysPoint, i32)> {
    let view = controller.popup()?;
    Some((
        PhysPoint {
            x: screen.x - view.popup.x,
            y: screen.y - view.popup.y,
        },
        view.scroll,
    ))
}

/// Hit-test the cached `PopupScene`.
///
/// When measurement fails, return `None` so input handling can continue.
fn popup_text_hit(renderer: &mut Renderer, local: PhysPoint, scroll: i32) -> Option<TextAddr> {
    match renderer.text_hit(local, scroll) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("chibipop: popup text hit failed: {e}");
            None
        }
    }
}
/// Return whether a screen point is inside the separate Anki button.
fn anki_button_hit(
    controller: &Controller,
    button: Option<&AnkiButton>,
    screen: PhysPoint,
) -> bool {
    let Some(button) = button.filter(|b| b.is_visible()) else {
        return false;
    };
    let Some(view) = controller.popup() else {
        return false;
    };
    screen.x >= view.popup.x
        && screen.x < view.popup.x + view.popup.w
        && screen.y >= view.popup.y + view.popup.h
        && screen.y < view.popup.y + view.popup.h + button.height_phys()
}

/// Returns the monitor that contains the anchor.
fn monitor_rect_for(anchor: PhysRect) -> PhysRect {
    let c = anchor.center();
    let pt = POINT { x: c.x, y: c.y };
    unsafe {
        // `MonitorFromPoint` never returns null with this flag.
        let hmon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(hmon, &mut mi).as_bool() {
            let rc = mi.rcWork;
            PhysRect {
                x: rc.left,
                y: rc.top,
                w: rc.right - rc.left,
                h: rc.bottom - rc.top,
            }
        } else {
            eprintln!("chibipop: GetMonitorInfoW failed; placing against a 1920x1080 fallback");
            PhysRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            }
        }
    }
}


/// Returns the current popup state for screenshot restoration.
fn screenshot_restore_view(controller: &Controller) -> Option<PopupView<'_>> {
    controller.popup()
}

/// Places, paints, or hides the Anki button.
///
/// The button sits below the popup and has the same left and right edges.
fn sync_anki_button(btn: Option<&AnkiButton>, view: Option<PopupView<'_>>, theme: &Theme) {
    let Some(btn) = btn else { return };
    let Some(v) = view else {
        btn.hide();
        return;
    };
    let Some((text, color)) = anki_button_label(v.presentation, theme, v.anki) else {
        btn.hide();
        return;
    };
    let r = PhysRect {
        x: v.popup.x,
        y: v.popup.y + v.popup.h,
        w: v.popup.w,
        h: btn.height_phys(),
    };
    if let Err(e) = btn.show_at(r) {
        eprintln!("chibipop: positioning the Anki button failed: {e:#}");
        return;
    }
    btn.render(&text, color, theme);
}

/// Builds the Controller configuration from live settings.
fn controller_config(live: &LiveSettings) -> ControllerConfig {
    ControllerConfig {
        trigger_mode: live.trigger_mode,
        per_character_lookup: live.per_character_lookup,
        scroll_popup: live.scroll_popup,
        anki_enabled: live.anki_enabled,
        include_dictionary_name: live.include_dictionary_name,
        first_dict_only: live.first_dict_only,
        summary_chars: live.summary_chars,
        log_lookups: live.show_lookup_log,
        tick_ms: DISPATCH_TICK_MS,
        roles: live.popup.render_settings().roles,
        edge_autoscroll: live.popup.edge_autoscroll,
        primary_additive: live.selection_buttons == SelectionButtons::PrimaryAdditive,
        separator: live.selection_separator.into(),
        triple_click: live.triple_click,
        sentence_probe: live.sentence_mode == crate::config::SentenceMode::Sentence,
    }
}


/// Stores one add while the OS does the screenshot-on-add steps.
///
/// `AddNote` parks it and the pump drains it after the Command batch.
/// The region selector owns a nested message pump, so this code cannot enter
/// it inside a Command batch.
struct PendingShot {
    plan: crate::shot::ShotPlan,
    /// The popup's AnkiConnect state when the add was authorized.
    /// `false` still writes the PNG but does not file a card.
    anki_connected: bool,
}

/// Provides the values that Command handling needs.
struct Exec<'a> {
    popup: &'a Popup,
    renderer: &'a mut Renderer,
    theme: &'a Theme,
    /// `AddNote` passes the full config to `chibipop::shot`.
    /// Core owns the screenshot-on-add rule.
    cfg: &'a Config,
    live: &'a LiveSettings,
    exe_dir: &'a Path,
    overlay: Option<&'a Overlay>,
    anki_button: Option<&'a AnkiButton>,
    trigger_tx: &'a mpsc::Sender<Trigger>,
    dicts: &'a [DictInfo],
    anki_tx: &'a mpsc::Sender<AnkiDupeResult>,
    add_tx: &'a mpsc::Sender<AddNoteResult>,
    main_tid: u32,
    /// The loop handles `OpenSettings`.
    want_settings: &'a mut bool,
    /// An add that needs a screenshot. The loop does the OS half.
    pending_shot: &'a mut Option<PendingShot>,
    /// This cache is read-only here. The pump owns all writes.
    dupe_cache: &'a HashMap<String, bool>,
    /// The Japanese analysis service has the same process lifetime as the Worker.
    analysis: &'a chibipop::analysis::Service,
    /// Button bits and the last local point let repaint feedback continue the drag.
    pointer_buttons: &'a mut u8,
    last_pointer: &'a mut Option<PhysPoint>,
    last_pointer_text: &'a mut Option<TextAddr>,
}

/// Drives one Event until the state machine has no more work.
///
/// The function handles `ShowPopup` immediately, so `PopupPlaced` or
/// `PopupPlaceFailed` enters the queue at once.
fn drive(controller: &mut Controller, event: Event, x: &mut Exec<'_>) {
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(event);
    while let Some(ev) = queue.pop_front() {
        for cmd in controller.handle(ev) {
            if let Some(feedback) = execute(controller, cmd, x) {
                queue.push_back(feedback);
            }
        }
    }
}

/// Handles one Command and returns any feedback Event.
fn execute(controller: &Controller, cmd: Command, x: &mut Exec<'_>) -> Option<Event> {
    match cmd {
        // Windows excludes its popup from its own captures at the OS level.
        // It uses WDA_EXCLUDEFROMCAPTURE or the hide-and-reshow Capture guard.
        // It therefore sends no mask rectangles, and `popup` is unread here.
        // (ARCHITECTURE.md#capture-and-masking).
        Command::RequestLookup {
            id,
            point,
            popup: _,
        } => {
            let _ = x.trigger_tx.send(Trigger {
                kind: TriggerKind::Hover(Hover {
                    at: point,
                    mask: CaptureMask::NONE,
                }),
                id,
            });
            None
        }
        // Windows excludes its popup at the OS level. The hide flag has no effect.
        // WDA_EXCLUDEFROMCAPTURE or the capture guard handles it.
        Command::RequestSentence {
            id,
            anchor,
            orientation,
            hide_popup: _,
        } => {
            sentence_send_feedback(
                id,
                x.trigger_tx.send(Trigger {
                    kind: TriggerKind::Sentence(SentenceProbe {
                        anchor,
                        orientation,
                        mask: CaptureMask::NONE,
                    }),
                    id,
                }),
            )
        }
        Command::RequestDrillDown { id, text } => {
            let _ = x.trigger_tx.send(Trigger {
                kind: TriggerKind::DrillDown(text),
                id,
            });
            None
        }
        Command::RequestReload { id } => {
            let _ = x.trigger_tx.send(Trigger {
                kind: TriggerKind::Reload(Box::new(worker_settings(x.live, x.dicts))),
                id,
            });
            None
        }
        Command::RequestAnalysis { generation, texts } => {
            x.analysis.request(generation, texts);
            None
        }
        Command::ShowPopup {
            presentation,
            anchor,
            scroll,
            show_back,
        } => {
            match show_presentation(
                x.popup,
                x.renderer,
                x.live.max_height_percent,
                x.live.max_width_percent,
                SceneInputs {
                    presentation: &presentation,
                    theme: x.theme,
                    show_back,
                    side_panel: x.live.side_panel,
                    render: x.live.popup.render_settings(),
                    selection: controller.selection(),
                },
                anchor,
                scroll,
            ) {
                Ok((rect, content_h, view_h)) => Some(Event::PopupPlaced {
                    rect,
                    content_h,
                    view_h,
                }),
                Err(e) => {
                    eprintln!("chibipop: showing the popup failed: {e:#}");
                    Some(Event::PopupPlaceFailed)
                }
            }
        }
        Command::RepaintPopup { scroll, show_back } => {
            let mut feedback = None;
            if let Some(view) = controller.popup() {
                let selection = controller.selection();
                match x.renderer.paint(
                    SceneInputs {
                        presentation: view.presentation,
                        theme: x.theme,
                        show_back,
                        side_panel: x.live.side_panel,
                        render: x.live.popup.render_settings(),
                        selection,
                    },
                    scroll,
                ) {
                    Ok(()) => {
                        if *x.pointer_buttons != 0 {
                            if let Some(local) = *x.last_pointer {
                                let text = popup_text_hit(x.renderer, local, scroll);
                                if text != *x.last_pointer_text {
                                    *x.last_pointer_text = text;
                                    feedback = Some(Event::PointerMoved { local, text });
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("chibipop: repainting the popup failed: {e:#}");
                    }
                }
            }
            feedback
        }
        Command::HidePopup => {
            let _ = x.popup.hide();
            if let Some(b) = x.anki_button {
                b.hide();
            }
            if let Some(ov) = x.overlay {
                ov.hide();
            }
            None
        }
        Command::ShowScanOverlay { rects } => {
            if let Some(ov) = x.overlay {
                if let Err(e) = ov.show_rects(&rects, x.theme) {
                    eprintln!("chibipop: showing the scan overlay failed: {e:#}");
                }
            }
            None
        }
        Command::SyncAnkiButton => {
            sync_anki_button(x.anki_button, controller.popup(), x.theme);
            None
        }
        Command::SetScrollArmed(armed) => {
            Hooks::set_scroll_armed(armed);
            None
        }
        Command::SetClickArmed(armed) => {
            // Keep the low-level hook armed through a captured drag outside
            // the popup. The release clears `pointer_buttons` before this
            // command runs.
            Hooks::set_click_armed(armed || *x.pointer_buttons != 0);
            None
        }
        Command::SetDragging(dragging) => {
            if dragging {
                x.popup.capture_pointer();
            } else {
                x.popup.release_pointer();
            }
            None
        }
        Command::SetAddArmed(armed) => {
            Hooks::set_add_armed(armed);
            None
        }
        Command::SetBackArmed(armed) => {
            Hooks::set_back_armed(armed);
            None
        }
        Command::DiscardScroll => {
            Hooks::discard_scroll();
            None
        }
        Command::SetCursorShape { local, scroll } => {
            let clickable = x.renderer.hit_test(local.x, local.y, scroll).is_some();
            if clickable {
                if let Ok(cur) = unsafe { LoadCursorW(None, IDC_HAND) } {
                    unsafe { SetCursor(Some(cur)) };
                }
            }
            None
        }
        Command::CheckDupes { generation, exprs } => {
            let (cached_dupes, uncached, _) = partition_dupes(exprs, x.dupe_cache);
            if uncached.is_empty() {
                // The cache answers every reference.
                // Do not start a thread or open a connection.
                let _ = x.anki_tx.send(AnkiDupeResult {
                    gen: generation,
                    checked: Vec::new(),
                    dupes: Some(cached_dupes),
                });
                // SAFETY: This call wakes the main loop.
                unsafe {
                    let _ = PostThreadMessageW(x.main_tid, WM_APP_ANKI, WPARAM(0), LPARAM(0));
                }
                return None;
            }
            let url = x.live.anki_url.clone();
            let deck = x.live.anki_deck.clone();
            let model = x.live.anki_model.clone();
            let field_map = x.live.anki_field_map.clone();
            let tx = x.anki_tx.clone();
            let main_tid = x.main_tid;
            thread::spawn(move || {
                let refs: Vec<&str> = uncached.iter().map(|s| s.as_str()).collect();
                // The Controller replaces its duplicate set. It does not merge it.
                let dupes = match anki::find_duplicates(&url, &deck, &model, &refs, &field_map) {
                    Ok(found) => {
                        let mut all = cached_dupes;
                        all.extend(found);
                        Some(all)
                    }
                    Err(e) => {
                        eprintln!("chibipop: dupe check failed: {e:#}");
                        None
                    }
                };
                let _ = tx.send(AnkiDupeResult {
                    gen: generation,
                    checked: uncached,
                    dupes,
                });
                // SAFETY: This call wakes the main loop.
                unsafe {
                    let _ = PostThreadMessageW(main_tid, WM_APP_ANKI, WPARAM(0), LPARAM(0));
                }
            });
            None
        }
        Command::AddNote { expr, fields } => {
            // The screenshot-on-add seam receives the already-authorized
            // command payload. The popup may have moved to another Card by
            // the time the platform performs the screenshot.
            //
            // Do not do the OS half here. The selector owns a nested
            // `GetMessageW` pump, and a pump inside a Command batch would
            // re-enter `drive` halfway through the batch. Park the plan, and
            // let the loop drain it after the batch ends.
            let root = crate::action::screenshot::save_root(&x.cfg.actions.screenshot, x.exe_dir);
            let anki_connected = controller.anki().is_some_and(|anki| anki.connected);
            let pending = crate::shot::plan_add(
                &expr,
                &fields,
                x.cfg,
                &root,
                crate::shot::epoch_secs(),
            )
            .map(|plan| PendingShot { plan, anki_connected });
            if pending.is_some() {
                *x.pending_shot = pending;
                return None;
            }
            spawn_add_note(expr, fields, x.live, x.add_tx, x.main_tid);
            None
        }
        Command::LogLookup {
            headword,
            match_len,
        } => {
            println!("{headword}  match={match_len}");
            None
        }
        Command::WarnLookupFailed(msg) => {
            eprintln!("chibipop: hover lookup failed: {msg}");
            None
        }
        Command::WarnScrollCaptured { seconds } => {
            eprintln!(
                "chibipop: the wheel has been captured for {seconds}s (SCROLL_ARMED). If your \
                 scroll wheel is not working elsewhere, this is why - move the cursor off \
                 the popup, or set scroll_popup = false."
            );
            None
        }
        Command::OpenUrl(url) => {
            open_url(&url);
            None
        }
        Command::OpenSettings => {
            *x.want_settings = true;
            None
        }
        Command::Exit => {
            // The main thread handles this Command.
            unsafe { PostQuitMessage(0) };
            None
        }
    }
}

/// Sends a glossary citation to the default browser.
///
/// The settings window also calls `ShellExecuteW` with the `open` verb to
/// open a plugin directory. `layout::link_action` allows only `http`/`https`
/// because the URL comes from a Dictionary file. The shell can start any
/// other scheme.
fn open_url(url: &str) {
    let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is NUL-terminated UTF-16 and stays valid for this call.
    // The OS reads it only. A URL that it cannot open causes no effect.
    unsafe {
        let _ = windows::Win32::UI::Shell::ShellExecuteW(
            None,
            windows::core::w!("open"),
            windows::core::PCWSTR(wide.as_ptr()),
            windows::core::PCWSTR::null(),
            windows::core::PCWSTR::null(),
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        );
    }
}

/// Selects the palette by name and then sets the font.
fn theme_from_config(popup: &crate::config::PopupConfig) -> Theme {
    let mut theme = match popup.theme.as_str() {
        "light" => Theme::light(),
        _ => Theme::dark(),
    };
    theme.font_name = popup.font.clone();
    let css_path = crate::paths::beside_exe("popup.css");
    if let Ok(css) = std::fs::read_to_string(&css_path) {
        let errors = crate::ui::css::parse(&css, &mut theme);
        for e in &errors {
            eprintln!("chibipop: popup.css:{}: {}", e.line, e.message);
        }
    }
    theme
}

/// Stores settings that `run` reads from the config.
struct LiveSettings {
    popup: crate::config::PopupConfig,
    present_cfg: PresentConfig,
    scan_display: ScanDisplay,
    max_ocr_passes: u8,
    prefer_vertical: bool,
    capture: CaptureSize,
    scan_alphanumeric: bool,
    language: String,
    exclude_from_capture: bool,
    show_lookup_log: bool,
    max_height_percent: i32,
    max_width_percent: i32,
    scroll_popup: bool,
    side_panel: bool,
    summary_chars: usize,
    anki_enabled: bool,
    anki_url: String,
    anki_deck: String,
    anki_model: String,
    anki_field_map: Vec<crate::config::FieldMapping>,
    sentence_mode: crate::config::SentenceMode,
    static_region: Option<PhysRect>,
    static_region_key: String,
    show_static_overlay: bool,
    trigger_mode: crate::config::TriggerMode,
    trigger_key: String,
    anki_add_key: String,
    notify_on_add: bool,
    per_character_lookup: bool,
    actions_screenshot_hotkey: String,
    actions_ocr_clipboard_hotkey: Option<String>,
    include_dictionary_name: bool,
    first_dict_only: bool,
    selection_buttons: SelectionButtons,
    selection_separator: SelectionSeparator,
    triple_click: TripleClick,
}

impl LiveSettings {
    /// Returns the static-region outline rectangle when all conditions allow it.
    ///
    /// The sentence mode must be `Static`.
    /// The outline must be enabled.
    /// The user must have drawn a region.
    /// Every show or hide path calls this function, so all paths use the same decision.
    fn static_overlay_region(&self) -> Option<PhysRect> {
        if self.sentence_mode == crate::config::SentenceMode::Static && self.show_static_overlay {
            self.static_region
        } else {
            None
        }
    }
}

/// Builds live settings from the config after each change.
fn derive(cfg: &Config) -> LiveSettings {
    LiveSettings {
        popup: cfg.popup.clone(),
        // The config has no Dictionary identities yet.
        // Resolve enabled lists from the names in the config only. Do not append
        // installed Dictionaries. Callers with identities resolve `present_cfg` again.
        present_cfg: cfg.present_config(&[]),
        scan_display: ScanDisplay {
            captures: cfg.debug.show_scan_region,
            highlight: cfg.popup.highlight_match,
        },
        max_ocr_passes: cfg.ocr.max_ocr_passes,
        prefer_vertical: cfg.ocr.prefer_vertical,
        capture: CaptureSize {
            w: cfg.ocr.capture_width,
            h: cfg.ocr.capture_height,
        },
        scan_alphanumeric: cfg.ocr.scan_alphanumeric,
        language: cfg.ocr.language.clone(),
        exclude_from_capture: cfg.popup.exclude_from_capture,
        show_lookup_log: cfg.debug.show_lookup_log,
        max_height_percent: i32::from(cfg.popup.max_height_percent),
        max_width_percent: i32::from(cfg.popup.max_width_percent),
        scroll_popup: cfg.popup.scroll_popup,
        side_panel: cfg.popup.side_panel,
        summary_chars: cfg.popup.summary_chars,
        anki_enabled: cfg.anki.enabled,
        anki_url: cfg.anki.url.clone(),
        anki_deck: cfg.anki.deck.clone(),
        anki_model: cfg.anki.model.clone(),
        anki_field_map: if cfg.anki.field_map.is_empty() {
            crate::config::AnkiConfig::default().field_map
        } else {
            cfg.anki.field_map.clone()
        },
        sentence_mode: cfg.anki.sentence_mode,
        static_region: cfg.anki.static_region.map(|a| PhysRect {
            x: a[0],
            y: a[1],
            w: a[2],
            h: a[3],
        }),
        static_region_key: cfg.anki.static_region_key.clone(),
        show_static_overlay: cfg.anki.show_static_overlay,
        trigger_mode: cfg.trigger.mode,
        trigger_key: cfg.trigger.trigger_key.clone(),
        anki_add_key: cfg.anki.add_key.clone(),
        notify_on_add: cfg.anki.notify_on_add,
        per_character_lookup: cfg.trigger.per_character_lookup,
        actions_screenshot_hotkey: cfg.actions.screenshot.hotkey.clone(),
        actions_ocr_clipboard_hotkey: cfg
            .actions
            .ocr_clipboard
            .as_ref()
            .and_then(|action| action.hotkey.clone()),
        include_dictionary_name: cfg.anki.include_dictionary_name,
        first_dict_only: cfg.anki.first_dict_only,
        selection_buttons: cfg.anki.selection_buttons,
        selection_separator: cfg.anki.selection_separator,
        triple_click: cfg.anki.triple_click,
    }
}

/// Builds the settings that the Worker reloads.
fn worker_settings(live: &LiveSettings, dicts: &[DictInfo]) -> WorkerSettings {
    WorkerSettings {
        max_passes: live.max_ocr_passes,
        upscale: crate::text::UPSCALE,
        prefer_vertical: live.prefer_vertical,
        capture: live.capture,
        scan_alphanumeric: live.scan_alphanumeric,
        language: live.language.clone(),
        present_cfg: live.present_cfg.clone(),
        scan_display: live.scan_display,
        sentence_mode: live.sentence_mode,
        static_region: live.static_region,
        dicts: dicts.to_vec(),
    }
}

/// Updates the enabled terms list after the Dictionary identities are known.
///
/// `Config::present_config` appends every installed Dictionary that the config
/// does not name. The first `Worker::spawn` call has no identities, so it
/// resolves only the names in the config. Update `present_cfg` now so a new
/// session searches the right Dictionaries on its first lookup. Do not wait
/// for the first reload. If the list did not change, do nothing.
fn rescope_lookups(
    live: &mut LiveSettings,
    cfg: &Config,
    dicts: &[DictInfo],
    trigger_tx: &mpsc::Sender<Trigger>,
) {
    let resolved = cfg.present_config(dicts);
    if resolved == live.present_cfg {
        return;
    }
    println!(
        "chibipop: {} searches {} of {} dictionary/ies",
        cfg.ocr.language,
        resolved.terms.len(),
        dicts.len(),
    );
    live.present_cfg = resolved;
    let _ = trigger_tx.send(Trigger {
        kind: TriggerKind::Reload(Box::new(worker_settings(live, dicts))),
        id: RequestId(0),
    });
}

/// Returns true when at least one window needs the Capture guard.
/// `None` means that the window does not exist.
fn capture_guard_needed(
    popup: CaptureExclusion,
    overlay: Option<CaptureExclusion>,
    button: Option<CaptureExclusion>,
) -> bool {
    popup.needs_capture_guard()
        || overlay.is_some_and(CaptureExclusion::needs_capture_guard)
        || button.is_some_and(CaptureExclusion::needs_capture_guard)
}

/// Applies live settings to all active Windows surfaces.
#[allow(clippy::too_many_arguments)]
fn apply_live(
    live: &LiveSettings,
    popup: &Popup,
    overlay: Option<&Overlay>,
    button: Option<&AnkiButton>,
    sr_overlay: Option<&StaticRegionOverlay>,
    theme: &mut Theme,
    capture_guard_active: &AtomicBool,
) {
    capture_guard_active.store(true, Ordering::SeqCst);
    popup.set_capture_exclusion(live.exclude_from_capture);
    if let Some(o) = overlay {
        o.set_capture_exclusion(live.exclude_from_capture);
    }
    if let Some(b) = button {
        b.set_capture_exclusion(live.exclude_from_capture);
        if !live.anki_enabled {
            b.hide();
        }
    }
    if let Some(sr) = sr_overlay {
        sr.set_capture_exclusion(live.exclude_from_capture);
        match live.static_overlay_region() {
            Some(region) => {
                if let Err(e) = sr.show(region) {
                    eprintln!("chibipop: static overlay: {e:#}");
                }
            }
            None => sr.hide(),
        }
    }
    capture_guard_active.store(
        capture_guard_needed(
            popup.capture_exclusion(),
            overlay.map(Overlay::capture_exclusion),
            button.map(AnkiButton::capture_exclusion),
        ),
        Ordering::SeqCst,
    );
    *theme = theme_from_config(&live.popup);
    let alpha = (theme.opacity * 255.0).round().clamp(0.0, 255.0) as u8;
    popup.set_alpha(alpha);
    if live.show_lookup_log {
        crate::ui::console::show();
    } else {
        crate::ui::console::hide();
    }
    Hooks::set_mode(live.trigger_mode);
    if let Some(vk) = crate::config::parse_trigger_key(&live.trigger_key) {
        Hooks::set_trigger_key(vk);
    }
    if let Some(vk) = crate::config::parse_trigger_key(&live.anki_add_key) {
        Hooks::set_add_hotkey(vk);
    }
    if let Some((vk, mods)) = crate::config::parse_hotkey(&live.actions_screenshot_hotkey) {
        Hooks::set_action_hotkey(0, vk, mods);
    }
    match crate::config::parse_trigger_key(&live.static_region_key) {
        Some(vk) => Hooks::set_action_hotkey(1, vk, 0),
        None => Hooks::set_action_hotkey(1, 0, 0),
    }
    match live
        .actions_ocr_clipboard_hotkey
        .as_deref()
        .and_then(crate::config::parse_trigger_key)
    {
        Some(vk) => Hooks::set_action_hotkey(2, vk, 0),
        None => Hooks::set_action_hotkey(2, 0, 0),
    }
}

/// Registers the action when a valid key exists.
fn sync_ocr_clipboard_action(registry: &mut crate::action::ActionRegistry, hotkey: Option<&str>) {
    if hotkey.and_then(crate::config::parse_trigger_key).is_some() {
        registry.register_at(
            2,
            Box::new(crate::action::ocr_clipboard::OcrClipboardAction),
        );
    }
}

/// Saves the config while the pump stays responsive.
fn save_in_background(
    prev: &mut Option<thread::JoinHandle<()>>,
    cfg: Config,
    path: PathBuf,
    tx: mpsc::Sender<Result<()>>,
    main_tid: u32,
) {
    join_save(prev);
    *prev = Some(thread::spawn(move || {
        let _ = tx.send(cfg.save(&path));
        // SAFETY: This call wakes the main loop.
        unsafe {
            let _ = PostThreadMessageW(main_tid, WM_APP_SAVED, WPARAM(0), LPARAM(0));
        }
    }));
}

/// Save a newly selected fixed target without changing another screenshot target.
///
/// Load the latest file first so a settings window cannot erase fields that
/// changed after it opened. Save the file before updating the live config.
fn persist_screenshot_target(
    cfg: &mut Config,
    target: &crate::action::selection::SelectionTarget,
    config_path: &Path,
    save_job: &mut Option<thread::JoinHandle<()>>,
) -> Result<()> {
    let mode = cfg.actions.screenshot.capture_mode;
    match (mode, target) {
        (
            crate::config::ScreenshotMode::FixedRegion,
            crate::action::selection::SelectionTarget::Region(rect),
        ) => {
            join_save(save_job);
            let mut latest = crate::config::load_or_create(config_path)?;
            if latest.actions.screenshot.capture_mode == mode
                && latest.actions.screenshot.fixed_region.is_none()
            {
                latest.actions.screenshot.fixed_region = Some([rect.x, rect.y, rect.w, rect.h]);
                latest.save(config_path)?;
                cfg.actions.screenshot.fixed_region = latest.actions.screenshot.fixed_region;
            }
        }
        (
            crate::config::ScreenshotMode::FixedWindow,
            crate::action::selection::SelectionTarget::Window { target, .. },
        ) => {
            join_save(save_job);
            let mut latest = crate::config::load_or_create(config_path)?;
            if latest.actions.screenshot.capture_mode == mode
                && latest.actions.screenshot.fixed_window.is_none()
            {
                latest.actions.screenshot.fixed_window = Some(target.clone());
                latest.save(config_path)?;
                cfg.actions.screenshot.fixed_window = latest.actions.screenshot.fixed_window.clone();
            }
        }
        _ => {}
    }
    Ok(())
}

/// Joins the previous save before a new save starts.
fn join_save(job: &mut Option<thread::JoinHandle<()>>) {
    if let Some(h) = job.take() {
        let _ = h.join();
    }
}

/// Returns a replacement language when the configured language is unavailable.
fn startup_language(
    configured: &str,
    fallback: &str,
    available: impl FnOnce() -> bool,
) -> Option<String> {
    if configured.eq_ignore_ascii_case(fallback) || available() {
        None
    } else {
        Some(fallback.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PopupConfig;
    #[test]
    fn dupe_partition_uses_cached_results_and_deduplicates_refs() {
        let cache = HashMap::from([("宿舎".to_string(), true), ("駅".to_string(), false)]);
        let (dupes, uncached, cached_any) = partition_dupes(
            vec!["宿舎".into(), "宿舎".into(), "駅".into(), "猫".into()],
            &cache,
        );
        assert_eq!(HashSet::from(["宿舎".to_string()]), dupes);
        assert_eq!(vec!["猫".to_string()], uncached);
        assert!(cached_any);
    }

    #[test]
    fn anki_detect_rejects_same_model_old_generation() {
        let result = AnkiDetect::fields(1, "http://127.0.0.1:8765".into(), "Basic".into(), vec![]);
        assert!(anki_detect_matches(
            1,
            "http://127.0.0.1:8765",
            "Basic",
            &result
        ));
        assert!(!anki_detect_matches(
            2,
            "http://127.0.0.1:8765",
            "Basic",
            &result
        ));
    }

    #[test]
    fn anki_detect_rejects_changed_url() {
        let result = AnkiDetect::fields(3, "http://127.0.0.1:8765".into(), "Basic".into(), vec![]);
        assert!(!anki_detect_matches(
            3,
            "http://127.0.0.1:8766",
            "Basic",
            &result
        ));
    }

    #[test]
    fn stale_anki_test_status_is_rejected() {
        let status = SettingsStatus::anki(3, "AnkiConnect is reachable.".into());
        assert!(status.matches(3));
        assert!(!status.matches(4));
    }

    #[test]
    fn non_anki_status_is_always_current() {
        let status = SettingsStatus::any("You already have the latest version.".into());
        assert!(status.matches(3));
        assert!(status.matches(4));
    }

    /// Starts with the default popup section and replaces the two fields that
    /// these tests check. A new core field therefore cannot get a value that
    /// the default config does not contain.
    fn popup_config(theme: &str, font: &str) -> PopupConfig {
        PopupConfig {
            theme: theme.to_string(),
            font: font.to_string(),
            ..Config::default().popup
        }
    }

    /// I1: passes the font to Theme.
    #[test]
    fn a_non_default_font_reaches_the_theme() {
        let theme = theme_from_config(&popup_config("dark", "Noto Sans JP"));
        assert_eq!("Noto Sans JP", theme.font_name);
    }

    #[test]
    fn theme_selection_by_name_is_unaffected_by_the_font_field() {
        assert_eq!(
            Theme::light().background,
            theme_from_config(&popup_config("light", "X")).background
        );
        assert_eq!(
            Theme::dark().background,
            theme_from_config(&popup_config("anything-else", "X")).background
        );
    }

    #[test]
    fn derive_carries_every_popup_field() {
        let mut cfg = Config::default();
        cfg.popup.max_width_percent = 33;
        cfg.popup.max_height_percent = 44;
        cfg.popup.summary_chars = 55;
        cfg.popup.side_panel = true;
        cfg.anki.enabled = true;
        cfg.anki.deck = "テスト".to_string();
        let live = derive(&cfg);
        assert_eq!(33, live.max_width_percent);
        assert_eq!(44, live.max_height_percent);
        assert_eq!(55, live.summary_chars);
        assert!(live.side_panel);
        assert!(live.anki_enabled);
        assert_eq!("テスト", live.anki_deck);
    }

    #[test]
    fn derive_carries_the_capture_settings() {
        let mut cfg = Config::default();
        cfg.ocr.capture_width = 320;
        cfg.ocr.capture_height = 240;
        cfg.ocr.scan_alphanumeric = false;
        let live = derive(&cfg);
        assert_eq!(CaptureSize { w: 320, h: 240 }, live.capture);
        assert!(!live.scan_alphanumeric);
    }

    /// Covers the main capture settings.
    #[test]
    fn worker_settings_carries_the_capture_settings() {
        let mut cfg = Config::default();
        cfg.ocr.capture_width = 640;
        cfg.ocr.capture_height = 480;
        cfg.ocr.scan_alphanumeric = false;
        cfg.ocr.max_ocr_passes = 3;
        let out = worker_settings(&derive(&cfg), &[]);
        assert_eq!(CaptureSize { w: 640, h: 480 }, out.capture);
        assert!(!out.scan_alphanumeric);
        assert_eq!(3, out.max_passes);
    }

    #[test]
    fn worker_settings_carries_the_language() {
        let mut cfg = Config::default();
        cfg.ocr.language = "zh-Hant".to_string();
        let live = derive(&cfg);
        assert_eq!("zh-Hant", live.language);
        assert_eq!("zh-Hant", worker_settings(&live, &[]).language);
    }

    /// The search includes an installed Dictionary when no configured list names it.
    /// The first Worker read supplies its name.
    /// `Worker::spawn` cannot resolve the enabled list before that read, so the code
    /// resolves the list again.
    /// Without this step, a new session searches only configured Dictionaries until
    /// a reload.
    /// A unit test cannot drive the pump, so this test checks the decision-and-push
    /// function directly.
    #[test]
    fn a_fresh_worker_is_told_the_split_once_the_names_are_known() {
        let mut cfg = Config::default();
        cfg.dictionaries.terms = vec!["大辞林　第四版".to_string()];
        cfg.dictionaries.terms_disabled = vec!["Jitendex.org [2026-07-09]".to_string()];
        let mut live = derive(&cfg);
        assert_eq!(
            vec!["大辞林　第四版".to_string()],
            live.present_cfg.terms,
            "an empty library resolves to the listed names alone"
        );
        let dicts = vec![
            DictInfo { dict_id: 1, name: "大辞林　第四版".to_string() },
            DictInfo { dict_id: 2, name: "Jitendex.org [2026-07-09]".to_string() },
            DictInfo { dict_id: 3, name: "新明解国語辞典".to_string() },
        ];
        let (tx, rx) = mpsc::channel::<Trigger>();

        rescope_lookups(&mut live, &cfg, &dicts, &tx);

        assert_eq!(
            vec!["大辞林　第四版".to_string(), "新明解国語辞典".to_string()],
            live.present_cfg.terms,
            "the enabled name, then the installed dictionary neither list mentions"
        );
        let sent = rx
            .try_recv()
            .expect("the reload must have reached the worker");
        match sent.kind {
            TriggerKind::Reload(settings) => {
                assert_eq!(live.present_cfg, settings.present_cfg);
                assert_eq!(3, settings.dicts.len(), "the reload carries the identities");
            }
            _ => panic!("a rescope is a reload"),
        }
    }

    /// Do nothing when no list changes. A config that names the installed
    /// Dictionary in every list resolves to itself, so no reload or log line occurs.
    #[test]
    fn a_worker_whose_scope_did_not_change_is_left_alone() {
        let mut cfg = Config::default();
        cfg.dictionaries.terms = vec!["Jitendex.org".to_string()];
        cfg.dictionaries.pitch = vec!["Jitendex.org".to_string()];
        let mut live = derive(&cfg);
        let before = live.present_cfg.clone();
        let dicts = vec![DictInfo {
            dict_id: 1,
            name: "Jitendex.org".to_string(),
        }];
        let (tx, rx) = mpsc::channel::<Trigger>();

        rescope_lookups(&mut live, &cfg, &dicts, &tx);

        assert_eq!(before, live.present_cfg);
        assert!(rx.try_recv().is_err(), "an unchanged scope must not reload");
    }

    /// Step 3b: carries the three input settings.
    #[test]
    fn derive_carries_the_three_input_settings() {
        let mut cfg = Config::default();
        cfg.trigger.mode = crate::config::TriggerMode::HoldKey;
        cfg.trigger.trigger_key = "f8".to_string();
        cfg.anki.add_key = "f9".to_string();
        let live = derive(&cfg);
        assert_eq!(crate::config::TriggerMode::HoldKey, live.trigger_mode);
        assert_eq!("f8", live.trigger_key);
        assert_eq!("f9", live.anki_add_key);
    }

    #[test]
    fn derive_carries_the_ocr_clipboard_key() {
        let mut cfg = Config::default();
        cfg.actions.ocr_clipboard = Some(crate::config::OcrClipboardConfig {
            hotkey: Some("f9".to_string()),
            hotkey_linux: None,
        });

        assert_eq!(
            Some("f9".to_string()),
            derive(&cfg).actions_ocr_clipboard_hotkey
        );
    }

    #[test]
    fn three_excluded_windows_leave_the_guard_disarmed() {
        assert!(!capture_guard_needed(
            CaptureExclusion::Excluded,
            Some(CaptureExclusion::Excluded),
            Some(CaptureExclusion::Excluded),
        ));
    }

    #[test]
    fn the_popup_alone_can_arm_the_guard() {
        assert!(capture_guard_needed(
            CaptureExclusion::DeliberatelyNotExcluded,
            Some(CaptureExclusion::Excluded),
            Some(CaptureExclusion::Excluded),
        ));
    }

    /// The overlay and button can diverge from the popup.
    #[test]
    fn an_overlay_the_os_refused_arms_the_guard_alone() {
        assert!(capture_guard_needed(
            CaptureExclusion::Excluded,
            Some(CaptureExclusion::AttemptFailed),
            Some(CaptureExclusion::Excluded),
        ));
    }

    #[test]
    fn a_button_the_os_refused_arms_the_guard_alone() {
        assert!(capture_guard_needed(
            CaptureExclusion::Excluded,
            Some(CaptureExclusion::Excluded),
            Some(CaptureExclusion::AttemptFailed),
        ));
    }

    #[test]
    fn a_window_that_was_never_created_cannot_need_the_guard() {
        assert!(!capture_guard_needed(
            CaptureExclusion::Excluded,
            None,
            None
        ));
    }

    #[test]
    fn the_guard_tracks_a_live_exclusion_toggle_in_both_directions() {
        let off = CaptureExclusion::from_attempt(false, false);
        assert!(capture_guard_needed(off, Some(off), Some(off)));
        let on = CaptureExclusion::from_attempt(true, true);
        assert!(!capture_guard_needed(on, Some(on), Some(on)));
    }

    #[test]
    fn derive_carries_per_character_lookup() {
        let mut cfg = Config::default();
        cfg.trigger.per_character_lookup = true;
        assert!(derive(&cfg).per_character_lookup);
        assert!(
            !derive(&Config::default()).per_character_lookup,
            "must default off"
        );
    }

    #[test]
    fn a_startup_language_with_no_pack_falls_back_to_the_default() {
        assert_eq!(
            Some("ja".to_string()),
            startup_language("ko", "ja", || false)
        );
    }

    #[test]
    fn an_installed_startup_language_is_left_alone() {
        assert_eq!(None, startup_language("ko", "ja", || true));
    }

    /// Avoid a substitution loop when the configured language equals the fallback.
    #[test]
    fn the_default_language_never_substitutes_itself() {
        assert_eq!(None, startup_language("JA", "ja", || false));
    }

    /// Do not call WinRT for the default language.
    #[test]
    fn the_default_language_never_asks_windows() {
        let mut asked = false;
        let got = startup_language("ja", "ja", || {
            asked = true;
            false
        });
        assert_eq!(None, got);
        assert!(!asked);
    }

    fn di(id: i64, name: &str) -> crate::present::DictInfo {
        crate::present::DictInfo {
            dict_id: id,
            name: name.to_string(),
        }
    }

    struct ScratchDir(PathBuf);

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn edit_scratch(test_name: &str) -> (PathBuf, ScratchDir) {
        let dir = std::env::temp_dir()
            .join("chibipop_apply_edit")
            .join(format!("t_{}_{test_name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), ScratchDir(dir))
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/yomitan")
            .join(name)
    }

    /// Creates a WAL database, as `build-dict` does.
    fn built_db(dir: &Path, library: &Path) -> PathBuf {
        std::fs::create_dir_all(library).unwrap();
        std::fs::copy(fixture("terms.zip"), library.join("terms.zip")).unwrap();
        let out = dir.join("chibipop.sqlite");
        crate::dict::build::build(&[library.join("terms.zip")], &[], &out, &|_| {}).unwrap();
        out
    }

    fn dict_rows(db: &Path) -> Vec<(i64, String)> {
        let conn = rusqlite::Connection::open(db).unwrap();
        let mut stmt = conn
            .prepare("SELECT dict_id, name FROM dict ORDER BY dict_id")
            .unwrap();
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        rows.map(std::result::Result::unwrap).collect()
    }

    fn entry_count(db: &Path) -> i64 {
        rusqlite::Connection::open(db)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM entry", [], |r| r.get(0))
            .unwrap()
    }

    fn staged_form(adds: &[(&Path, &str)], removes: &[&str]) -> SettingsForm {
        let mut form = settings::from_config(&Config::default(), &[]);
        for (source, name) in adds {
            form.staged_adds.push(crate::settings::StagedAdd {
                source: source.to_path_buf(),
                name: (*name).to_string(),
            });
        }
        form.staged_removes = removes.iter().map(|n| (*n).to_string()).collect();
        form
    }

    fn lib_of(entries: &[(&str, &str)]) -> Library {
        Library {
            entries: entries
                .iter()
                .map(|(file, name)| crate::library::Entry {
                    file: (*file).to_string(),
                    name: (*name).to_string(),
                    roles: Roles::only(&[Role::Terms]),
                })
                .collect(),
        }
    }

    fn report_of(added: &[&str], removed: &[&str], failed: &[&str]) -> EditReport {
        EditReport {
            added: added
                .iter()
                .map(|s| ((*s).to_string(), Roles::only(&[Role::Terms])))
                .collect(),
            removed: removed.iter().map(|s| (*s).to_string()).collect(),
            freq_added: Vec::new(),
            freq_removed: Vec::new(),
            failed: failed.iter().map(|s| (*s).to_string()).collect(),
            dicts: Vec::new(),
        }
    }

    /// Do not write to a database that lacks WAL mode.
    #[test]
    fn the_writer_refuses_a_database_that_is_not_in_wal_mode() {
        let (dir, _guard) = edit_scratch("legacy_mode");
        let legacy = dir.join("legacy.sqlite");
        let conn = rusqlite::Connection::open(&legacy).unwrap();
        conn.execute_batch("PRAGMA journal_mode = DELETE; CREATE TABLE t(x);")
            .unwrap();
        drop(conn);

        let err = open_writer(&legacy).expect_err("a delete-mode file must be refused");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("delete"),
            "the message must name the mode found: {msg}"
        );
        assert!(msg.contains("WAL"), "the message must name WAL: {msg}");
    }

    #[test]
    fn the_writer_opens_a_wal_database_for_writing() {
        let (dir, _guard) = edit_scratch("wal_ok");
        let db = built_db(&dir, &dir.join("library"));
        let conn = open_writer(&db).expect("a WAL database must open for writing");
        conn.execute_batch("INSERT INTO meta (k, v) VALUES ('t6', '1')")
            .expect("the writer must be able to write");
    }

    /// Do not create a database for a missing path.
    #[test]
    fn the_writer_never_creates_a_missing_database() {
        let (dir, _guard) = edit_scratch("no_create");
        let missing = dir.join("absent.sqlite");
        assert!(
            open_writer(&missing).is_err(),
            "a missing database must not open"
        );
        assert!(!missing.exists(), "opening must not create the file");
    }

    /// Absolute IDs would produce an incorrect count.
    #[test]
    fn progress_counts_from_the_dictionary_being_added() {
        assert_eq!(
            "progress  4997 / ?",
            rebased("progress  365000 / ?", 360004)
        );
        assert_eq!(
            Some("4,997 entries\u{2026}".to_string()),
            crate::dict::progress::friendly(&rebased("progress  365000 / ?", 360004))
        );
    }

    #[test]
    fn progress_into_an_empty_database_is_unchanged() {
        assert_eq!("progress  5000 / ?", rebased("progress  5000 / ?", 1));
    }

    #[test]
    fn a_line_that_is_not_progress_survives_rebasing() {
        assert_eq!(
            "building  creating index",
            rebased("building  creating index", 360004)
        );
        assert_eq!("progress  x / ?", rebased("progress  x / ?", 10));
    }

    /// `banks.zip` is distinct from the library's `terms.zip`.
    /// Two byte-identical archives form one Dictionary, and `Library::load`
    /// removes the duplicate. An Apply that adds the duplicate would leave the
    /// database with a Dictionary absent from the library.
    /// [`drifted`] reports this mismatch at the end of the test.
    #[test]
    fn a_mixed_frequency_apply_reapplies_in_place() {
        let (dir, _guard) = edit_scratch("mixed_frequency");
        let library = dir.join("library");
        let db = built_db(&dir, &library);
        let mut form = settings::from_config(&Config::default(), &[]);
        assert_eq!(
            Some(Roles::only(&[Role::Frequency])),
            form.stage_add(&fixture("freq.zip"))
        );
        assert_eq!(
            Some(Roles::only(&[Role::Terms])),
            form.stage_add(&fixture("banks.zip"))
        );
        let (tx, rx) = mpsc::channel::<EditMsg>();
        let report =
            apply_edits_with_frequencies(&db, &library, &form, &tx).expect("the apply must work");

        assert_eq!(
            vec![("FixtureBanks".to_string(), Roles::only(&[Role::Terms]))],
            report.added
        );
        assert_eq!(vec!["FixtureFreq".to_string()], report.freq_added);
        assert!(report.removed.is_empty());
        assert!(report.freq_removed.is_empty());
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(library.join("freq.zip").exists());
        let conn = rusqlite::Connection::open(&db).unwrap();
        let freq: i64 = conn
            .query_row(
                "SELECT freq FROM term WHERE surface = ?1",
                ["食べる"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(7, freq);
        assert_eq!(None, drifted(&library, &db).unwrap());
        assert!(rx.try_iter().any(|message| {
            matches!(message, EditMsg::Status(text) if text == "Updating frequency rankings...")
        }));
    }

    #[test]
    fn a_frequency_removal_reapplies_nulls_in_place() {
        let (dir, _guard) = edit_scratch("remove_frequency");
        let library = dir.join("library");
        let db = built_db(&dir, &library);
        std::fs::copy(fixture("freq.zip"), library.join("freq.zip")).unwrap();
        let lib = Library::load(&library).unwrap();
        let mut form = settings::with_library(settings::from_config(&Config::default(), &[]), &lib);
        form.stage_remove("FixtureFreq");

        let (tx, rx) = mpsc::channel::<EditMsg>();
        let report =
            apply_edits_with_frequencies(&db, &library, &form, &tx).expect("the removal must work");

        assert!(report.added.is_empty());
        assert!(report.removed.is_empty());
        assert_eq!(vec!["FixtureFreq".to_string()], report.freq_removed);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(!library.join("freq.zip").exists());
        assert_eq!(1, report.dicts.len());
        let conn = rusqlite::Connection::open(&db).unwrap();
        let ranked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM term WHERE freq IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(0, ranked);
        assert_eq!(None, drifted(&library, &db).unwrap());
        assert!(rx.try_iter().any(|message| {
            matches!(message, EditMsg::Status(text) if text == "Updating frequency rankings...")
        }));
    }

    #[test]
    fn a_term_only_form_does_not_mark_frequency_changes() {
        let form = staged_form(&[(&fixture("terms.zip"), "FixtureTerms")], &[]);
        assert!(!form.freq_changed);
    }

    #[test]
    fn engine_status_names_a_running_plugin() {
        let mut cfg = Config::default();
        cfg.ocr.engine = "meikiocr".into();
        cfg.plugins.enabled = vec!["meikiocr".into()];
        assert_eq!("Engine: meikiocr", engine_status_line(&cfg));
    }

    #[test]
    fn engine_status_names_the_builtin() {
        assert_eq!(
            "Engine: Built-in (Windows OCR)",
            engine_status_line(&Config::default())
        );
    }

    #[test]
    fn engine_status_names_a_missing_plugin() {
        let mut cfg = Config::default();
        cfg.ocr.engine = "meikiocr".into();
        let s = engine_status_line(&cfg);
        assert!(s.contains("meikiocr"), "{s}");
        assert!(s.contains("not found"), "{s}");
    }

    #[test]
    fn a_removal_resolves_its_row_and_its_archive() {
        let form = staged_form(&[], &["\u{5927}\u{8f9e}\u{6797}"]);
        let lib = lib_of(&[("daijirin.zip", "\u{5927}\u{8f9e}\u{6797}")]);
        let plan = plan_edits(&form, &[di(7, "\u{5927}\u{8f9e}\u{6797}")], &lib);
        assert_eq!(1, plan.removals.len());
        assert_eq!(Some(7), plan.removals[0].dict_id);
        assert_eq!(Some("daijirin.zip".to_string()), plan.removals[0].file);
    }

    /// Check that removal can name an unreadable archive by file.
    #[test]
    fn a_removal_may_name_the_archive_file() {
        let form = staged_form(&[], &["broken.zip"]);
        let plan = plan_edits(&form, &[], &lib_of(&[("broken.zip", "broken")]));
        assert_eq!(None, plan.removals[0].dict_id);
        assert_eq!(Some("broken.zip".to_string()), plan.removals[0].file);
    }

    /// A database row can remain after the library loses its archive.
    #[test]
    fn a_removal_with_no_archive_still_names_its_row() {
        let form = staged_form(&[], &["Orphan"]);
        let plan = plan_edits(&form, &[di(3, "Orphan")], &lib_of(&[]));
        assert_eq!(Some(3), plan.removals[0].dict_id);
        assert_eq!(None, plan.removals[0].file);
    }

    #[test]
    fn the_status_names_both_lists() {
        let s = edit_status(&report_of(&["New"], &["Old"], &[]));
        assert!(s.contains("New"), "{s}");
        assert!(s.contains("Old"), "{s}");
    }

    /// Include the failure name.
    #[test]
    fn the_status_names_what_failed_beside_what_worked() {
        let s = edit_status(&report_of(&["New"], &[], &["Bad: the zip is corrupt"]));
        assert!(
            s.contains("New"),
            "the applied change must still be named: {s}"
        );
        assert!(s.contains("Bad"), "the failure must be named: {s}");
        assert!(
            s.contains("the zip is corrupt"),
            "the reason must be named: {s}"
        );
    }

    #[test]
    fn a_change_that_did_nothing_says_so() {
        assert_eq!(
            "No dictionary changed.",
            edit_status(&report_of(&[], &[], &[]))
        );
    }

    /// This test covers the main Apply path.
    #[test]
    fn an_apply_adds_a_dictionary_to_the_live_database() {
        let (dir, _guard) = edit_scratch("add");
        let library = dir.join("library");
        let db = built_db(&dir, &library);
        let before = entry_count(&db);
        let form = staged_form(&[(&fixture("terms.zip"), "FixtureTerms")], &[]);

        let (tx, rx) = mpsc::channel::<EditMsg>();
        let report = apply_edits(&db, &library, &form, &tx).expect("the add must apply");
        drop(tx);

        assert_eq!(
            vec![("FixtureTerms".to_string(), Roles::only(&[Role::Terms]))],
            report.added
        );
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(2, report.dicts.len(), "{:?}", report.dicts);
        assert_eq!(2, dict_rows(&db).len());
        assert_eq!(
            before * 2,
            entry_count(&db),
            "every entry must be kept and doubled"
        );
        assert!(
            rx.try_iter().any(|m| matches!(m, EditMsg::Status(_))),
            "the edit must report progress"
        );
    }

    /// REGRESSION 1.18: preserve the reported symptom.
    #[test]
    fn an_apply_removes_a_dictionary_and_its_archive() {
        let (dir, _guard) = edit_scratch("remove");
        let library = dir.join("library");
        let db = built_db(&dir, &library);
        // Use distinct bytes. A copy of `terms.zip` would be the same
        // Dictionary under another name, and the library would collapse both.
        std::fs::copy(fixture("banks.zip"), library.join("extra.zip")).unwrap();
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(
                "INSERT INTO dict (dict_id, name, priority) VALUES (9, 'extra.zip', 8);
                 INSERT INTO entry (entry_id, dict_id, glossary) VALUES (900, 9, '[]');
                 INSERT INTO term (surface, written, reading, pos, freq, entry_id, dict_id)
                     VALUES ('x', 'x', 'x', '', NULL, 900, 9);",
            )
            .unwrap();
        }
        let kept = entry_count(&db) - 1;
        let form = staged_form(&[], &["extra.zip"]);

        let (tx, _rx) = mpsc::channel::<EditMsg>();
        let report = apply_edits(&db, &library, &form, &tx).expect("the removal must apply");

        assert_eq!(vec!["extra.zip".to_string()], report.removed);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(1, dict_rows(&db).len());
        assert_eq!(
            kept,
            entry_count(&db),
            "the other dictionary must be untouched"
        );
        assert!(
            !library.join("extra.zip").exists(),
            "the archive must be gone"
        );
        assert!(
            !library.join(".removed").exists(),
            "nothing may stay quarantined"
        );
        assert_eq!(1, report.dicts.len());
    }

    /// A frequency archive is a Dictionary that the user orders and enables.
    /// The database stores its Reported frequencies under the archive's
    /// `dict_id`. Removal deletes those records through the same `DICT_KEYED`
    /// walk as every other Dictionary-keyed table
    /// (ARCHITECTURE.md#dictionary-and-lookup). The archive adds no `entry` row.
    #[test]
    fn a_frequency_addition_owns_a_dictionary_row_and_contributes_no_entries() {
        let (dir, _guard) = edit_scratch("add_frequency");
        let library = dir.join("library");
        let db = built_db(&dir, &library);
        let before = entry_count(&db);
        let mut form = settings::from_config(&Config::default(), &[]);
        assert_eq!(
            Some(Roles::only(&[Role::Frequency])),
            form.stage_add(&fixture("freq.zip"))
        );

        let (tx, _rx) = mpsc::channel::<EditMsg>();
        let report = apply_edits_with_frequencies(&db, &library, &form, &tx)
            .expect("the frequency apply must work");

        assert!(report.added.is_empty(), "{:?}", report.added);
        assert_eq!(vec!["FixtureFreq".to_string()], report.freq_added);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(library.join("freq.zip").exists());
        assert_eq!(before, entry_count(&db));
        assert_eq!(
            vec![
                (1, "FixtureTerms".to_string()),
                (2, "FixtureFreq".to_string()),
            ],
            dict_rows(&db)
        );
        let claims: i64 = rusqlite::Connection::open(&db)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM reported_freq WHERE dict_id = 2", [], |r| r.get(0))
            .unwrap();
        assert_eq!(3, claims, "and freq.zip's three claims are stored under it");
        assert_eq!(None, drifted(&library, &db).unwrap());
    }

    /// Refuse before the library changes.
    #[test]
    fn a_legacy_database_is_refused_before_the_library_is_touched() {
        let (dir, _guard) = edit_scratch("legacy_apply");
        let library = dir.join("library");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::copy(fixture("terms.zip"), library.join("terms.zip")).unwrap();
        let db = dir.join("legacy.sqlite");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("PRAGMA journal_mode = DELETE; CREATE TABLE dict(dict_id INTEGER);")
            .unwrap();
        drop(conn);
        let form = staged_form(&[(&fixture("terms.zip"), "FixtureTerms")], &[]);

        let (tx, _rx) = mpsc::channel::<EditMsg>();
        let err = apply_edits(&db, &library, &form, &tx).expect_err("a legacy file is refused");

        assert!(format!("{err:#}").contains("WAL"), "{err:#}");
        assert_eq!(
            1,
            std::fs::read_dir(&library).unwrap().count(),
            "nothing may be imported before the refusal"
        );
    }

    /// Do not remove the last Dictionary.
    #[test]
    fn an_apply_that_would_empty_the_library_is_refused() {
        let (dir, _guard) = edit_scratch("last_one");
        let library = dir.join("library");
        let db = built_db(&dir, &library);
        let form = staged_form(&[], &["FixtureTerms"]);

        let (tx, _rx) = mpsc::channel::<EditMsg>();
        let err = apply_edits(&db, &library, &form, &tx).expect_err("the last one is refused");

        assert!(format!("{err:#}").contains("no dictionary"), "{err:#}");
        assert_eq!(1, dict_rows(&db).len(), "the database must be untouched");
        assert!(library.join("terms.zip").exists(), "the archive must stay");
    }

    /// Use actual source bytes instead of a constant.
    #[test]
    fn the_library_that_built_the_database_has_not_drifted() {
        let (dir, _guard) = edit_scratch("no_drift");
        let library = dir.join("library");
        let db = built_db(&dir, &library);

        let raw = read_source_hashes(&db)
            .unwrap()
            .expect("build-dict records what it read");
        assert!(
            raw.contains(r#""name": "terms.zip""#),
            "json.dumps spacing: {raw}"
        );
        assert_eq!(None, drifted(&library, &db).unwrap(), "the two agree");
    }

    #[test]
    fn result_routing_keeps_sentences_and_freshest_other_in_arrival_order() {
        let routed = route_results(vec![
            WorkerResult {
                id: RequestId(1),
                outcome: LookupOutcome::Hide,
            },
            WorkerResult {
                id: RequestId(2),
                outcome: LookupOutcome::Sentence(Some("first sentence".into())),
            },
            WorkerResult {
                id: RequestId(3),
                outcome: LookupOutcome::Hide,
            },
            WorkerResult {
                id: RequestId(4),
                outcome: LookupOutcome::Sentence(None),
            },
        ]);

        assert_eq!(
            vec![RequestId(2), RequestId(3), RequestId(4)],
            routed.iter().map(|result| result.id).collect::<Vec<_>>()
        );
        assert!(matches!(
            &routed[0].outcome,
            LookupOutcome::Sentence(Some(text)) if text == "first sentence"
        ));
        assert!(matches!(&routed[1].outcome, LookupOutcome::Hide));
        assert!(matches!(&routed[2].outcome, LookupOutcome::Sentence(None)));
    }

    #[test]
    fn hook_events_route_press_and_accepted_cursor_move() {
        let press = PhysPoint { x: 10, y: 20 };
        let moved = PhysPoint { x: 30, y: 40 };
        let events: Vec<Event> = route_hook_events(Some(press), Some(moved))
            .into_iter()
            .flatten()
            .collect();

        assert_eq!(
            vec![
                Event::TriggerPressed { pos: press },
                Event::CursorMoved { pos: moved },
            ],
            events
        );
    }

    #[test]
    fn a_failed_sentence_send_returns_empty_sentence_feedback() {
        let (tx, rx) = mpsc::channel::<Trigger>();
        drop(rx);
        let sent = tx.send(Trigger {
            kind: TriggerKind::Sentence(SentenceProbe {
                anchor: PhysRect {
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 1,
                },
                orientation: crate::text::layout::Orientation::Horizontal,
                mask: CaptureMask::NONE,
            }),
            id: RequestId(7),
        });

        assert_eq!(
            Some(Event::LookupResult {
                id: RequestId(7),
                outcome: LookupOutcome::Sentence(None),
            }),
            sentence_send_feedback(RequestId(7), sent)
        );
    }

    #[test]
    fn screenshot_restore_uses_the_current_popup_state() {
        let mut controller = Controller::new(controller_config(&derive(&Config::default())));
        assert!(screenshot_restore_view(&controller).is_none());

        let point = PhysPoint { x: 110, y: 110 };
        let id = controller
            .handle(Event::CursorMoved { pos: point })
            .into_iter()
            .find_map(|cmd| match cmd {
                Command::RequestLookup { id, .. } => Some(id),
                _ => None,
            })
            .expect("the first cursor move must request a lookup");
        controller.handle(Event::LookupResult {
            id,
            outcome: LookupOutcome::Ready {
                presentation: Box::new(crate::present::Presentation {
                    top: Some(crate::present::Card {
                        written: Some("猫".to_string()),
                        reading: None,
                        pos: Vec::new(),
                        freq: None,
                        blocks: Vec::new(),
                        match_len: 1,
                        pitch: Vec::new(),
                    }),
                    collapsed: Vec::new(),
                    all_cards: Vec::new(),
                    sentence: None,
                    surface: None,
                }),
                anchor: PhysRect {
                    x: 100,
                    y: 100,
                    w: 20,
                    h: 20,
                },
                orientation: crate::text::layout::Orientation::Horizontal,
                matched: None,
                scan: Vec::new(),
            },
        });
        controller.handle(Event::PopupPlaced {
            rect: PhysRect {
                x: 100,
                y: 160,
                w: 300,
                h: 200,
            },
            content_h: 200,
            view_h: 200,
        });
        assert!(screenshot_restore_view(&controller).is_some());

        controller.handle(Event::LookupResult {
            id,
            outcome: LookupOutcome::Hide,
        });
        assert!(
            screenshot_restore_view(&controller).is_none(),
            "a later Hide must prevent stale native-window restoration"
        );
    }

    /// Check that a new archive produces a drift notice.
    #[test]
    fn an_archive_the_build_never_saw_is_reported_as_drift() {
        let (dir, _guard) = edit_scratch("drifted");
        let library = dir.join("library");
        let db = built_db(&dir, &library);
        std::fs::copy(fixture("freq.zip"), library.join("freq.zip")).unwrap();

        let text = drifted(&library, &db)
            .unwrap()
            .expect("a dropped-in archive is drift");

        assert!(text.contains("freq.zip"), "{text}");
        assert!(text.contains("chibipop build-dict --library"), "{text}");
    }
}
