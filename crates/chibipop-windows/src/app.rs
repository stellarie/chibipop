//! Two threads: pump and worker.

use crate::anki;
use crate::config::Config;
use crate::controller::{
    Command, Controller, ControllerConfig, Event, PopupView, TrayAction,
};
use crate::geom::{place_popup, PhysPoint, PhysRect, ScanDisplay};
use crate::input::hooks::Hooks;
use crate::library::{Library, Pending};
use crate::lock::LibraryLock;
use crate::lookup::deconj::Deconjugator;
use crate::lookup::engine::LookupEngine;
use crate::lookup::model::Dictionary;
use crate::lookup::rules::load_rules;
use crate::lookup::sqlite::SqliteDictionary;
use crate::present::{DictInfo, Presentation, PresentConfig};
use crate::rebuild::{self, Progress};
use crate::settings::{self, SettingsForm};
use crate::text::capture::{CaptureGuard, CaptureGuardMsg, WinCapture, WM_APP_CAPTURE_GUARD};
use crate::text::layout::CaptureSize;
use crate::text::mask::CaptureMask;
use crate::text::ocr::{recogniser_available, WinrtOcr};
use crate::ui::overlay::Overlay;
use crate::ui::layout::anki_button_label;
use crate::ui::render::Renderer;
use crate::ui::settings_window::{ApplyMode, SettingsClick, SettingsOutcome, SettingsWindow};
use crate::ui::theme::Theme;
use crate::ui::tray::{Tray, TrayCommand};
use crate::ui::window::{AnkiButton, CaptureExclusion, Popup};
use crate::update;
use crate::worker::{
    Hover, Trigger, TriggerKind, Worker, WorkerParts, WorkerResult, WorkerSettings,
};
use anyhow::{anyhow, Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::collections::HashSet;
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
    DispatchMessageW, GetCursorPos, GetMessageW, IDC_HAND, IsDialogMessageW, IsWindowVisible,
    KillTimer, LoadCursorW, PostQuitMessage, PostThreadMessageW, SetCursor, SetTimer, ShowWindow,
    TranslateMessage, MSG, SW_HIDE, SW_SHOWNOACTIVATE, WM_APP, WM_KEYDOWN, WM_SYSKEYDOWN,
    WM_TIMER,
};

/// Worker pushed a result.
const WM_APP_RESULT: u32 = WM_APP + 1;

/// Dupe check finished.
const WM_APP_ANKI: u32 = WM_APP + 4;

/// Add-note finished.
const WM_APP_ADD_NOTE: u32 = WM_APP + 5;

/// Settings op finished.
const WM_APP_SETTINGS: u32 = WM_APP + 6;

/// Anki deck/model detect done.
const WM_APP_ANKI_DETECT: u32 = WM_APP + 7;

/// Background save finished.
const WM_APP_SAVED: u32 = WM_APP + 9;

/// Pending-cursor poll, ms.
const DISPATCH_TICK_MS: u32 = 20;

/// Anchor-to-popup gap.
const POPUP_GAP: i32 = 40;


/// Rebuild progress poll, ms.
const REBUILD_TICK_MS: u32 = 100;

/// Over this, hooks stall.
const APPLY_BUDGET_MS: u128 = 50;

/// One dupe check's answer.
struct AnkiDupeResult {
    gen: u64,
    /// `None` = connection failed.
    dupes: Option<HashSet<String>>,
}

/// One add-note's answer.
struct AddNoteResult {
    expr: String,
    err: Option<String>,
}

/// Settings alone, no tray.
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
    let (settings_tx, settings_rx) = mpsc::channel::<String>();
    let (detect_tx, detect_rx) =
        mpsc::channel::<(Vec<String>, Vec<String>, Vec<String>)>();
    // SAFETY: no preconditions.
    let tid = unsafe { GetCurrentThreadId() };

    let mut msg = MSG::default();
    // SAFETY: `msg` is this loop's own stack storage, and `window` is alive
    // for the whole loop - it is dropped only after this function returns.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        // No hooks, nothing to disarm.
        window.pump(|| {});

        if matches!(msg.message, WM_KEYDOWN | WM_SYSKEYDOWN)
            && window.handle_capture_key(msg.wParam.0 as u16)
        {
            continue;
        }

        if msg.message == WM_APP_SETTINGS {
            while let Ok(status) = settings_rx.try_recv() {
                window.set_status(&status);
            }
        }

        if msg.message == WM_APP_ANKI_DETECT {
            while let Ok((decks, models, fields)) = detect_rx.try_recv() {
                window.populate_combos(&decks, &models, &fields);
            }
        }

        // Dialog keys first, as in run.
        if !unsafe { IsDialogMessageW(window.hwnd(), &msg) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        service_settings_click(&window, &settings_tx, &detect_tx, tid);

        // Tab switch -> detect.
        if let Some(tab) = window.take_tab_change() {
            window.switch_tab(tab);
            if tab == 3 {
                spawn_detect(
                    window.anki_url(), window.anki_model(),
                    detect_tx.clone(), tid,
                );
            }
        }
        if window.take_field_map_toggle() {
            window.toggle_field_map();
        }

        if rebuild.is_some() {
            // Not while the child writes.
            let _ = window.take_outcome();
            // Taken only when finished.
            let Some(built) = rebuild.as_ref().and_then(|f| pump_rebuild(&f.rx, &window)) else {
                continue;
            };
            let Some(flight) = rebuild.take() else { continue };
            // SAFETY: `tick` is this loop's own timer, set below.
            unsafe {
                let _ = KillTimer(None, tick);
            }
            window.set_busy(false);
            match built {
                Ok(()) => {
                    keep_apply(&flight, &window);
                    let updated = pending.take().unwrap_or_else(|| cfg.clone());
                    updated.save(config_path).with_context(|| {
                        format!("saving settings to {}", config_path.display())
                    })?;
                    println!("chibipop: rebuilt {}.", dict_path.display());
                    println!("chibipop: settings saved to {}.", config_path.display());
                    // New dictionary: start it.
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
            // No tray: X exits like Quit.
            Some(SettingsOutcome::Cancel) | Some(SettingsOutcome::Quit) => return Ok(()),
            Some(SettingsOutcome::Apply) => {
                let edited = window.read(&form);
                let updated = settings::apply_to(&edited, &cfg);
                // A font is not a rebuild.
                if !edited.has_staged() {
                    updated.save(config_path).with_context(|| {
                        format!("saving settings to {}", config_path.display())
                    })?;
                    println!("chibipop: settings saved to {}.", config_path.display());
                    println!("chibipop: restart chibipop for them to take effect.");
                    return Ok(());
                }
                match start_rebuild(&edited, &library, dict_path) {
                    Err(e) => refuse_apply(&window, &e),
                    Ok(flight) => {
                        begin_rebuild(&window);
                        // SAFETY: a thread timer, killed above on every exit
                        // from the rebuild - the same shape `run` uses.
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

/// The archive folder.
fn library_dir() -> PathBuf {
    crate::paths::beside_exe("library")
}

/// The form and the library.
fn form_with_library(cfg: &Config, dicts: &[DictInfo], dir: &Path) -> SettingsForm {
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

/// A change and its rebuild.
struct InFlight {
    pending: Pending,
    rx: mpsc::Receiver<Progress>,
    _lock: LibraryLock,
}

/// Lock, update, build.
fn start_rebuild(form: &SettingsForm, dir: &Path, out: &Path) -> Result<InFlight> {
    let lock = LibraryLock::acquire(dir)?;
    let (pending, rx) = stage_and_spawn(form, dir, out)?;
    Ok(InFlight { pending, rx, _lock: lock })
}

/// Stage, then start the build.
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

/// Put every archive back.
fn undo_apply(flight: &InFlight, why: &anyhow::Error) {
    undo_apply_pending(&flight.pending, why);
}

/// Put every archive back.
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

/// Let the removals go.
fn keep_apply(flight: &InFlight, w: &SettingsWindow) {
    if let Err(e) = flight.pending.commit() {
        eprintln!("chibipop: clearing the library's .removed folder failed: {e:#}");
    }
    w.clear_staged();
}

/// Progress, never blocking.
///
/// None while it runs.
fn pump_rebuild(rx: &mpsc::Receiver<Progress>, w: &SettingsWindow) -> Option<Result<()>> {
    loop {
        match rx.try_recv() {
            Ok(Progress::Line(line)) => {
                println!("chibipop: {line}");
                // Never the raw .tmp line.
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

/// Lock it and say so.
fn begin_rebuild(w: &SettingsWindow) {
    w.set_busy(true);
    w.set_status("Rebuilding your dictionary. This can take a few minutes.");
}

/// Busy while files copy.
fn begin_apply(w: &SettingsWindow) {
    w.set_busy(true);
    w.set_status("Applying your changes\u{2026}");
}

/// Say why Apply did nothing.
fn refuse_apply(w: &SettingsWindow, e: &anyhow::Error) {
    w.set_status(&format!("Not applied: {e}"));
    eprintln!("chibipop: not applied: {e:#}");
}

/// Freq needs a rebuild.
///
/// CRLF: the box is an EDIT.
fn frequency_notice(library: &Path, db: &Path) -> String {
    format!(
        "Frequency lists rank the words in every dictionary, so changing one needs the \
         whole database rebuilt - chibipop cannot do that while it is running. Nothing was \
         changed. Quit chibipop, then run this in a terminal, and start chibipop again \
         when it finishes:\r\nchibipop build-dict --library \"{}\" --out \"{}\"",
        library.display(),
        db.display()
    )
}

/// Say the library disagrees.
fn notice_drift(w: &SettingsWindow, dir: &Path, db: &Path) {
    match drifted(dir, db) {
        Err(e) => eprintln!("chibipop: checking for drift failed: {e:#}"),
        Ok(None) => {}
        Ok(Some(text)) => w.set_status(&text),
    }
}

/// The notice, if it drifted.
fn drifted(dir: &Path, db: &Path) -> Result<Option<String>> {
    let sources = read_source_hashes(db)?;
    let lib = Library::load(dir).with_context(|| format!("reading {}", dir.display()))?;
    Ok(settings::drift_notice(sources.as_deref(), &lib, dir, db))
}

/// What built it, if recorded.
fn read_source_hashes(db: &Path) -> Result<Option<String>> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {} to read its source list", db.display()))?;
    conn.query_row("SELECT v FROM meta WHERE k = 'source_hashes'", [], |r| r.get(0))
        .optional()
        .with_context(|| format!("reading source_hashes from {}", db.display()))
}

/// Say nothing was changed.
fn report_failed_rebuild(w: &SettingsWindow, e: &anyhow::Error) {
    w.set_status(STATUS_REBUILD_FAILED);
    eprintln!("chibipop: the rebuild failed: {e:#}");
    eprintln!("chibipop: the dictionary in use was not touched.");
}

/// A dictionary Apply deletes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Removal {
    name: String,
    dict_id: Option<i64>,
    file: Option<String>,
}

/// What one Apply must edit.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EditPlan {
    removals: Vec<Removal>,
    additions: Vec<crate::settings::StagedAdd>,
}

/// What one Apply changed.
#[derive(Debug, Default)]
struct EditReport {
    added: Vec<String>,
    removed: Vec<String>,
    failed: Vec<String>,
    dicts: Vec<DictInfo>,
}

/// One edit's progress channel.
enum EditMsg {
    Status(String),
    Done(Result<Box<EditReport>>),
}

/// An in-place edit running.
struct EditFlight {
    rx: mpsc::Receiver<EditMsg>,
    _lock: LibraryLock,
}

/// Read-write, WAL asserted.
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

/// Does Apply touch a freq zip?
///
/// Those still need a rebuild.
fn stages_frequency(form: &SettingsForm, dicts: &[DictInfo]) -> bool {
    let added = form.staged_adds.iter().any(|a| form.freq_names.contains(&a.name));
    let removed = form.staged_removes.iter().any(|name| {
        !dicts.iter().any(|d| &d.name == name) && !form.unreadable.contains(name)
    });
    added || removed
}

/// Which rows and files change.
fn plan_edits(form: &SettingsForm, dicts: &[DictInfo], lib: &Library) -> EditPlan {
    let removals = form
        .staged_removes
        .iter()
        .map(|name| Removal {
            name: name.clone(),
            dict_id: dicts.iter().find(|d| &d.name == name).map(|d| d.dict_id),
            file: lib
                .entries
                .iter()
                .find(|e| &e.name == name || &e.file == name)
                .map(|e| e.file.clone()),
        })
        .collect();
    EditPlan { removals, additions: form.staged_adds.clone() }
}

/// Count from this dictionary.
///
/// Builder ids are absolute.
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

/// What the edit achieved.
fn edit_status(report: &EditReport) -> String {
    let mut parts = Vec::new();
    if !report.added.is_empty() {
        parts.push(format!("Added {}.", report.added.join(", ")));
    }
    if !report.removed.is_empty() {
        parts.push(format!("Removed {}.", report.removed.join(", ")));
    }
    if !report.failed.is_empty() {
        parts.push(format!("Not applied: {}.", report.failed.join("; ")));
    }
    if parts.is_empty() {
        return "No dictionary changed.".to_string();
    }
    parts.join(" ")
}

/// Edit the live database.
///
/// Refuses before it moves.
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
    // From the file, not a cache.
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
            Ok(()) => report.removed.push(removal.name.clone()),
            Err(e) => report.failed.push(format!("{}: {e:#}", removal.name)),
        }
    }

    let freqs = lib.freq_paths(dir);
    for add in &plan.additions {
        say(format!("Reading {}\u{2026}", add.name));
        match add_one(&mut conn, &mut lib, dir, &freqs, add, tx) {
            Ok(name) => report.added.push(name),
            Err(e) => report.failed.push(format!("{}: {e:#}", add.name)),
        }
    }

    lib.save(dir).with_context(|| format!("saving {}", dir.display()))?;
    pending.commit()?;
    report.dicts = reader.dicts().context("re-reading dictionary identities")?;
    Ok(Box::new(report))
}

/// One dict: rows, then file.
fn remove_one(
    conn: &mut Connection,
    lib: &mut Library,
    dir: &Path,
    pending: &mut Pending,
    removal: &Removal,
) -> Result<()> {
    if let Some(dict_id) = removal.dict_id {
        let archive = removal.file.as_ref().map(|f| dir.join(f)).unwrap_or_default();
        let done = crate::dict::edit::remove_dictionary(conn, dict_id, &archive)?;
        if done.dicts == 0 {
            anyhow::bail!("dictionary {dict_id} was no longer in the database");
        }
    }
    if let Some(file) = &removal.file {
        lib.quarantine(dir, file).with_context(|| format!("removing {file}"))?;
        pending.held(file.clone());
    }
    Ok(())
}

/// One archive: file, then rows.
fn add_one(
    conn: &mut Connection,
    lib: &mut Library,
    dir: &Path,
    freqs: &[PathBuf],
    add: &crate::settings::StagedAdd,
    tx: &mpsc::Sender<EditMsg>,
) -> Result<String> {
    let entry = lib
        .import(dir, &add.source)
        .with_context(|| format!("importing {}", add.source.display()))?;
    let path = dir.join(&entry.file);
    let base = crate::dict::edit::next_entry_id(conn)?;
    let on_progress = |line: &str| {
        if let Some(text) = crate::dict::progress::friendly(&rebased(line, base)) {
            let _ = tx.send(EditMsg::Status(text));
        }
    };
    match crate::dict::edit::add_dictionary(conn, &path, freqs, &on_progress) {
        Ok(done) => Ok(done.name),
        Err(e) => {
            lib.entries.retain(|x| x.file != entry.file);
            let _ = std::fs::remove_file(&path);
            Err(e)
        }
    }
}

/// Progress, never blocking.
///
/// Never writes to stdout.
fn pump_edit(rx: &mpsc::Receiver<EditMsg>, w: &SettingsWindow) -> Option<Result<Box<EditReport>>> {
    loop {
        match rx.try_recv() {
            Ok(EditMsg::Status(text)) => w.set_status(&text),
            Ok(EditMsg::Done(done)) => return Some(done),
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => {
                return Some(Err(anyhow!("the dictionary change ended without reporting")));
            }
        }
    }
}

/// Reachable text + fields.
fn reachable_message(url: &str, model: &str) -> (String, Vec<String>) {
    match anki::model_field_names(url, model) {
        Ok(fields) => {
            let msg = format!(
                "AnkiConnect is reachable. \"{model}\" fields: {}",
                fields.join(", "),
            );
            (msg, fields)
        }
        Err(_) => ("AnkiConnect is reachable.".into(), Vec::new()),
    }
}

/// Deck/model/field names, off-thread.
fn spawn_detect(
    url: String,
    model: String,
    tx: mpsc::Sender<(Vec<String>, Vec<String>, Vec<String>)>,
    tid: u32,
) {
    thread::spawn(move || {
        let decks = anki::deck_names(&url).unwrap_or_default();
        let models = anki::model_names(&url).unwrap_or_default();
        let fields = anki::model_field_names(&url, &model).unwrap_or_default();
        let _ = tx.send((decks, models, fields));
        // SAFETY: wakes the pump.
        unsafe {
            let _ = PostThreadMessageW(
                tid, WM_APP_ANKI_DETECT,
                WPARAM(0), LPARAM(0),
            );
        }
    });
}

/// Spawns the Anki/update op.
fn service_settings_click(
    w: &SettingsWindow,
    tx: &mpsc::Sender<String>,
    detect_tx: &mpsc::Sender<(Vec<String>, Vec<String>, Vec<String>)>,
    tid: u32,
) {
    match w.take_click() {
        Some(SettingsClick::AnkiTest) => {
            w.set_status("Testing\u{2026}");
            let url = w.anki_url();
            let model = w.anki_model();
            let tx = tx.clone();
            let detect_tx = detect_tx.clone();
            thread::spawn(move || {
                let status = anki::check_connection(&url);
                let (msg, fields) = match &status {
                    Ok(true) => reachable_message(&url, &model),
                    Ok(false) => ("AnkiConnect did not respond.".into(), Vec::new()),
                    Err(e) => (format!("Anki test failed: {e:#}"), Vec::new()),
                };
                let _ = tx.send(msg);
                if matches!(status, Ok(true)) {
                    let decks = anki::deck_names(&url).unwrap_or_default();
                    let models = anki::model_names(&url).unwrap_or_default();
                    let _ = detect_tx.send((decks, models, fields));
                    // SAFETY: wakes the pump thread.
                    unsafe {
                        let _ = PostThreadMessageW(
                            tid, WM_APP_ANKI_DETECT,
                            WPARAM(0), LPARAM(0),
                        );
                    }
                }
                // SAFETY: wakes the pump thread.
                unsafe {
                    let _ = PostThreadMessageW(
                        tid, WM_APP_SETTINGS,
                        WPARAM(0), LPARAM(0),
                    );
                }
            });
        }
        Some(SettingsClick::CheckUpdate) => {
            w.set_status("Checking\u{2026}");
            let tx = tx.clone();
            thread::spawn(move || {
                let msg = match update::check(env!("CARGO_PKG_VERSION")) {
                    Ok(None) => "You already have the latest version.".into(),
                    Ok(Some(release)) => {
                        match update::download_and_replace(&release) {
                            Ok(()) => format!(
                                "Updated to {}. Restart to use it.",
                                release.tag,
                            ),
                            Err(e) => format!(
                                "Update to {} failed: {e:#}",
                                release.tag,
                            ),
                        }
                    }
                    Err(e) => format!("Update check failed: {e:#}"),
                };
                let _ = tx.send(msg);
                // SAFETY: wakes the pump thread.
                unsafe {
                    let _ = PostThreadMessageW(
                        tid, WM_APP_SETTINGS,
                        WPARAM(0), LPARAM(0),
                    );
                }
            });
        }
        None => {}
    }
}

/// The Windows worker parts, built on the worker thread: capture and OCR
/// backends are thread-affine (COM, per-thread DXGI cache), so nothing is
/// constructed until the core `Worker`'s thread runs this.
fn worker_open(
    dict_path: PathBuf,
    rules_path: PathBuf,
    language: String,
    guard: CaptureGuard,
) -> impl FnOnce() -> Result<WorkerParts> + Send + 'static {
    move || {
        let fallback = crate::config::default_ocr_language();
        let substitute =
            startup_language(&language, &fallback, || recogniser_available(&language));
        let language = match substitute {
            Some(sub) => {
                eprintln!("chibipop: no {language} OCR recogniser installed; starting with {sub}");
                sub
            }
            None => language,
        };
        // Contract 3: DPI before GDI.
        let capture = WinCapture::new(Some(guard)).context("preparing screen capture")?;
        let ocr = WinrtOcr::new(&language).context("creating the OCR text source")?;
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
            ocr: Box::new(ocr),
            dict: Box::new(dict),
            // A finished rebuild restarts this whole process
            // (`start_run` on `Progress::Done`), so this worker never
            // outlives the database it opened and a reload has nothing
            // to reopen.
            reopen_dict: None,
            engine,
        })
    }
}

/// Run until the user quits.
pub fn run(
    mut cfg: Config,
    dict_path: &Path,
    rules_path: &Path,
    config_path: &Path,
) -> Result<()> {
    // Nothing built yet.
    if !dict_path.exists() || !rules_path.exists() {
        return settings_only(cfg, &[], config_path, dict_path);
    }

    let library = library_dir();
    let db_path = dict_path.to_path_buf();
    let rules_path = rules_path.to_path_buf();

    // Unknown until Popup::create.
    let capture_guard_active = Arc::new(AtomicBool::new(false));
    let (capture_guard_tx, capture_guard_rx) = mpsc::channel::<CaptureGuardMsg>();

    // SAFETY: FFI call with no preconditions - always succeeds, returns the
    // id of whichever thread calls it.
    let main_tid = unsafe { GetCurrentThreadId() };
    let mut live = derive(&cfg);

    // Never joined - join hangs.
    let (worker, mut dicts) = Worker::spawn(
        // Spawn reads the file itself.
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
        ),
        // Worker pushed a result.
        move || unsafe {
            let _ = PostThreadMessageW(main_tid, WM_APP_RESULT, WPARAM(0), LPARAM(0));
        },
    )?;

    let popup = Popup::create(live.exclude_from_capture).context("creating the popup window")?;

    // Contract 2: report all three.
    match popup.capture_exclusion() {
        CaptureExclusion::Excluded => {
            println!(
                "chibipop: capture exclusion active - the popup will not appear in its own OCR captures"
            );
        }
        CaptureExclusion::DeliberatelyNotExcluded => {
            println!("chibipop: capture exclusion disabled (exclude_from_capture = false in the config)");
            println!("chibipop: the popup IS recordable now - each capture briefly hides and reshows it,");
            println!("chibipop: so hovering keeps resolving the real text underneath, not its own");
        }
        CaptureExclusion::AttemptFailed => {
            eprintln!("chibipop: ============================================================");
            eprintln!("chibipop: WARNING: capture exclusion is NOT active for the popup window.");
            eprintln!("chibipop: SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE) was not accepted,");
            eprintln!("chibipop: even though exclude_from_capture = true. This was NOT requested.");
            eprintln!("chibipop: The capture guard below will still hide/reshow the popup around");
            eprintln!("chibipop: every capture, so lookups stay correct, at the cost of a flicker");
            eprintln!("chibipop: this build did not expect to pay. Investigate why the OS refused.");
            eprintln!("chibipop: ============================================================");
        }
    }

    // Never fatal - spec §5.
    //
    // Always live; shown on demand.
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

    // Spec D5: can diverge.
    if let Some(CaptureExclusion::AttemptFailed) = overlay.as_ref().map(Overlay::capture_exclusion) {
        eprintln!("chibipop: ============================================================");
        eprintln!("chibipop: WARNING: capture exclusion is NOT active for the scan overlay window.");
        eprintln!("chibipop: SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE) was not accepted,");
        eprintln!("chibipop: even though exclude_from_capture = true. This was NOT requested.");
        eprintln!("chibipop: The capture guard below will still hide/reshow the overlay around");
        eprintln!("chibipop: every capture, so its outlines never land inside one, at the cost");
        eprintln!("chibipop: of a flicker this build did not expect to pay. Investigate why the OS refused.");
        eprintln!("chibipop: ============================================================");
    }

    // Never fatal - spec §5.
    //
    // Always live; shown on demand.
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

    // Recomputed by `apply_live`.
    capture_guard_active.store(
        capture_guard_needed(
            popup.capture_exclusion(),
            overlay.as_ref().map(Overlay::capture_exclusion),
            anki_button.as_ref().map(AnkiButton::capture_exclusion),
        ),
        Ordering::SeqCst,
    );

    let mut renderer =
        Renderer::new(popup.hwnd()).context("creating the D2D/DirectWrite renderer")?;
    let mut theme = theme_from_config(&live.popup);
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

    // No tray means no control.
    let tray = Tray::create(popup.hwnd()).context("creating the tray icon")?;

    // Thread timer, no window.
    let timer_id = unsafe { SetTimer(None, 0, DISPATCH_TICK_MS, None) };
    if timer_id == 0 {
        anyhow::bail!("SetTimer failed to install the dispatch tick");
    }

    println!("chibipop: running - hover Japanese text anywhere on screen.");
    println!("chibipop: right-click the tray icon to change mode or quit.");

    // Spawned before dicts existed.
    let (order, restrict) =
        resolve_dict_filter(&cfg, &dicts, || configured_recogniser_runs(&cfg));
    live.present_cfg.dict_order = order;
    live.present_cfg.restrict_to_order = restrict;
    // Visible just before the Hide.
    //
    // Cleared by hides elsewhere.
    let capture_guard_prev_visible = std::cell::Cell::new(false);
    // Overlay's own visibility.
    let overlay_prev_visible = std::cell::Cell::new(false);
    // Anki button visibility.
    let btn_prev_visible = std::cell::Cell::new(false);
    // Event in, Command out.
    let mut controller = Controller::new(controller_config(&live));
    // OpenSettings, loop-deferred.
    let mut want_settings = false;
    // Rising/falling key edges.
    let mut trigger_was_held = false;
    let (anki_tx, anki_rx) = mpsc::channel::<AnkiDupeResult>();
    let (add_tx, add_rx) = mpsc::channel::<AddNoteResult>();
    let (settings_tx, settings_rx) = mpsc::channel::<String>();
    let (detect_tx, detect_rx) =
        mpsc::channel::<(Vec<String>, Vec<String>, Vec<String>)>();
    let (save_tx, save_rx) = mpsc::channel::<Result<()>>();
    // One writer at a time.
    let mut save_job: Option<thread::JoinHandle<()>> = None;
    // BACKLOG 7: no way in but this.
    let mut settings: Option<SettingsWindow> = match SettingsWindow::open(
        &form_with_library(&cfg, &dicts, &library),
        &settings::stale_order_entries(&cfg, &dicts),
        ApplyMode::Live,
    ) {
        // Never fatal.
        Err(e) => {
            eprintln!("chibipop: opening settings at startup failed: {e:#}");
            None
        }
        Ok(w) => {
            notice_drift(&w, &library, &db_path);
            Some(w)
        }
    };
    // An in-place edit in flight.
    let mut edit: Option<EditFlight> = None;
    let mut edit_cfg: Option<Config> = None;

    // I4: kept in one place.
    let drain_capture_guard = || {
        while let Ok(req) = capture_guard_rx.try_recv() {
            match req {
                CaptureGuardMsg::Hide { ack } => {
                    capture_guard_prev_visible.set(popup.is_visible());
                    let _ = popup.hide();
                    btn_prev_visible.set(
                        anki_button.as_ref().is_some_and(|b| b.is_visible()),
                    );
                    if let Some(b) = &anki_button {
                        b.hide();
                    }
                    if let Some(hwnd) = overlay_hwnd {
                        // SAFETY: `hwnd` is `Overlay::hwnd()`'s own handle;
                        // the `Overlay` that owns it lives in `run`'s local
                        // `overlay` for this whole loop, so the window is
                        // still live here. Both calls only read/set
                        // visibility - no other precondition applies.
                        overlay_prev_visible.set(unsafe { IsWindowVisible(hwnd).as_bool() });
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
                            // SAFETY: same handle, same guarantee as above.
                            unsafe {
                                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                            }
                        }
                    }
                }
            }
        }
    };

    // One Event through the state
    // machine, every Command done;
    // ShowPopup answers in place.
    macro_rules! drive {
        ($event:expr) => {
            drive(
                &mut controller,
                $event,
                &mut Exec {
                    popup: &popup,
                    renderer: &mut renderer,
                    theme: &theme,
                    live: &live,
                    overlay: overlay.as_ref(),
                    anki_button: anki_button.as_ref(),
                    trigger_tx: worker.trigger(),
                    dicts: &dicts,
                    anki_tx: &anki_tx,
                    add_tx: &add_tx,
                    main_tid,
                    want_settings: &mut want_settings,
                },
            )
        };
    }

    // The worker spawned before the
    // dictionaries were known.
    drive!(Event::ConfigReloaded(Box::new(controller_config(&live))));

    let mut msg = MSG::default();

    loop {
        // Window and thread messages.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break; // 0 = WM_QUIT, -1 = error. Either way, stop pumping.
        }

        // Modeless routing - spec D2.
        if let Some(w) = &settings {
            // Spec D9: the picker pumps.
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

            // SAFETY: `w.hwnd()` is live until the `SettingsWindow` is
            // dropped, and `msg` is this loop's own stack storage.
            let handled = unsafe { IsDialogMessageW(w.hwnd(), &msg) }.as_bool();
            service_settings_click(w, &settings_tx, &detect_tx, main_tid);

            // Tab switch -> detect.
            if let Some(tab) = w.take_tab_change() {
                w.switch_tab(tab);
                if tab == 3 {
                    spawn_detect(
                        w.anki_url(), w.anki_model(),
                        detect_tx.clone(), main_tid,
                    );
                }
            }
            if w.take_field_map_toggle() {
                w.toggle_field_map();
            }

            if handled {
                continue;
            }
        }

        if msg.message == WM_TIMER && msg.wParam.0 == timer_id {
            // Spec D7: the popup's own rect.
            let cursor_pos = cursor_now();
            let button_h = anki_button
                .as_ref()
                .filter(|b| b.is_visible())
                .map_or(0, |b| b.height_phys());
            drive!(Event::Tick { cursor: cursor_pos, button_h });

            let notches = Hooks::take_whole_notches();
            if notches != 0 {
                drive!(Event::Scrolled { notches });
            }

            if let Some(click) = Hooks::take_click() {
                // The bin hit-tests: it
                // owns the painted layout.
                let local_hit = controller.popup().map(|view| {
                    let local = PhysPoint {
                        x: click.x - view.popup.x,
                        y: click.y - view.popup.y,
                    };
                    (local, renderer.hit_test(local.x, local.y, view.scroll))
                });
                if let Some((local, hit)) = local_hit {
                    drive!(Event::Clicked { local, hit });
                }
            }

            // Fallback: direct WM_LBUTTONDOWN.
            if anki_button.as_ref().is_some_and(|b| b.take_click()) {
                drive!(Event::AddRequested);
            }

            if Hooks::take_add_hotkey() {
                drive!(Event::AddRequested);
            }

            if Hooks::take_back() {
                drive!(Event::BackRequested);
            }

            if let Some(w) = &settings {
                if edit.is_some() {
                    // Not while the db changes.
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
                                let mut updated =
                                    edit_cfg.take().unwrap_or_else(|| cfg.clone());
                                // Removals first: keys collide.
                                for name in &report.removed {
                                    settings::dictionary_removed(&mut updated, name);
                                }
                                for name in &report.added {
                                    settings::dictionary_added(&mut updated, name);
                                }
                                // Spec §4: the cache was stale.
                                dicts = report.dicts;
                                w.clear_staged();
                                w.reseed_per_language(&updated.dictionaries.per_language);
                                cfg = updated.clone();
                                live = derive(&cfg);
                                let (order, restrict) = resolve_dict_filter(
                                    &cfg, &dicts, || configured_recogniser_runs(&cfg));
                                live.present_cfg.dict_order = order;
                                live.present_cfg.restrict_to_order = restrict;
                                apply_live(&live, &popup, overlay.as_ref(),
                                           anki_button.as_ref(), &mut theme,
                                           &capture_guard_active);
                                // Kills stale results.
                                drive!(Event::ConfigReloaded(Box::new(
                                    controller_config(&live),
                                )));
                                save_in_background(&mut save_job, updated,
                                                   config_path.to_path_buf(),
                                                   save_tx.clone(), main_tid);
                                w.set_status(&status);
                            }
                        }
                    }
                } else {
                    match w.take_outcome() {
                        // Tray remains; just hide.
                        Some(SettingsOutcome::Cancel) => settings = None,
                        // Already on the main thread.
                        Some(SettingsOutcome::Quit) => drive!(Event::Quit),
                        Some(SettingsOutcome::Apply) => {
                            let t0 = std::time::Instant::now();
                            let edited = w.read(&form_with_library(&cfg, &dicts, &library));
                            let updated = settings::apply_to(&edited, &cfg);
                            // Never half-apply.
                            if edited.has_staged() && stages_frequency(&edited, &dicts) {
                                w.set_status(&frequency_notice(&library, &db_path));
                            } else if edited.has_staged() {
                                match LibraryLock::acquire(&library) {
                                    Err(e) => refuse_apply(w, &e),
                                    Ok(lock) => {
                                        begin_apply(w);
                                        edit_cfg = Some(updated);
                                        let (etx, erx) = mpsc::channel::<EditMsg>();
                                        edit = Some(EditFlight { rx: erx, _lock: lock });
                                        let db = db_path.clone();
                                        let dir = library.clone();
                                        thread::spawn(move || {
                                            let done =
                                                apply_edits(&db, &dir, &edited, &etx);
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
                                live = derive(&updated);
                                let (order, restrict) = resolve_dict_filter(
                                    &updated, &dicts,
                                    || configured_recogniser_runs(&updated));
                                live.present_cfg.dict_order = order;
                                live.present_cfg.restrict_to_order = restrict;
                                apply_live(&live, &popup, overlay.as_ref(),
                                           anki_button.as_ref(), &mut theme,
                                           &capture_guard_active);
                                drive!(Event::ConfigReloaded(Box::new(
                                    controller_config(&live),
                                )));
                                let clamped = settings::clamp_notice(&edited, &updated);
                                w.reseed_per_language(&updated.dictionaries.per_language);
                                cfg = updated.clone();
                                save_in_background(&mut save_job, updated,
                                                   config_path.to_path_buf(),
                                                   save_tx.clone(), main_tid);
                                match &clamped {
                                    Some(notice) => {
                                        w.set_capture_fields(&cfg.ocr);
                                        w.set_status(notice);
                                    }
                                    None => w.set_status("Settings applied."),
                                }
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

            // Shift up retracts it.
            let held = Hooks::trigger_held();
            if held != trigger_was_held {
                trigger_was_held = held;
                if held {
                    drive!(Event::TriggerDown);
                } else {
                    if !matches!(live.trigger_mode, crate::config::TriggerMode::Live) {
                        // Restore would re-show it.
                        capture_guard_prev_visible.set(false);
                        overlay_prev_visible.set(false);
                        btn_prev_visible.set(false);
                    }
                    drive!(Event::TriggerUp);
                }
            }

            let cursor = Hooks::take_pending().unwrap_or_else(|| {
                // Fallback: poll GetCursorPos when the LL hook
                // is blocked (e.g. by anti-cheat).
                let pos = cursor_pos;
                let dominated = Hooks::poll_gate(pos);
                if dominated { pos } else {
                    PhysPoint { x: i32::MIN, y: i32::MIN }
                }
            });
            if cursor.x != i32::MIN {
                drive!(Event::CursorMoved { pos: cursor });
            }
        } else if msg.message == WM_APP_RESULT {
            // Only the freshest queued.
            let mut freshest: Option<WorkerResult> = None;
            while let Ok(r) = worker.results().try_recv() {
                freshest = Some(r);
            }
            if let Some(result) = freshest {
                drive!(Event::LookupResult { id: result.id, outcome: result.outcome });
            }
        } else if msg.message == WM_APP_ANKI {
            while let Ok(result) = anki_rx.try_recv() {
                drive!(Event::DupesChecked { generation: result.gen, dupes: result.dupes });
            }
        } else if msg.message == WM_APP_ADD_NOTE {
            while let Ok(result) = add_rx.try_recv() {
                if let Some(e) = &result.err {
                    eprintln!("chibipop: add to Anki failed: {e}");
                }
                let failed = result.err.is_some();
                drive!(Event::NoteAdded { expr: result.expr, failed });
            }
        } else if msg.message == WM_APP_SETTINGS {
            while let Ok(status) = settings_rx.try_recv() {
                if let Some(w) = &settings {
                    w.set_status(&status);
                }
            }
        } else if msg.message == WM_APP_ANKI_DETECT {
            while let Ok((decks, models, fields)) = detect_rx.try_recv() {
                if let Some(w) = &settings {
                    w.populate_combos(&decks, &models, &fields);
                }
            }
        } else if msg.message == WM_APP_SAVED {
            while let Ok(result) = save_rx.try_recv() {
                if let Err(e) = result {
                    eprintln!("chibipop: could not save settings to {}: {e:#}",
                              config_path.display());
                    if let Some(w) = &settings {
                        w.set_status(
                            "Settings applied, but could not be saved - \
                             they will be lost on restart.",
                        );
                    }
                }
            }
        } else if msg.message == WM_APP_CAPTURE_GUARD {
            // Drain, never one per wakeup.
            drain_capture_guard();
        } else if let Some(cmd) = tray.handle_message(msg.message, msg.lParam, || {
            // The menu swallows WM_TIMER.
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
                        // Never fatal.
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
    }

    // Shutdown, decision 5's order.
    unsafe {
        let _ = KillTimer(None, timer_id);
    }
    // No hooks while we block.
    drop(hooks.take());
    // No ack while we block.
    capture_guard_active.store(false, Ordering::SeqCst);
    // exit(0) kills it mid-write.
    join_save(&mut save_job);
    std::process::exit(0)
}

/// Measure, place, show, paint.
#[allow(clippy::too_many_arguments)]
fn show_presentation(
    popup: &Popup,
    renderer: &mut Renderer,
    theme: &Theme,
    max_height_percent: i32,
    max_width_percent: i32,
    presentation: &Presentation,
    anchor: PhysRect,
    scroll: i32,
    show_back: bool,
    side_panel: bool,
) -> Result<(PhysRect, i32, i32)> {
    let monitor = monitor_rect_for(anchor);
    let max_w = ((monitor.w * max_width_percent) / 100).max(1);
    let max_h = ((monitor.h * max_height_percent) / 100).max(1);

    // view_h, not content_h, below.
    let (w, view_h, content_h) = renderer
        .measure(presentation, theme, max_w, max_h, show_back, side_panel)
        .context("measuring popup content")?;

    let rect = place_popup(anchor, (w, view_h), monitor, POPUP_GAP);
    popup.show_at(rect).context("moving/showing the popup")?;
    renderer.paint(presentation, theme, scroll, show_back, side_panel)
        .context("painting the popup")?;
    Ok((rect, content_h, view_h))
}

/// Relaunch with this argv.
/// Start the popup app.
fn start_run(config_path: &Path, dict_path: &Path) -> Result<()> {
    let exe = std::env::current_exe().context("locating this executable")?;
    // Explicit: may be non-default.
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

/// Live, not the gated point.
fn cursor_now() -> PhysPoint {
    let mut pt = POINT::default();
    // SAFETY: FFI call taking a pointer to local stack storage that outlives
    // the call. On failure `pt` stays zeroed, which merely disarms the wheel
    // for one tick.
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    PhysPoint { x: pt.x, y: pt.y }
}

/// The monitor under the anchor.
fn monitor_rect_for(anchor: PhysRect) -> PhysRect {
    let c = anchor.center();
    let pt = POINT { x: c.x, y: c.y };
    unsafe {
        // Never null, so never checked.
        let hmon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(hmon, &mut mi).as_bool() {
            let rc = mi.rcWork;
            PhysRect { x: rc.left, y: rc.top, w: rc.right - rc.left, h: rc.bottom - rc.top }
        } else {
            eprintln!("chibipop: GetMonitorInfoW failed; placing against a 1920x1080 fallback");
            PhysRect { x: 0, y: 0, w: 1920, h: 1080 }
        }
    }
}

/// Places, paints, or hides it.
///
/// Sits below the popup, flush
/// with its left/right edges.
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

/// What the Controller reads.
fn controller_config(live: &LiveSettings) -> ControllerConfig {
    ControllerConfig {
        trigger_mode: live.trigger_mode,
        per_character_lookup: live.per_character_lookup,
        scroll_popup: live.scroll_popup,
        anki_enabled: live.anki_enabled,
        summary_chars: live.summary_chars,
        log_lookups: live.show_lookup_log,
        tick_ms: DISPATCH_TICK_MS,
    }
}

/// What executing Commands needs.
struct Exec<'a> {
    popup: &'a Popup,
    renderer: &'a mut Renderer,
    theme: &'a Theme,
    live: &'a LiveSettings,
    overlay: Option<&'a Overlay>,
    anki_button: Option<&'a AnkiButton>,
    trigger_tx: &'a mpsc::Sender<Trigger>,
    dicts: &'a [DictInfo],
    anki_tx: &'a mpsc::Sender<AnkiDupeResult>,
    add_tx: &'a mpsc::Sender<AddNoteResult>,
    main_tid: u32,
    /// OpenSettings, loop-handled.
    want_settings: &'a mut bool,
}

/// One Event, to quiescence.
///
/// `ShowPopup` is executed right
/// here, so its `PopupPlaced` (or
/// failure) feeds straight back.
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

/// One Command, one effect.
fn execute(controller: &Controller, cmd: Command, x: &mut Exec<'_>) -> Option<Event> {
    match cmd {
        // Windows keeps its own popup out of its own captures at the OS
        // level - WDA_EXCLUDEFROMCAPTURE or the hide/reshow capture guard
        // - so it supplies no mask rects and `popup` goes unread here
        // (ADR-0008).
        Command::RequestLookup { id, point, popup: _ } => {
            let _ = x.trigger_tx.send(Trigger {
                kind: TriggerKind::Hover(Hover { at: point, mask: CaptureMask::NONE }),
                id,
            });
            None
        }
        Command::RequestDrillDown { id, text } => {
            let _ = x.trigger_tx.send(Trigger { kind: TriggerKind::DrillDown(text), id });
            None
        }
        Command::RequestReload { id } => {
            let _ = x.trigger_tx.send(Trigger {
                kind: TriggerKind::Reload(Box::new(worker_settings(x.live, x.dicts))),
                id,
            });
            None
        }
        Command::ShowPopup { presentation, anchor, scroll, show_back } => {
            match show_presentation(
                x.popup,
                x.renderer,
                x.theme,
                x.live.max_height_percent,
                x.live.max_width_percent,
                &presentation,
                anchor,
                scroll,
                show_back,
                x.live.side_panel,
            ) {
                Ok((rect, content_h, view_h)) => {
                    Some(Event::PopupPlaced { rect, content_h, view_h })
                }
                Err(e) => {
                    eprintln!("chibipop: showing the popup failed: {e:#}");
                    Some(Event::PopupPlaceFailed)
                }
            }
        }
        Command::RepaintPopup { scroll, show_back } => {
            if let Some(view) = controller.popup() {
                let painted = x.renderer.paint(
                    view.presentation,
                    x.theme,
                    scroll,
                    show_back,
                    x.live.side_panel,
                );
                if let Err(e) = painted {
                    eprintln!("chibipop: repainting the popup failed: {e:#}");
                }
            }
            None
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
            Hooks::set_click_armed(armed);
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
            let url = x.live.anki_url.clone();
            let deck = x.live.anki_deck.clone();
            let model = x.live.anki_model.clone();
            let tx = x.anki_tx.clone();
            let main_tid = x.main_tid;
            thread::spawn(move || {
                let refs: Vec<&str> = exprs.iter().map(|s| s.as_str()).collect();
                let dupes = match anki::find_duplicates(&url, &deck, &model, &refs) {
                    Ok(d) => Some(d),
                    Err(e) => {
                        eprintln!("chibipop: dupe check failed: {e:#}");
                        None
                    }
                };
                let _ = tx.send(AnkiDupeResult { gen: generation, dupes });
                // SAFETY: wakes the pump.
                unsafe {
                    let _ = PostThreadMessageW(main_tid, WM_APP_ANKI, WPARAM(0), LPARAM(0));
                }
            });
            None
        }
        Command::AddNote { expr, fields } => {
            let url = x.live.anki_url.clone();
            let deck = x.live.anki_deck.clone();
            let model = x.live.anki_model.clone();
            let field_map = x.live.anki_field_map.clone();
            let tx = x.add_tx.clone();
            let main_tid = x.main_tid;
            thread::spawn(move || {
                let err = anki::add_note(&url, &deck, &model, &fields, &field_map)
                    .err()
                    .map(|e| format!("{e:#}"));
                let _ = tx.send(AddNoteResult { expr, err });
                // SAFETY: wakes the pump.
                unsafe {
                    let _ = PostThreadMessageW(main_tid, WM_APP_ADD_NOTE, WPARAM(0), LPARAM(0));
                }
            });
            None
        }
        Command::LogLookup { headword, match_len } => {
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
        Command::OpenSettings => {
            *x.want_settings = true;
            None
        }
        Command::Exit => {
            // Already on the main thread.
            unsafe { PostQuitMessage(0) };
            None
        }
    }
}

/// Palette by name, font on top.
fn theme_from_config(popup: &crate::config::PopupConfig) -> Theme {
    let mut theme = match popup.theme.as_str() {
        "light" => Theme::light(),
        _ => Theme::dark(),
    };
    theme.font_name = popup.font.clone();
    theme
}

/// What run reads from config.
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
    trigger_mode: crate::config::TriggerMode,
    trigger_key: String,
    anki_add_key: String,
    per_character_lookup: bool,
}

/// Rebuilt on each change.
fn derive(cfg: &Config) -> LiveSettings {
    LiveSettings {
        popup: cfg.popup.clone(),
        present_cfg: cfg.present_config(),
        scan_display: ScanDisplay {
            captures: cfg.debug.show_scan_region,
            highlight: cfg.popup.highlight_match,
        },
        max_ocr_passes: cfg.ocr.max_ocr_passes,
        prefer_vertical: cfg.ocr.prefer_vertical,
        capture: CaptureSize { w: cfg.ocr.capture_width, h: cfg.ocr.capture_height },
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
        trigger_mode: cfg.trigger.mode,
        trigger_key: cfg.trigger.trigger_key.clone(),
        anki_add_key: cfg.anki.add_key.clone(),
        per_character_lookup: cfg.trigger.per_character_lookup,
    }
}

/// What the worker reloads.
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
        dicts: dicts.to_vec(),
    }
}

/// Not excluded means guard on.
/// `None`: no such window.
fn capture_guard_needed(
    popup: CaptureExclusion,
    overlay: Option<CaptureExclusion>,
    button: Option<CaptureExclusion>,
) -> bool {
    popup.needs_capture_guard()
        || overlay.is_some_and(CaptureExclusion::needs_capture_guard)
        || button.is_some_and(CaptureExclusion::needs_capture_guard)
}

/// Push settings to windows.
fn apply_live(
    live: &LiveSettings,
    popup: &Popup,
    overlay: Option<&Overlay>,
    button: Option<&AnkiButton>,
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
    capture_guard_active.store(
        capture_guard_needed(
            popup.capture_exclusion(),
            overlay.map(Overlay::capture_exclusion),
            button.map(AnkiButton::capture_exclusion),
        ),
        Ordering::SeqCst,
    );
    *theme = theme_from_config(&live.popup);
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
}

/// Must not block the pump.
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
        // SAFETY: wakes the pump thread.
        unsafe {
            let _ = PostThreadMessageW(main_tid, WM_APP_SAVED, WPARAM(0), LPARAM(0));
        }
    }));
}

/// No second writer, ever.
fn join_save(job: &mut Option<thread::JoinHandle<()>>) {
    if let Some(h) = job.take() {
        let _ = h.join();
    }
}

/// Some = substitute it.
fn startup_language(configured: &str, fallback: &str, available: impl FnOnce() -> bool)
    -> Option<String> {
    if configured.eq_ignore_ascii_case(fallback) || available() {
        None
    } else {
        Some(fallback.to_string())
    }
}

/// Will the configured tag run?
fn configured_recogniser_runs(cfg: &Config) -> bool {
    let fallback = crate::config::default_ocr_language();
    let tag = &cfg.ocr.language;
    startup_language(tag, &fallback, || recogniser_available(tag)).is_none()
}

/// The list this language uses.
fn resolve_dict_filter(
    cfg: &Config,
    dicts: &[DictInfo],
    engine_runs: impl FnOnce() -> bool,
) -> (Vec<String>, bool) {
    let listed = cfg.dictionaries.per_language.get(&cfg.ocr.language);
    let Some(list) = listed.filter(|l| !l.is_empty()) else {
        return (cfg.dictionaries.display_order.clone(), false);
    };
    let installed = dicts.iter().map(|d| d.name.as_str());
    if crate::present::any_listed(installed, list) && engine_runs() {
        (list.clone(), true)
    } else {
        (cfg.dictionaries.display_order.clone(), false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PopupConfig;

    fn popup_config(theme: &str, font: &str) -> PopupConfig {
        PopupConfig {
            theme: theme.to_string(),
            exclude_from_capture: false,
            max_width_percent: 25,
            max_height_percent: 45,
            summary_chars: 40,
            font: font.to_string(),
            highlight_match: true,
            scroll_popup: true,
            side_panel: false,
            layer: Default::default(),
        }
    }

    /// I1: font must reach Theme.
    #[test]
    fn a_non_default_font_reaches_the_theme() {
        let theme = theme_from_config(&popup_config("dark", "Noto Sans JP"));
        assert_eq!("Noto Sans JP", theme.font_name);
    }

    #[test]
    fn theme_selection_by_name_is_unaffected_by_the_font_field() {
        assert_eq!(Theme::light().background, theme_from_config(&popup_config("light", "X")).background);
        assert_eq!(Theme::dark().background, theme_from_config(&popup_config("anything-else", "X")).background);
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

    /// The headline plumbing.
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

    /// Step 3b: the input trio.
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

    /// Spec D5: they can diverge.
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
        assert!(!capture_guard_needed(CaptureExclusion::Excluded, None, None));
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
        assert!(!derive(&Config::default()).per_character_lookup, "must default off");
    }

    #[test]
    fn a_startup_language_with_no_pack_falls_back_to_the_default() {
        assert_eq!(Some("ja".to_string()), startup_language("ko", "ja", || false));
    }

    #[test]
    fn an_installed_startup_language_is_left_alone() {
        assert_eq!(None, startup_language("ko", "ja", || true));
    }

    /// Else it would loop on itself.
    #[test]
    fn the_default_language_never_substitutes_itself() {
        assert_eq!(None, startup_language("JA", "ja", || false));
    }

    /// No WinRT call on the default.
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
        crate::present::DictInfo { dict_id: id, name: name.to_string() }
    }

    #[test]
    fn the_active_language_selects_its_own_list() {
        let mut cfg = Config::default();
        cfg.ocr.language = "zh-Hans-CN".to_string();
        cfg.dictionaries.per_language.insert(
            "zh-Hans-CN".to_string(), vec!["中日大辞典".to_string()]);
        let dicts = [di(1, "大辞林　第四版"), di(2, "中日大辞典　第二版")];
        let (order, restrict) = resolve_dict_filter(&cfg, &dicts, || true);
        assert_eq!(vec!["中日大辞典".to_string()], order);
        assert!(restrict);
    }

    #[test]
    fn a_language_with_no_list_falls_back_to_display_order() {
        let cfg = Config::default();
        let dicts = [di(1, "大辞林　第四版")];
        let (order, restrict) = resolve_dict_filter(&cfg, &dicts, || true);
        assert_eq!(cfg.dictionaries.display_order, order);
        assert!(!restrict, "no entry must not restrict");
    }

    /// A typo must not blank it.
    #[test]
    fn a_list_matching_nothing_installed_falls_back() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries.per_language.insert(
            "ja".to_string(), vec!["Typoo".to_string()]);
        let dicts = [di(1, "大辞林　第四版")];
        let (_, restrict) = resolve_dict_filter(&cfg, &dicts, || true);
        assert!(!restrict, "all patterns missed, so do not restrict");
    }

    /// Wrong engine: no filter.
    #[test]
    fn a_substituted_recogniser_ignores_the_language_list() {
        let mut cfg = Config::default();
        cfg.ocr.language = "zh-Hans-CN".to_string();
        cfg.dictionaries.per_language.insert(
            "zh-Hans-CN".to_string(), vec!["中日大辞典".to_string()]);
        let dicts = [di(1, "大辞林　第四版"), di(2, "中日大辞典　第二版")];
        let (order, restrict) = resolve_dict_filter(&cfg, &dicts, || false);
        assert_eq!(cfg.dictionaries.display_order, order);
        assert!(!restrict, "the engine is not running this language");
    }

    #[test]
    fn an_empty_list_does_not_restrict() {
        let mut cfg = Config::default();
        cfg.ocr.language = "ja".to_string();
        cfg.dictionaries.per_language.insert("ja".to_string(), Vec::new());
        let dicts = [di(1, "大辞林　第四版")];
        let (_, restrict) = resolve_dict_filter(&cfg, &dicts, || true);
        assert!(!restrict);
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
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/yomitan").join(name)
    }

    /// WAL, as build-dict writes it.
    fn built_db(dir: &Path, library: &Path) -> PathBuf {
        std::fs::create_dir_all(library).unwrap();
        std::fs::copy(fixture("terms.zip"), library.join("terms.zip")).unwrap();
        let out = dir.join("chibipop.sqlite");
        crate::dict::build::build(&[library.join("terms.zip")], &[], &out, &|_| {}).unwrap();
        out
    }

    fn dict_rows(db: &Path) -> Vec<(i64, String)> {
        let conn = rusqlite::Connection::open(db).unwrap();
        let mut stmt = conn.prepare("SELECT dict_id, name FROM dict ORDER BY dict_id").unwrap();
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
                    kind: crate::library::Kind::Term,
                })
                .collect(),
        }
    }

    fn report_of(added: &[&str], removed: &[&str], failed: &[&str]) -> EditReport {
        EditReport {
            added: added.iter().map(|s| (*s).to_string()).collect(),
            removed: removed.iter().map(|s| (*s).to_string()).collect(),
            failed: failed.iter().map(|s| (*s).to_string()).collect(),
            dicts: Vec::new(),
        }
    }

    /// Spec 2: never write to it.
    #[test]
    fn the_writer_refuses_a_database_that_is_not_in_wal_mode() {
        let (dir, _guard) = edit_scratch("legacy_mode");
        let legacy = dir.join("legacy.sqlite");
        let conn = rusqlite::Connection::open(&legacy).unwrap();
        conn.execute_batch("PRAGMA journal_mode = DELETE; CREATE TABLE t(x);").unwrap();
        drop(conn);

        let err = open_writer(&legacy).expect_err("a delete-mode file must be refused");
        let msg = format!("{err:#}");
        assert!(msg.contains("delete"), "the message must name the mode found: {msg}");
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

    /// Never create a blank one.
    #[test]
    fn the_writer_never_creates_a_missing_database() {
        let (dir, _guard) = edit_scratch("no_create");
        let missing = dir.join("absent.sqlite");
        assert!(open_writer(&missing).is_err(), "a missing database must not open");
        assert!(!missing.exists(), "opening must not create the file");
    }

    /// Absolute ids read wrong.
    #[test]
    fn progress_counts_from_the_dictionary_being_added() {
        assert_eq!("progress  4997 / ?", rebased("progress  365000 / ?", 360004));
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
        assert_eq!("building  creating index", rebased("building  creating index", 360004));
        assert_eq!("progress  x / ?", rebased("progress  x / ?", 10));
    }

    /// A freq zip has no dict row.
    #[test]
    fn adding_a_frequency_archive_is_refused() {
        let mut form = staged_form(&[(Path::new("freq.zip"), "JA Freq")], &[]);
        form.freq_names = vec!["JA Freq".to_string()];
        assert!(stages_frequency(&form, &[]));
    }

    #[test]
    fn adding_a_term_dictionary_stays_incremental() {
        let form = staged_form(&[(Path::new("terms.zip"), "FixtureTerms")], &[]);
        assert!(!stages_frequency(&form, &[]));
    }

    #[test]
    fn removing_a_row_the_database_never_had_is_refused() {
        let form = staged_form(&[], &["JA Freq"]);
        assert!(stages_frequency(&form, &[di(1, "\u{5927}\u{8f9e}\u{6797}")]));
    }

    #[test]
    fn a_refused_frequency_change_names_the_builder_and_both_paths() {
        let notice = frequency_notice(
            Path::new(r"C:\a\library"),
            Path::new(r"C:\a\data\chibipop.sqlite"),
        );
        assert!(notice.contains("Nothing was changed."), "{notice}");
        assert!(notice.contains("\r\nchibipop build-dict"), "the command needs its own line");
        assert!(notice.contains("--library \"C:\\a\\library\""), "{notice}");
        assert!(notice.contains("--out \"C:\\a\\data\\chibipop.sqlite\""), "{notice}");
    }

    #[test]
    fn removing_an_installed_dictionary_stays_incremental() {
        let form = staged_form(&[], &["\u{5927}\u{8f9e}\u{6797}"]);
        assert!(!stages_frequency(&form, &[di(1, "\u{5927}\u{8f9e}\u{6797}")]));
    }

    /// A broken zip has no dict row.
    #[test]
    fn removing_an_unreadable_file_stays_incremental() {
        let mut form = staged_form(&[], &["broken.zip"]);
        form.unreadable = vec!["broken.zip".to_string()];
        assert!(!stages_frequency(&form, &[di(1, "\u{5927}\u{8f9e}\u{6797}")]));
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

    /// Unreadables list by file.
    #[test]
    fn a_removal_may_name_the_archive_file() {
        let form = staged_form(&[], &["broken.zip"]);
        let plan = plan_edits(&form, &[], &lib_of(&[("broken.zip", "broken")]));
        assert_eq!(None, plan.removals[0].dict_id);
        assert_eq!(Some("broken.zip".to_string()), plan.removals[0].file);
    }

    /// A row the library forgot.
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

    /// Spec 9: name the failure.
    #[test]
    fn the_status_names_what_failed_beside_what_worked() {
        let s = edit_status(&report_of(&["New"], &[], &["Bad: the zip is corrupt"]));
        assert!(s.contains("New"), "the applied change must still be named: {s}");
        assert!(s.contains("Bad"), "the failure must be named: {s}");
        assert!(s.contains("the zip is corrupt"), "the reason must be named: {s}");
    }

    #[test]
    fn a_change_that_did_nothing_says_so() {
        assert_eq!("No dictionary changed.", edit_status(&report_of(&[], &[], &[])));
    }

    /// The release's whole point.
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

        assert_eq!(vec!["FixtureTerms".to_string()], report.added);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(2, report.dicts.len(), "{:?}", report.dicts);
        assert_eq!(2, dict_rows(&db).len());
        assert_eq!(before * 2, entry_count(&db), "every entry must be kept and doubled");
        assert!(
            rx.try_iter().any(|m| matches!(m, EditMsg::Status(_))),
            "the edit must report progress"
        );
    }

    /// REGRESSION 1.18's symptom.
    #[test]
    fn an_apply_removes_a_dictionary_and_its_archive() {
        let (dir, _guard) = edit_scratch("remove");
        let library = dir.join("library");
        let db = built_db(&dir, &library);
        std::fs::copy(fixture("terms.zip"), library.join("extra.zip")).unwrap();
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(
                "INSERT INTO dict (dict_id, name, priority) VALUES (9, 'extra.zip', 8);
                 INSERT INTO entry (entry_id, dict_id, senses) VALUES (900, 9, '[]');
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
        assert_eq!(kept, entry_count(&db), "the other dictionary must be untouched");
        assert!(!library.join("extra.zip").exists(), "the archive must be gone");
        assert!(!library.join(".removed").exists(), "nothing may stay quarantined");
        assert_eq!(1, report.dicts.len());
    }

    /// Task 4's guard, from here.
    #[test]
    fn a_refused_addition_leaves_no_trace_in_the_library() {
        let (dir, _guard) = edit_scratch("refused_add");
        let library = dir.join("library");
        let db = built_db(&dir, &library);
        let before = entry_count(&db);
        let form = staged_form(&[(&fixture("freq.zip"), "FixtureFreq")], &[]);

        let (tx, _rx) = mpsc::channel::<EditMsg>();
        let report = apply_edits(&db, &library, &form, &tx).expect("the apply must report");

        assert!(report.added.is_empty(), "{:?}", report.added);
        assert_eq!(1, report.failed.len(), "{:?}", report.failed);
        assert!(!library.join("freq.zip").exists(), "the imported copy must be removed");
        assert_eq!(before, entry_count(&db));
        assert_eq!(1, dict_rows(&db).len());
    }

    /// Refuse before it moves.
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


    /// Never the last dictionary.
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

    /// Real bytes, not a constant.
    #[test]
    fn the_library_that_built_the_database_has_not_drifted() {
        let (dir, _guard) = edit_scratch("no_drift");
        let library = dir.join("library");
        let db = built_db(&dir, &library);

        let raw = read_source_hashes(&db).unwrap().expect("build-dict records what it read");
        assert!(raw.contains(r#""name": "terms.zip""#), "json.dumps spacing: {raw}");
        assert_eq!(None, drifted(&library, &db).unwrap(), "the two agree");
    }

    /// The dropped-in archive.
    #[test]
    fn an_archive_the_build_never_saw_is_reported_as_drift() {
        let (dir, _guard) = edit_scratch("drifted");
        let library = dir.join("library");
        let db = built_db(&dir, &library);
        std::fs::copy(fixture("freq.zip"), library.join("freq.zip")).unwrap();

        let text = drifted(&library, &db).unwrap().expect("a dropped-in archive is drift");

        assert!(text.contains("freq.zip"), "{text}");
        assert!(text.contains("chibipop build-dict --library"), "{text}");
    }
}
