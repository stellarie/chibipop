//! Two threads: pump and worker.

use crate::anki;
use crate::config::Config;
use crate::geom::{in_sticky, place_popup, PhysPoint, PhysRect, ScanDisplay, ScanKind, ScanRect};
use crate::input::hooks::Hooks;
use crate::library::{Library, Pending};
use crate::lock::LibraryLock;
use crate::lookup::deconj::Deconjugator;
use crate::lookup::engine::LookupEngine;
use crate::lookup::model::Dictionary;
use crate::lookup::rules::load_rules;
use crate::lookup::sqlite::SqliteDictionary;
use crate::present::{self, DictInfo, Presentation, PresentConfig};
use crate::rebuild::{self, Progress};
use crate::settings::{self, SettingsForm};
use crate::text::layout::{CaptureSize, Orientation};
use crate::text::ocr::OcrTextSource;
use crate::ui::overlay::Overlay;
use crate::ui::render::{anki_button_label, max_scroll, AnkiPopupState, HitAction, Renderer};
use crate::ui::settings_window::{ApplyMode, SettingsClick, SettingsOutcome, SettingsWindow};
use crate::ui::theme::Theme;
use crate::ui::tray::{Tray, TrayCommand};
use crate::ui::window::{AnkiButton, CaptureExclusion, Popup};
use crate::update;
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
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

/// Wake the pump. +2 is tray's.
const WM_APP_CAPTURE_GUARD: u32 = WM_APP + 3;

/// Dupe check finished.
const WM_APP_ANKI: u32 = WM_APP + 4;

/// Add-note finished.
const WM_APP_ADD_NOTE: u32 = WM_APP + 5;

/// Settings op finished.
const WM_APP_SETTINGS: u32 = WM_APP + 6;

/// Anki deck/model detect done.
const WM_APP_ANKI_DETECT: u32 = WM_APP + 7;

/// Apply staging finished.
const WM_APP_APPLY: u32 = WM_APP + 8;

/// Background save finished.
const WM_APP_SAVED: u32 = WM_APP + 9;

/// Hide-ack wait, then capture.
const ACK_TIMEOUT: Duration = Duration::from_millis(250);

/// Pending-cursor poll, ms.
const DISPATCH_TICK_MS: u32 = 20;

/// Anchor-to-popup gap.
const POPUP_GAP: i32 = 12;

/// Not slop: UPSCALE 2 rounds.
const ANCHOR_JITTER_PX: i32 = 4;

/// Pixels per wheel notch.
const SCROLL_STEP_PX: i32 = 48;

/// Armed ticks before warning.
const ARM_WARN_TICKS: u32 = 250;

/// Rebuild progress poll, ms.
const REBUILD_TICK_MS: u32 = 100;

/// Over this, hooks stall.
const APPLY_BUDGET_MS: u128 = 50;


/// Staleness by id, no sentinel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct RequestId(u64);

/// Hover, drill-down, reload.
pub enum TriggerKind {
    Hover(PhysPoint),
    DrillDown(String),
    Reload(Box<WorkerSettings>),
}

/// What the worker owns.
pub struct WorkerSettings {
    pub max_passes: u8,
    pub prefer_vertical: bool,
    pub capture: CaptureSize,
    pub scan_alphanumeric: bool,
    pub present_cfg: PresentConfig,
    pub scan_display: ScanDisplay,
}

/// One gated cursor movement.
struct Trigger {
    kind: TriggerKind,
    id: RequestId,
}

/// One answer, carrying its id.
struct WorkerResult {
    id: RequestId,
    outcome: WorkerOutcome,
}

enum WorkerOutcome {
    /// No text, or no hits.
    Hide,
    /// Logged; never fatal.
    Failed(String),
    /// `scan` empty without debug.
    Ready {
        presentation: Box<Presentation>,
        anchor: PhysRect,
        /// Which axis the hold may grow.
        orientation: Orientation,
        /// What the top card matched.
        matched: Option<PhysRect>,
        scan: Vec<ScanRect>,
    },
    /// Kanji drill-down result.
    DrillDown(Box<Presentation>),
}

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

/// Popup out of one capture.
enum CaptureGuardMsg {
    /// Hide now; ack when done.
    Hide { ack: mpsc::Sender<()> },
    /// Undo a Hide. Fire-and-forget.
    Restore,
}

/// The worker's guard handle.
struct CaptureGuard {
    main_tid: u32,
    request_tx: mpsc::Sender<CaptureGuardMsg>,
}

impl CaptureGuard {
    /// Blocks until hidden.
    fn hide_for_capture(&self) {
        let (ack_tx, ack_rx) = mpsc::channel();
        if self.request_tx.send(CaptureGuardMsg::Hide { ack: ack_tx }).is_err() {
            return; // main thread gone - nothing left to hide.
        }
        self.wake_main_thread();
        if ack_rx.recv_timeout(ACK_TIMEOUT).is_err() {
            eprintln!(
                "chibipop: capture guard: hide was not acknowledged within {ACK_TIMEOUT:?}; \
                 capturing anyway - this capture may include the popup itself"
            );
        }
    }

    /// Undoes `hide_for_capture`.
    fn restore_after_capture(&self) {
        let _ = self.request_tx.send(CaptureGuardMsg::Restore);
        self.wake_main_thread();
    }

    /// Thread message, not window.
    fn wake_main_thread(&self) {
        unsafe {
            let _ = PostThreadMessageW(self.main_tid, WM_APP_CAPTURE_GUARD, WPARAM(0), LPARAM(0));
        }
    }
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

    // A run may hold it open.
    let staged_db = rebuild::staging_path(dict_path);
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
                // Built beside the live one.
                Ok(()) => match rebuild::promote(&staged_db, dict_path) {
                    Err(e) => {
                        undo_apply(&flight, &e);
                        let _ = std::fs::remove_file(&staged_db);
                        window.set_status(
                            "Another chibipop is running. Close it, then Apply again.",
                        );
                    }
                    Ok(()) => {
                        keep_apply(&flight, &window);
                        let updated = pending.take().unwrap_or_else(|| cfg.clone());
                        updated.save(config_path).with_context(|| {
                            format!("saving settings to {}", config_path.display())
                        })?;
                        println!("chibipop: rebuilt {}.", dict_path.display());
                        println!("chibipop: settings saved to {}.", config_path.display());
                        // A new dictionary: start it.
                        match start_run(config_path, dict_path) {
                            Ok(()) => println!("chibipop: starting."),
                            Err(e) => {
                                eprintln!("chibipop: could not start chibipop: {e:#}");
                                eprintln!("chibipop: the dictionary is ready - start it yourself.");
                            }
                        }
                        return Ok(());
                    }
                },
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
                match start_rebuild(&edited, &library, &staged_db) {
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
                if let Some(text) = rebuild::friendly(&line) {
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

/// Say nothing was changed.
fn report_failed_rebuild(w: &SettingsWindow, e: &anyhow::Error) {
    w.set_status("The rebuild failed. Your dictionary is unchanged.");
    eprintln!("chibipop: the rebuild failed: {e:#}");
    eprintln!("chibipop: the dictionary in use was not touched.");
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
    // The worker holds it open.
    let staged_db = rebuild::staging_path(dict_path);
    let db_path = dict_path.to_path_buf();
    let dict_path = dict_path.to_path_buf();
    let rules_path = rules_path.to_path_buf();

    let running = Arc::new(AtomicBool::new(true));
    let (trigger_tx, trigger_rx) = mpsc::channel::<Trigger>();
    let (result_tx, result_rx) = mpsc::channel::<WorkerResult>();
    let (startup_tx, startup_rx) = mpsc::channel::<Result<Vec<DictInfo>>>();
    // Unknown until Popup::create.
    let capture_guard_active = Arc::new(AtomicBool::new(false));
    let (capture_guard_tx, capture_guard_rx) = mpsc::channel::<CaptureGuardMsg>();

    // SAFETY: FFI call with no preconditions - always succeeds, returns the
    // id of whichever thread calls it.
    let main_tid = unsafe { GetCurrentThreadId() };
    let mut live = derive(&cfg);
    let w_present_cfg = live.present_cfg.clone();
    let w_max_ocr_passes = live.max_ocr_passes;
    let w_prefer_vertical = live.prefer_vertical;
    let w_capture = live.capture;
    let w_scan_alphanumeric = live.scan_alphanumeric;
    let w_scan_display = live.scan_display;
    let worker_running = Arc::clone(&running);
    let worker_capture_guard_active = Arc::clone(&capture_guard_active);

    let _worker = thread::spawn(move || {
        worker_main(
            dict_path,
            rules_path,
            w_present_cfg,
            w_max_ocr_passes,
            w_prefer_vertical,
            w_capture,
            w_scan_alphanumeric,
            w_scan_display,
            main_tid,
            trigger_rx,
            result_tx,
            worker_running,
            startup_tx,
            worker_capture_guard_active,
            capture_guard_tx,
        );
    });

    // Contract 3: DPI before GDI.
    let dicts: Vec<DictInfo> = startup_rx
        .recv()
        .context("worker thread ended before completing startup")??;

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

    // Always live; shown on demand.
    let anki_button = Some(
        AnkiButton::create(live.exclude_from_capture)
            .context("creating the Anki button window")?,
    );

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

    let _hooks = Hooks::install().context("installing the low-level input hooks")?;
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

    let mut next_id: u64 = 0;
    let mut latest_dispatched = RequestId(0);
    // Visible just before the Hide.
    //
    // Cleared by hides elsewhere.
    let capture_guard_prev_visible = std::cell::Cell::new(false);
    // Overlay's own visibility.
    let overlay_prev_visible = std::cell::Cell::new(false);
    // Anki button visibility.
    let btn_prev_visible = std::cell::Cell::new(false);
    // What is on screen now.
    let mut shown: Option<Shown> = None;
    let (anki_tx, anki_rx) = mpsc::channel::<AnkiDupeResult>();
    let (add_tx, add_rx) = mpsc::channel::<AddNoteResult>();
    let (settings_tx, settings_rx) = mpsc::channel::<String>();
    let (detect_tx, detect_rx) =
        mpsc::channel::<(Vec<String>, Vec<String>, Vec<String>)>();
    let (apply_tx, apply_rx) =
        mpsc::channel::<Result<(Pending, mpsc::Receiver<Progress>)>>();
    let (save_tx, save_rx) = mpsc::channel::<Result<()>>();
    // One writer at a time.
    let mut save_job: Option<thread::JoinHandle<()>> = None;
    let mut popup_gen: u64 = 0;
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
        Ok(w) => Some(w),
    };
    // Consecutive armed ticks.
    let mut armed_ticks: u32 = 0;
    // A rebuild in flight.
    let mut rebuild: Option<InFlight> = None;
    // Held while staging runs.
    let mut staging_lock: Option<LibraryLock> = None;
    let mut pending_cfg: Option<Config> = None;
    let mut promote: Option<PathBuf> = None;
    // The swap can still fail.
    let mut applied: Option<InFlight> = None;
    let mut restart_at_exit = false;

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
            let over_popup = shown.as_ref().is_some_and(|s| {
                s.popup.contains(cursor_pos)
            });
            let over_popup_or_btn = shown.as_ref().is_some_and(|s| {
                let btn_h = anki_button.as_ref()
                    .filter(|b| b.is_visible())
                    .map_or(0, |b| b.height_phys());
                let full = PhysRect { h: s.popup.h + btn_h, ..s.popup };
                full.contains(cursor_pos)
            });
            let armed = live.scroll_popup
                && over_popup
                && shown.as_ref().is_some_and(|s| s.content_h > s.view_h);
            Hooks::set_scroll_armed(armed);
            Hooks::set_click_armed(over_popup_or_btn);
            Hooks::set_add_armed(shown.is_some() && live.anki_enabled);

            if over_popup {
                if let Some(s) = shown.as_ref() {
                    let lx = cursor_pos.x - s.popup.x;
                    let ly = cursor_pos.y - s.popup.y;
                    let clickable = renderer.hit_test(lx, ly, s.scroll).is_some();
                    if clickable {
                        if let Ok(cur) = unsafe { LoadCursorW(None, IDC_HAND) } {
                            unsafe { SetCursor(Some(cur)) };
                        }
                    }
                }
            }

            armed_ticks = if armed { armed_ticks + 1 } else { 0 };
            if armed_ticks == ARM_WARN_TICKS {
                eprintln!(
                    "chibipop: the wheel has been captured for {}s (SCROLL_ARMED). If your \
                     scroll wheel is not working elsewhere, this is why - move the cursor off \
                     the popup, or set scroll_popup = false.",
                    (ARM_WARN_TICKS * DISPATCH_TICK_MS) / 1000
                );
            }

            let notches = Hooks::take_whole_notches();
            if notches != 0 {
                if let Some(s) = shown.as_mut() {
                    let span = max_scroll(s.content_h, s.view_h);
                    // Wheel-up is positive.
                    let step = notches.saturating_mul(SCROLL_STEP_PX);
                    let next = s.scroll.saturating_sub(step).clamp(0, span);
                    if next != s.scroll {
                        s.scroll = next;
                        let back = !s.history.is_empty();
                        let painted = renderer
                            .paint(&s.presentation, &theme, s.scroll, back, live.side_panel);
                        if let Err(e) = painted {
                            eprintln!("chibipop: repainting for scroll failed: {e:#}");
                        }
                    }
                }
            }

            if let Some(click) = Hooks::take_click() {
                if let Some(s) = shown.as_mut() {
                    let click_x = click.x - s.popup.x;
                    let click_y = click.y - s.popup.y;
                    let has_history = !s.history.is_empty();
                    if let Some(action) = renderer.hit_test(click_x, click_y, s.scroll) {
                        match action {
                            HitAction::ExpandEntry(i) => {
                                present::swap_top(&mut s.presentation, i, live.summary_chars);
                                match show_presentation(
                                    &popup,
                                    &mut renderer,
                                    &theme,
                                    live.max_height_percent,
                                    live.max_width_percent,
                                    &s.presentation,
                                    s.anchor,
                                    0,
                                    has_history,
                                    live.side_panel,
                                ) {
                                    Ok((rect, content_h, view_h)) => {
                                        s.popup = rect;
                                        s.scroll = 0;
                                        s.content_h = content_h;
                                        s.view_h = view_h;
                                        sync_anki_button(anki_button.as_ref(), Some(s), &theme);
                                    }
                                    Err(e) => {
                                        eprintln!("chibipop: repaint after swap failed: {e:#}");
                                    }
                                }
                            }
                            HitAction::DrillDown(ch) => {
                                next_id += 1;
                                latest_dispatched = RequestId(next_id);
                                let _ = trigger_tx.send(Trigger {
                                    kind: TriggerKind::DrillDown(ch),
                                    id: latest_dispatched,
                                });
                            }
                            HitAction::Back => {
                                pop_history(
                                    s, &popup, &mut renderer, &theme,
                                    live.max_height_percent, live.max_width_percent,
                                    anki_button.as_ref(), live.side_panel,
                                );
                            }
                        }
                    } else if click_y >= s.popup.h && live.anki_enabled {
                        // Below popup = button area.
                        start_add_to_anki(
                            s, &mut renderer, &theme,
                            &live.anki_url, &live.anki_deck, &live.anki_model,
                            &live.anki_field_map, &add_tx, main_tid, live.side_panel,
                        );
                        sync_anki_button(anki_button.as_ref(), Some(s), &theme);
                    }
                }
            }

            // Fallback: direct WM_LBUTTONDOWN.
            if anki_button.as_ref().is_some_and(|b| b.take_click()) {
                if let Some(s) = shown.as_mut() {
                    start_add_to_anki(
                        s, &mut renderer, &theme,
                        &live.anki_url, &live.anki_deck, &live.anki_model, &live.anki_field_map,
                        &add_tx, main_tid, live.side_panel,
                    );
                    sync_anki_button(anki_button.as_ref(), Some(s), &theme);
                }
            }

            if Hooks::take_add_hotkey() {
                if let Some(s) = shown.as_mut() {
                    start_add_to_anki(
                        s, &mut renderer, &theme,
                        &live.anki_url, &live.anki_deck, &live.anki_model, &live.anki_field_map,
                        &add_tx, main_tid, live.side_panel,
                    );
                    sync_anki_button(anki_button.as_ref(), Some(s), &theme);
                }
            }

            let has_hist = shown.as_ref().is_some_and(|s| !s.history.is_empty());
            Hooks::set_back_armed(has_hist);
            if Hooks::take_back() {
                if let Some(s) = shown.as_mut() {
                    pop_history(
                        s, &popup, &mut renderer, &theme,
                        live.max_height_percent, live.max_width_percent,
                        anki_button.as_ref(), live.side_panel,
                    );
                }
            }

            if let Some(w) = &settings {
                if staging_lock.is_some() {
                    // Not while it copies files.
                    let _ = w.take_outcome();
                } else if rebuild.is_some() {
                    // Not while the child writes.
                    let _ = w.take_outcome();
                    // Taken only when finished.
                    let done = rebuild.as_ref().and_then(|f| pump_rebuild(&f.rx, w));
                    if let Some(built) = done {
                        let flight = rebuild.take();
                        w.set_busy(false);
                        match (built, flight) {
                            (_, None) => {}
                            (Ok(()), Some(flight)) => {
                                let updated =
                                    pending_cfg.take().unwrap_or_else(|| cfg.clone());
                                if let Err(e) = updated.save(config_path) {
                                    eprintln!("chibipop: could not save settings to {}: {e:#}",
                                              config_path.display());
                                    let _ = std::fs::remove_file(&staged_db);
                                    undo_apply(&flight, &e);
                                    w.set_status("Settings could not be saved. Nothing changed.");
                                } else {
                                    // The swap needs it closed.
                                    w.set_status("Dictionary rebuilt. Restarting chibipop.");
                                    w.clear_staged();
                                    promote = Some(staged_db.clone());
                                    applied = Some(flight);
                                    restart_at_exit = true;
                                    unsafe { PostQuitMessage(0) };
                                }
                            }
                            (Err(e), Some(flight)) => {
                                undo_apply(&flight, &e);
                                report_failed_rebuild(w, &e);
                            }
                        }
                    }
                } else {
                    match w.take_outcome() {
                        // Tray remains; just hide.
                        Some(SettingsOutcome::Cancel) => settings = None,
                        // Already on the main thread.
                        Some(SettingsOutcome::Quit) => unsafe { PostQuitMessage(0) },
                        Some(SettingsOutcome::Apply) => {
                            let t0 = std::time::Instant::now();
                            let edited = w.read(&form_with_library(&cfg, &dicts, &library));
                            let updated = settings::apply_to(&edited, &cfg);
                            // Never half-apply.
                            if edited.has_staged() {
                                match LibraryLock::acquire(&library) {
                                    Err(e) => refuse_apply(w, &e),
                                    Ok(lock) => {
                                        begin_apply(w);
                                        pending_cfg = Some(updated);
                                        staging_lock = Some(lock);
                                        let dir = library.clone();
                                        let out = staged_db.clone();
                                        let tx = apply_tx.clone();
                                        thread::spawn(move || {
                                            let result = stage_and_spawn(&edited, &dir, &out);
                                            let _ = tx.send(result);
                                            // SAFETY: wakes the pump thread.
                                            unsafe {
                                                let _ = PostThreadMessageW(
                                                    main_tid, WM_APP_APPLY,
                                                    WPARAM(0), LPARAM(0),
                                                );
                                            }
                                        });
                                    }
                                }
                            } else {
                                live = derive(&updated);
                                apply_live(&live, &popup, overlay.as_ref(),
                                           anki_button.as_ref(), &mut theme,
                                           &capture_guard_active);
                                next_id += 1;
                                latest_dispatched = RequestId(next_id);
                                let _ = trigger_tx.send(Trigger {
                                    kind: TriggerKind::Reload(Box::new(worker_settings(&live))),
                                    id: latest_dispatched,
                                });
                                let clamped = settings::clamp_notice(&edited, &updated);
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
            if Hooks::take_hide() {
                // An in-flight hit re-shows it.
                next_id += 1;
                latest_dispatched = RequestId(next_id);
                // Restore would re-show it.
                capture_guard_prev_visible.set(false);
                overlay_prev_visible.set(false);
                btn_prev_visible.set(false);
                if shown.is_some() {
                    let _ = popup.hide();
                    if let Some(b) = &anki_button {
                        b.hide();
                    }
                    if let Some(ov) = overlay.as_ref() {
                        ov.hide();
                    }
                    shown = None;
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
                // Spec D3: hold, do not resolve.
                let frozen = shown.as_ref().is_some_and(|s| {
                    let btn_h = anki_button.as_ref()
                        .filter(|b| b.is_visible())
                        .map_or(0, |b| b.height_phys());
                    let sticky_rect = PhysRect {
                        h: s.popup.h + btn_h,
                        ..s.popup
                    };
                    in_sticky(cursor, s.hold, sticky_rect)
                });
                if !frozen {
                    next_id += 1;
                    latest_dispatched = RequestId(next_id);
                    let _ = trigger_tx.send(Trigger {
                        kind: TriggerKind::Hover(cursor),
                        id: latest_dispatched,
                    });
                }
            }
        } else if msg.message == WM_APP_RESULT {
            // Only the freshest queued.
            let mut freshest: Option<WorkerResult> = None;
            while let Ok(r) = result_rx.try_recv() {
                freshest = Some(r);
            }
            if let Some(result) = freshest {
                if result.id < latest_dispatched {
                    // Superseded, not an error.
                } else if let WorkerOutcome::DrillDown(pres) = result.outcome {
                    if let Some(s) = shown.as_mut() {
                        push_drilldown(
                            s, *pres, &popup, &mut renderer, &theme,
                            live.max_height_percent, live.max_width_percent,
                            live.anki_enabled, live.side_panel,
                        );
                        sync_anki_button(anki_button.as_ref(), Some(s), &theme);
                        if live.anki_enabled {
                            popup_gen = popup_gen.wrapping_add(1);
                            s.gen = popup_gen;
                            let mut exprs: Vec<String> = Vec::new();
                            if let Some(card) = &s.presentation.top {
                                if let Some(e) = card.written.as_deref().or(card.reading.as_deref()) {
                                    exprs.push(e.to_string());
                                }
                            }
                            if !exprs.is_empty() {
                                let url = live.anki_url.clone();
                                let deck = live.anki_deck.clone();
                                let model = live.anki_model.clone();
                                let gen = popup_gen;
                                let tx = anki_tx.clone();
                                thread::spawn(move || {
                                    let refs: Vec<&str> = exprs.iter().map(|s| s.as_str()).collect();
                                    let dupes = match anki::find_duplicates(&url, &deck, &model, &refs) {
                                        Ok(d) => Some(d),
                                        Err(e) => {
                                            eprintln!("chibipop: dupe check failed: {e:#}");
                                            None
                                        }
                                    };
                                    let _ = tx.send(AnkiDupeResult { gen, dupes });
                                    // SAFETY: wakes the pump.
                                    unsafe {
                                        let _ = PostThreadMessageW(
                                            main_tid, WM_APP_ANKI, WPARAM(0), LPARAM(0),
                                        );
                                    }
                                });
                            }
                        }
                    }
                } else {
                    let new_popup = handle_worker_outcome(
                        &popup,
                        &mut renderer,
                        &theme,
                        live.max_height_percent,
                        live.max_width_percent,
                        overlay.as_ref(),
                        &mut shown,
                        result.outcome,
                        live.show_lookup_log,
                        live.anki_enabled,
                        live.side_panel,
                    );
                    sync_anki_button(anki_button.as_ref(), shown.as_ref(), &theme);
                    if new_popup && live.anki_enabled {
                        popup_gen = popup_gen.wrapping_add(1);
                        let mut exprs: Vec<String> = Vec::new();
                        if let Some(s) = shown.as_mut() {
                            s.gen = popup_gen;
                            if let Some(card) = &s.presentation.top {
                                if let Some(e) = card.written.as_deref().or(card.reading.as_deref()) {
                                    exprs.push(e.to_string());
                                }
                            }
                            for row in &s.presentation.collapsed {
                                if let Some(e) = row.written.as_deref().or(row.reading.as_deref()) {
                                    exprs.push(e.to_string());
                                }
                            }
                        }
                        if !exprs.is_empty() {
                            let url = live.anki_url.clone();
                            let deck = live.anki_deck.clone();
                            let model = live.anki_model.clone();
                            let gen = popup_gen;
                            let tx = anki_tx.clone();
                            thread::spawn(move || {
                                let refs: Vec<&str> = exprs.iter().map(|s| s.as_str()).collect();
                                let dupes = match anki::find_duplicates(&url, &deck, &model, &refs) {
                                    Ok(d) => Some(d),
                                    Err(e) => {
                                        eprintln!("chibipop: dupe check failed: {e:#}");
                                        None
                                    }
                                };
                                let _ = tx.send(AnkiDupeResult { gen, dupes });
                                // SAFETY: wakes the pump.
                                unsafe {
                                    let _ = PostThreadMessageW(
                                        main_tid, WM_APP_ANKI, WPARAM(0), LPARAM(0),
                                    );
                                }
                            });
                        }
                    }
                }
            }
        } else if msg.message == WM_APP_ANKI {
            while let Ok(result) = anki_rx.try_recv() {
                if let Some(s) = shown.as_mut() {
                    if s.gen == result.gen {
                        s.anki.checking = false;
                        match result.dupes {
                            Some(dupes) => {
                                s.anki.connected = true;
                                s.anki.dupes = dupes;
                            }
                            None => {
                                s.anki.connected = false;
                            }
                        }
                        let back = !s.history.is_empty();
                        match show_presentation(
                            &popup,
                            &mut renderer,
                            &theme,
                            live.max_height_percent,
                            live.max_width_percent,
                            &s.presentation,
                            s.anchor,
                            s.scroll,
                            back,
                            live.side_panel,
                        ) {
                            Ok((rect, content_h, view_h)) => {
                                s.popup = rect;
                                s.content_h = content_h;
                                s.view_h = view_h;
                                let m = max_scroll(s.content_h, s.view_h);
                                if s.scroll > m { s.scroll = m; }
                                sync_anki_button(anki_button.as_ref(), Some(s), &theme);
                            }
                            Err(e) => {
                                eprintln!("chibipop: repaint for dupe markers failed: {e:#}");
                            }
                        }
                    }
                }
            }
        } else if msg.message == WM_APP_ADD_NOTE {
            while let Ok(result) = add_rx.try_recv() {
                if let Some(s) = shown.as_mut() {
                    s.anki.adding = false;
                    if let Some(e) = result.err {
                        eprintln!("chibipop: add to Anki failed: {e}");
                        s.anki.failed = true;
                    } else {
                        s.anki.added.insert(result.expr);
                    }
                    let back = !s.history.is_empty();
                    match show_presentation(
                        &popup,
                        &mut renderer,
                        &theme,
                        live.max_height_percent,
                        live.max_width_percent,
                        &s.presentation,
                        s.anchor,
                        s.scroll,
                        back,
                        live.side_panel,
                    ) {
                        Ok((rect, content_h, view_h)) => {
                            s.popup = rect;
                            s.content_h = content_h;
                            s.view_h = view_h;
                            let m = max_scroll(s.content_h, s.view_h);
                            if s.scroll > m { s.scroll = m; }
                            sync_anki_button(anki_button.as_ref(), Some(s), &theme);
                        }
                        Err(e) => {
                            eprintln!("chibipop: repaint after add failed: {e:#}");
                        }
                    }
                }
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
        } else if msg.message == WM_APP_APPLY {
            while let Ok(result) = apply_rx.try_recv() {
                let Some(lock) = staging_lock.take() else { continue };
                let Some(w) = &settings else { continue };
                w.set_busy(false);
                match result {
                    Ok((pending, rx)) => {
                        begin_rebuild(w);
                        rebuild = Some(InFlight { pending, rx, _lock: lock });
                    }
                    Err(e) => {
                        pending_cfg = None;
                        refuse_apply(w, &e);
                    }
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
                TrayCommand::OpenSettings => {
                    if let Some(w) = &settings {
                        w.focus();
                    } else {
                        let form = form_with_library(&cfg, &dicts, &library);
                        let stale = settings::stale_order_entries(&cfg, &dicts);
                        match SettingsWindow::open(&form, &stale, ApplyMode::Live) {
                            // Never fatal.
                            Err(e) => eprintln!("chibipop: opening settings failed: {e:#}"),
                            Ok(w) => settings = Some(w),
                        }
                    }
                }
                TrayCommand::Quit => unsafe {
                    // Already on the main thread.
                    PostQuitMessage(0);
                },
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
    if let Some(staged) = promote {
        if let Ok(()) = rebuild::promote(&staged, &db_path) {
            if let Some(flight) = &applied {
                let _ = flight.pending.commit();
            }
        }
    }
    if restart_at_exit {
        let _ = restart_self();
    }
    // exit(0) kills it mid-write.
    join_save(&mut save_job);
    std::process::exit(0)
}

/// Newest hover; all reloads.
fn drain(first: Trigger, rx: &mpsc::Receiver<Trigger>) -> (Option<Trigger>, Vec<WorkerSettings>) {
    let mut reloads = Vec::new();
    let mut hover = None;
    let mut take = |t: Trigger| match t.kind {
        TriggerKind::Reload(s) => reloads.push(*s),
        _ => hover = Some(t),
    };
    take(first);
    while let Ok(next) = rx.try_recv() {
        take(next);
    }
    (hover, reloads)
}

/// Serves triggers, owns OCR.
#[allow(clippy::too_many_arguments)]
fn worker_main(
    dict_path: PathBuf,
    rules_path: PathBuf,
    mut present_cfg: PresentConfig,
    max_ocr_passes: u8,
    prefer_vertical: bool,
    capture: CaptureSize,
    scan_alphanumeric: bool,
    mut scan_display: ScanDisplay,
    main_tid: u32,
    trigger_rx: mpsc::Receiver<Trigger>,
    result_tx: mpsc::Sender<WorkerResult>,
    running: Arc<AtomicBool>,
    startup_tx: mpsc::Sender<Result<Vec<DictInfo>>>,
    capture_guard_active: Arc<AtomicBool>,
    capture_guard_tx: mpsc::Sender<CaptureGuardMsg>,
) {
    let built = OcrTextSource::new(max_ocr_passes, prefer_vertical, capture, scan_alphanumeric);
    let mut ocr = match built.context("creating the OCR text source") {
        Ok(o) => o,
        Err(e) => {
            let _ = startup_tx.send(Err(e));
            return;
        }
    };
    let dict = match SqliteDictionary::open(&dict_path).with_context(|| {
        format!(
            "opening {} - add dictionaries in the settings window",
            dict_path.display()
        )
    }) {
        Ok(d) => d,
        Err(e) => {
            let _ = startup_tx.send(Err(e));
            return;
        }
    };
    let rules = match load_rules(&rules_path) {
        Ok(r) => r,
        Err(e) => {
            let _ = startup_tx.send(Err(e));
            return;
        }
    };
    let engine = LookupEngine::new(Deconjugator::new(rules));

    // Decision 2: read once.
    let dicts: Vec<DictInfo> = match dict.dicts().context("reading dictionary identities") {
        Ok(d) => d,
        Err(e) => {
            let _ = startup_tx.send(Err(e));
            return;
        }
    };

    let capture_guard = CaptureGuard { main_tid, request_tx: capture_guard_tx };

    // An Arc would be ceremony.
    if startup_tx.send(Ok(dicts.clone())).is_err() {
        return; // main thread gave up waiting; nothing left to do.
    }

    // Sender dropped: shutdown.
    while let Ok(first) = trigger_rx.recv() {
        let (hover, reloads) = drain(first, &trigger_rx);
        for s in reloads {
            ocr.apply_settings(s.max_passes, s.prefer_vertical, s.capture, s.scan_alphanumeric);
            present_cfg = s.present_cfg;
            scan_display = s.scan_display;
        }
        if !running.load(Ordering::SeqCst) {
            break;
        }
        let Some(trigger) = hover else {
            continue;
        };

        // Fresh, so no ordering rule.
        let guard = if capture_guard_active.load(Ordering::SeqCst) {
            Some(&capture_guard)
        } else {
            None
        };

        // One bad frame is not fatal.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match trigger.kind {
                TriggerKind::Hover(cursor) => resolve_trigger(
                    &ocr,
                    &dict,
                    &engine,
                    &dicts,
                    &present_cfg,
                    cursor,
                    guard,
                    scan_display,
                ),
                TriggerKind::DrillDown(ref text) => resolve_drilldown(
                    &dict,
                    &engine,
                    &dicts,
                    &present_cfg,
                    text,
                ),
                TriggerKind::Reload(_) => {
                    WorkerOutcome::Failed("a reload reached the hover path".to_string())
                }
            }
        }))
        .unwrap_or_else(|_| WorkerOutcome::Failed("a hover lookup panicked".to_string()));

        if result_tx.send(WorkerResult { id: trigger.id, outcome }).is_err() {
            break; // main thread gone
        }
        unsafe {
            let _ = PostThreadMessageW(main_tid, WM_APP_RESULT, WPARAM(0), LPARAM(0));
        }
    }
}

/// One hover: OCR to present.
#[allow(clippy::too_many_arguments)]
fn resolve_trigger(
    ocr: &OcrTextSource,
    dict: &SqliteDictionary,
    engine: &LookupEngine,
    dicts: &[DictInfo],
    present_cfg: &PresentConfig,
    cursor: PhysPoint,
    capture_guard: Option<&CaptureGuard>,
    scan_display: ScanDisplay,
) -> WorkerOutcome {
    let raw = match capture_guard {
        Some(guard) => {
            guard.hide_for_capture();
            let r = ocr.resolve_at_tiled_scanned(cursor, scan_display.captures);
            guard.restore_after_capture();
            r
        }
        None => ocr.resolve_at_tiled_scanned(cursor, scan_display.captures),
    };
    let (resolved, mut scan) = match raw {
        Ok((Some(r), scan)) => (r, scan),
        Ok((None, _)) => return WorkerOutcome::Hide,
        Err(e) => return WorkerOutcome::Failed(format!("{e:#}")),
    };

    let text = &resolved.span.text[resolved.span.cursor_byte_offset..];
    let hits = match engine.run(dict, text) {
        Ok(h) => h,
        Err(e) => return WorkerOutcome::Failed(format!("{e:#}")),
    };
    if hits.is_empty() {
        return WorkerOutcome::Hide;
    }

    let presentation = present::build(&hits, dicts, present_cfg);
    let matched = present::match_highlight(&resolved.span, presentation.top.as_ref());
    if scan_display.highlight {
        if let Some(rect) = matched {
            scan.push(ScanRect { rect, kind: ScanKind::Match });
        }
    }
    WorkerOutcome::Ready {
        presentation: Box::new(presentation),
        anchor: resolved.span.anchor,
        orientation: resolved.orientation,
        matched,
        scan,
    }
}

/// Dict lookup without OCR.
fn resolve_drilldown(
    dict: &SqliteDictionary,
    engine: &LookupEngine,
    dicts: &[DictInfo],
    present_cfg: &PresentConfig,
    text: &str,
) -> WorkerOutcome {
    let hits = match engine.run(dict, text) {
        Ok(h) => h,
        Err(e) => return WorkerOutcome::Failed(format!("{e:#}")),
    };
    if hits.is_empty() {
        return WorkerOutcome::Hide;
    }
    let p = present::build(&hits, dicts, present_cfg);
    WorkerOutcome::DrillDown(Box::new(p))
}


/// Saved for back navigation.
struct HistoryEntry {
    presentation: Presentation,
    anki: AnkiPopupState,
}

/// On screen. Never outlive it.
struct Shown {
    /// The hovered glyph's box.
    anchor: PhysRect,
    /// Stored, never re-derived.
    popup: PhysRect,
    /// Where the cursor may roam.
    hold: PhysRect,
    presentation: Presentation,
    /// Content offset; 0 is the top.
    scroll: i32,
    /// Natural height, unclamped.
    content_h: i32,
    /// The window's own height.
    view_h: i32,
    /// Stale-result guard.
    gen: u64,
    anki: AnkiPopupState,
    /// Drill-down stack.
    history: Vec<HistoryEntry>,
}

/// True if a new popup was shown.
#[allow(clippy::too_many_arguments)]
fn handle_worker_outcome(
    popup: &Popup,
    renderer: &mut Renderer,
    theme: &Theme,
    max_height_percent: i32,
    max_width_percent: i32,
    overlay: Option<&Overlay>,
    shown: &mut Option<Shown>,
    outcome: WorkerOutcome,
    log: bool,
    anki_enabled: bool,
    side_panel: bool,
) -> bool {
    match outcome {
        WorkerOutcome::Hide => {
            let _ = popup.hide();
            if let Some(ov) = overlay {
                ov.hide();
            }
            *shown = None;
            false
        }
        WorkerOutcome::Failed(msg) => {
            eprintln!("chibipop: hover lookup failed: {msg}");
            let _ = popup.hide();
            if let Some(ov) = overlay {
                ov.hide();
            }
            *shown = None;
            false
        }
        WorkerOutcome::Ready { presentation, anchor, orientation, matched, scan } => {
            if shown.as_ref().is_some_and(|prev| same_content(prev, &presentation, anchor)) {
                return false;
            }
            if log {
                if let Some(card) = &presentation.top {
                    let head = card.written.clone()
                        .or_else(|| card.reading.clone())
                        .unwrap_or_default();
                    println!("{head}  match={}", card.match_len);
                }
            }
            let anki = AnkiPopupState {
                dupes: HashSet::new(),
                added: HashSet::new(),
                enabled: anki_enabled,
                adding: false,
                checking: anki_enabled,
                connected: anki_enabled,
                failed: false,
            };
            match show_presentation(
                popup,
                renderer,
                theme,
                max_height_percent,
                max_width_percent,
                &presentation,
                anchor,
                0,
                false,
                side_panel,
            ) {
                Err(e) => {
                    eprintln!("chibipop: showing the popup failed: {e:#}");
                    let _ = popup.hide();
                    if let Some(ov) = overlay {
                        ov.hide();
                    }
                    *shown = None;
                    Hooks::set_scroll_armed(false);
                    Hooks::set_click_armed(false);
                    false
                }
                Ok((rect, content_h, view_h)) => {
                    Hooks::discard_scroll();
                    *shown = Some(Shown {
                        anchor,
                        popup: rect,
                        hold: hold_region(anchor, matched, orientation),
                        presentation: *presentation,
                        scroll: 0,
                        content_h,
                        view_h,
                        gen: 0,
                        anki,
                        history: Vec::new(),
                    });
                    if let Some(ov) = overlay {
                        if let Err(e) = ov.show_rects(&scan, theme) {
                            eprintln!("chibipop: showing the scan overlay failed: {e:#}");
                        }
                    }
                    true
                }
            }
        }
        WorkerOutcome::DrillDown(_) => false,
    }
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

/// Same guard as the click path.
#[allow(clippy::too_many_arguments)]
fn start_add_to_anki(
    s: &mut Shown,
    renderer: &mut Renderer,
    theme: &Theme,
    anki_url: &str,
    anki_deck: &str,
    anki_model: &str,
    anki_field_map: &[crate::config::FieldMapping],
    add_tx: &mpsc::Sender<AddNoteResult>,
    main_tid: u32,
    side_panel: bool,
) {
    let info = s.presentation.top.as_ref().map(|card| {
        let expr = card.written.as_deref()
            .or(card.reading.as_deref())
            .unwrap_or("")
            .to_string();
        let fields = anki::fields_from_card(card, &card.blocks);
        (expr, fields)
    });
    let Some((expr, fields)) = info else { return };
    if s.anki.adding || s.anki.added.contains(&expr) {
        return;
    }
    s.anki.adding = true;
    s.anki.failed = false;
    let back = !s.history.is_empty();
    if let Err(e) = renderer.paint(&s.presentation, theme, s.scroll, back, side_panel) {
        eprintln!("chibipop: repaint for adding failed: {e:#}");
    }
    let url = anki_url.to_string();
    let deck = anki_deck.to_string();
    let model = anki_model.to_string();
    let field_map = anki_field_map.to_vec();
    let tx = add_tx.clone();
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
}

/// Would it redraw the same?
fn same_content(prev: &Shown, new: &Presentation, anchor: PhysRect) -> bool {
    prev.presentation == *new
        && (prev.anchor.x - anchor.x).abs() <= ANCHOR_JITTER_PX
        && (prev.anchor.y - anchor.y).abs() <= ANCHOR_JITTER_PX
}

/// Match one axis, slack other.
fn hold_region(anchor: PhysRect, matched: Option<PhysRect>, orientation: Orientation) -> PhysRect {
    let span = matched.unwrap_or(anchor);
    match orientation {
        Orientation::Horizontal => PhysRect {
            x: span.x,
            y: anchor.y - anchor.h / 2,
            w: span.w,
            h: anchor.h * 2,
        },
        Orientation::Vertical => PhysRect {
            x: anchor.x - anchor.w / 2,
            y: span.y,
            w: anchor.w * 2,
            h: span.h,
        },
    }
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

fn restart_self() -> Result<()> {
    let exe = std::env::current_exe().context("locating this executable")?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::Command::new(exe)
        .args(args)
        .spawn()
        .context("spawning the replacement process")?;
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
fn sync_anki_button(btn: Option<&AnkiButton>, shown: Option<&Shown>, theme: &Theme) {
    let Some(btn) = btn else { return };
    let Some(s) = shown else {
        btn.hide();
        return;
    };
    let Some((text, color)) = anki_button_label(&s.presentation, theme, &s.anki) else {
        btn.hide();
        return;
    };
    let r = PhysRect {
        x: s.popup.x,
        y: s.popup.y + s.popup.h,
        w: s.popup.w,
        h: btn.height_phys(),
    };
    if let Err(e) = btn.show_at(r) {
        eprintln!("chibipop: positioning the Anki button failed: {e:#}");
        return;
    }
    btn.render(&text, color, theme);
}

/// Pushes current, replaces.
#[allow(clippy::too_many_arguments)]
fn push_drilldown(
    s: &mut Shown,
    presentation: Presentation,
    popup: &Popup,
    renderer: &mut Renderer,
    theme: &Theme,
    max_h_pct: i32,
    max_w_pct: i32,
    anki_enabled: bool,
    side_panel: bool,
) {
    s.history.push(HistoryEntry {
        presentation: s.presentation.clone(),
        anki: s.anki.clone(),
    });
    s.presentation = presentation;
    s.anki = AnkiPopupState {
        dupes: HashSet::new(),
        added: HashSet::new(),
        enabled: anki_enabled,
        adding: false,
        checking: anki_enabled,
        connected: anki_enabled,
        failed: false,
    };
    s.scroll = 0;
    match show_presentation(
        popup, renderer, theme,
        max_h_pct, max_w_pct,
        &s.presentation, s.anchor,
        0, true, side_panel,
    ) {
        Ok((rect, content_h, view_h)) => {
            s.popup = rect;
            s.content_h = content_h;
            s.view_h = view_h;
        }
        Err(e) => {
            eprintln!("chibipop: drill-down repaint failed: {e:#}");
        }
    }
}

/// Pops the history stack.
#[allow(clippy::too_many_arguments)]
fn pop_history(
    s: &mut Shown,
    popup: &Popup,
    renderer: &mut Renderer,
    theme: &Theme,
    max_h_pct: i32,
    max_w_pct: i32,
    anki_button: Option<&AnkiButton>,
    side_panel: bool,
) {
    let Some(entry) = s.history.pop() else { return };
    s.presentation = entry.presentation;
    s.anki = entry.anki;
    s.scroll = 0;
    let back = !s.history.is_empty();
    match show_presentation(
        popup, renderer, theme,
        max_h_pct, max_w_pct,
        &s.presentation, s.anchor,
        0, back, side_panel,
    ) {
        Ok((rect, content_h, view_h)) => {
            s.popup = rect;
            s.content_h = content_h;
            s.view_h = view_h;
            sync_anki_button(anki_button, Some(s), theme);
        }
        Err(e) => {
            eprintln!("chibipop: back repaint failed: {e:#}");
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
    }
}

/// What the worker reloads.
fn worker_settings(live: &LiveSettings) -> WorkerSettings {
    WorkerSettings {
        max_passes: live.max_ocr_passes,
        prefer_vertical: live.prefer_vertical,
        capture: live.capture,
        scan_alphanumeric: live.scan_alphanumeric,
        present_cfg: live.present_cfg.clone(),
        scan_display: live.scan_display,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PopupConfig;
    use crate::present::Card;

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

    fn presentation_of(written: &str) -> Presentation {
        let card = Card {
            written: Some(written.to_string()),
            reading: None,
            pos: vec![],
            freq: None,
            blocks: vec![],
            match_len: 2,
        };
        Presentation {
            top: Some(card.clone()),
            collapsed: vec![],
            all_cards: vec![card],
        }
    }

    fn shown_of(written: &str, anchor: PhysRect) -> Shown {
        Shown {
            anchor,
            popup: PhysRect { x: anchor.x, y: anchor.y + anchor.h + POPUP_GAP, w: 420, h: 300 },
            hold: anchor,
            presentation: presentation_of(written),
            scroll: 0,
            content_h: 300,
            view_h: 300,
            gen: 0,
            anki: AnkiPopupState::disabled(),
            history: Vec::new(),
        }
    }

    /// UPSCALE 2 jitters each edge.
    #[test]
    fn an_equal_card_with_a_jittered_anchor_is_the_same_content() {
        let a = PhysRect { x: 100, y: 200, w: 26, h: 27 };
        let prev = shown_of("宿舎", a);
        let jittered = PhysRect { x: 101, y: 199, w: 26, h: 27 };
        assert!(same_content(&prev, &presentation_of("宿舎"), jittered));
    }

    /// One word twice; it must move.
    #[test]
    fn an_equal_card_that_moved_is_not_the_same_content() {
        let a = PhysRect { x: 100, y: 200, w: 26, h: 27 };
        let prev = shown_of("猫", a);
        let elsewhere = PhysRect { x: 700, y: 900, w: 26, h: 27 };
        assert!(!same_content(&prev, &presentation_of("猫"), elsewhere));
    }

    #[test]
    fn a_different_card_at_the_same_anchor_is_not_the_same_content() {
        let a = PhysRect { x: 100, y: 200, w: 26, h: 27 };
        let prev = shown_of("宿舎", a);
        assert!(!same_content(&prev, &presentation_of("駅長"), a));
    }

    /// One word, one popup.
    #[test]
    fn the_hold_region_covers_the_whole_matched_word() {
        let anchor = PhysRect { x: 3010, y: 257, w: 27, h: 26 };
        // 通ってる matched 4 characters.
        let matched = PhysRect { x: 3007, y: 254, w: 120, h: 32 };
        let popup = PhysRect { x: 3007, y: 300, w: 420, h: 300 };

        // Same word, later glyphs.
        assert!(in_sticky(PhysPoint { x: 3051, y: 270 }, matched, popup));
        assert!(in_sticky(PhysPoint { x: 3100, y: 270 }, matched, popup));
        // Past the match: re-resolve.
        assert!(!in_sticky(PhysPoint { x: 3200, y: 270 }, matched, popup));
        // The anchor alone releases.
        assert!(!in_sticky(PhysPoint { x: 3051, y: 270 }, anchor, popup));
    }

    /// A 22px は resolves over 34px.
    #[test]
    fn the_hold_covers_the_vertical_slack_hit_scan_allows() {
        let anchor = PhysRect { x: 2704, y: 260, w: 24, h: 22 };
        let matched = Some(PhysRect { x: 2701, y: 257, w: 30, h: 28 });
        let hold = hold_region(anchor, matched, Orientation::Horizontal);

        assert!(hold.y <= 254, "must reach the measured top of the region, got {}", hold.y);
        assert!(hold.y + hold.h >= 288, "must reach the measured bottom");
        // The line above starts at 248.
        assert!(hold.y > 248, "reaching the line above would hold a stale popup");
    }

    /// 宿 resolves 8px past は's box.
    #[test]
    fn the_hold_never_widens_along_the_reading_axis() {
        let anchor = PhysRect { x: 2704, y: 260, w: 24, h: 22 };
        let matched = PhysRect { x: 2701, y: 257, w: 30, h: 28 };
        let hold = hold_region(anchor, Some(matched), Orientation::Horizontal);
        assert_eq!(matched.x, hold.x);
        assert_eq!(matched.w, hold.w);
        assert!(!hold.contains(PhysPoint { x: 2736, y: 271 }), "2736 resolves 宿");
    }

    /// Vertical: slack on x.
    #[test]
    fn the_hold_mirrors_for_vertical_text() {
        let anchor = PhysRect { x: 2860, y: 1650, w: 28, h: 25 };
        let matched = PhysRect { x: 2857, y: 1647, w: 34, h: 90 };
        let hold = hold_region(anchor, Some(matched), Orientation::Vertical);
        assert_eq!(matched.y, hold.y, "the reading axis keeps the match span");
        assert_eq!(matched.h, hold.h);
        assert_eq!(anchor.x - anchor.w / 2, hold.x, "slack goes across the column");
        assert_eq!(anchor.w * 2, hold.w);
    }

    /// No match still gets slack.
    #[test]
    fn the_hold_without_a_match_still_carries_its_slack() {
        let anchor = PhysRect { x: 100, y: 200, w: 26, h: 27 };
        let hold = hold_region(anchor, None, Orientation::Horizontal);
        assert_eq!(anchor.x, hold.x);
        assert_eq!(anchor.w, hold.w);
        assert!(hold.h > anchor.h, "must still tolerate perpendicular drift");
    }

    /// At tolerance yes, past it no.
    #[test]
    fn the_jitter_tolerance_is_inclusive_and_bounded() {
        let a = PhysRect { x: 100, y: 200, w: 26, h: 27 };
        let prev = shown_of("宿舎", a);
        let at = PhysRect { x: 100 + ANCHOR_JITTER_PX, y: 200, w: 26, h: 27 };
        let past = PhysRect { x: 100 + ANCHOR_JITTER_PX + 1, y: 200, w: 26, h: 27 };
        assert!(same_content(&prev, &presentation_of("宿舎"), at));
        assert!(!same_content(&prev, &presentation_of("宿舎"), past));
    }

    fn ws(passes: u8) -> WorkerSettings {
        WorkerSettings {
            max_passes: passes,
            prefer_vertical: false,
            capture: CaptureSize::default(),
            scan_alphanumeric: true,
            present_cfg: Config::default().present_config(),
            scan_display: ScanDisplay { captures: false, highlight: false },
        }
    }

    /// Newest hover; every reload.
    #[test]
    fn drain_keeps_the_newest_hover_and_every_reload() {
        let (tx, rx) = mpsc::channel::<Trigger>();
        let reload = TriggerKind::Reload(Box::new(ws(2)));
        tx.send(Trigger { kind: reload, id: RequestId(2) }).unwrap();
        let newer = TriggerKind::Hover(PhysPoint { x: 9, y: 9 });
        tx.send(Trigger { kind: newer, id: RequestId(3) }).unwrap();
        let older = TriggerKind::Hover(PhysPoint { x: 1, y: 1 });
        let first = Trigger { kind: older, id: RequestId(1) };
        let (hover, reloads) = drain(first, &rx);
        let hover = hover.expect("a hover survives");
        assert!(matches!(hover.kind, TriggerKind::Hover(p) if p.x == 9), "newest hover wins");
        assert_eq!(1, reloads.len(), "the reload must not be swallowed");
        assert_eq!(2, reloads[0].max_passes);
    }

    /// A reload alone still arrives.
    #[test]
    fn drain_returns_no_hover_when_only_a_reload_queued() {
        let (tx, rx) = mpsc::channel::<Trigger>();
        drop(tx);
        let first = Trigger { kind: TriggerKind::Reload(Box::new(ws(3))), id: RequestId(1) };
        let (hover, reloads) = drain(first, &rx);
        assert!(hover.is_none());
        assert_eq!(1, reloads.len());
        assert_eq!(3, reloads[0].max_passes);
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
}
