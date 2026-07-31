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
use crate::text::layout::Orientation;
use crate::text::ocr::OcrTextSource;
use crate::ui::overlay::Overlay;
use crate::ui::render::{max_scroll, Renderer};
use crate::ui::settings_window::{SettingsClick, SettingsOutcome, SettingsWindow};
use crate::ui::theme::Theme;
use crate::ui::tray::{Tray, TrayCommand};
use crate::ui::window::{CaptureExclusion, Popup};
use crate::update;
use anyhow::{Context, Result};
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
    DispatchMessageW, GetCursorPos, GetMessageW, IsDialogMessageW, IsWindowVisible, KillTimer,
    PostQuitMessage,
    PostThreadMessageW, SetTimer, ShowWindow, TranslateMessage, MSG, SW_HIDE, SW_SHOWNOACTIVATE,
    WM_APP, WM_TIMER,
};

/// Worker pushed a result.
const WM_APP_RESULT: u32 = WM_APP + 1;

/// Wake the pump. +2 is tray's.
const WM_APP_CAPTURE_GUARD: u32 = WM_APP + 3;

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


/// Staleness by id, no sentinel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct RequestId(u64);

/// One gated cursor movement.
struct Trigger {
    cursor: PhysPoint,
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
        presentation: Presentation,
        anchor: PhysRect,
        /// Which axis the hold may grow.
        orientation: Orientation,
        /// What the top card matched.
        matched: Option<PhysRect>,
        scan: Vec<ScanRect>,
    },
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
    let window =
        SettingsWindow::open(&form, &stale).context("opening the settings window")?;

    // A run may hold it open.
    let staged_db = rebuild::staging_path(dict_path);
    let mut rebuild: Option<InFlight> = None;
    let mut pending: Option<Config> = None;
    let mut tick = 0usize;

    let mut msg = MSG::default();
    // SAFETY: `msg` is this loop's own stack storage, and `window` is alive
    // for the whole loop - it is dropped only after this function returns.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        // No hooks, nothing to disarm.
        window.pump(|| {});

        // Dialog keys first, as in run.
        if !unsafe { IsDialogMessageW(window.hwnd(), &msg) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        service_settings_click(&window);

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
            // Quit and dismiss both end it.
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
    let pending = settings::stage_into_library(form, dir)?;
    match rebuild::spawn(dir, out) {
        Ok(rx) => Ok(InFlight { pending, rx, _lock: lock }),
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

/// Runs the Anki/update click.
fn service_settings_click(w: &SettingsWindow) {
    match w.take_click() {
        Some(SettingsClick::AnkiTest) => {
            let url = w.anki_url();
            match anki::check_connection(&url) {
                Ok(true) => w.set_status("AnkiConnect is reachable."),
                Ok(false) => w.set_status("AnkiConnect did not respond."),
                Err(e) => w.set_status(&format!("Anki test failed: {e:#}")),
            }
        }
        Some(SettingsClick::CheckUpdate) => match update::check(env!("CARGO_PKG_VERSION")) {
            Ok(None) => w.set_status("You already have the latest version."),
            Ok(Some(release)) => match update::download_and_replace(&release) {
                Ok(()) => {
                    w.set_status(&format!("Updated to {}. Restart to use it.", release.tag));
                }
                Err(e) => w.set_status(&format!("Update to {} failed: {e:#}", release.tag)),
            },
            Err(e) => w.set_status(&format!("Update check failed: {e:#}")),
        },
        None => {}
    }
}

/// Run until the user quits.
pub fn run(cfg: Config, dict_path: &Path, rules_path: &Path, config_path: &Path) -> Result<()> {
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
    let present_cfg = cfg.present_config();
    let max_ocr_passes = cfg.ocr.max_ocr_passes;
    let scan_display = ScanDisplay {
        captures: cfg.debug.show_scan_region,
        highlight: cfg.popup.highlight_match,
    };
    let worker_running = Arc::clone(&running);
    let worker_capture_guard_active = Arc::clone(&capture_guard_active);

    let worker_handle = thread::spawn(move || {
        worker_main(
            dict_path,
            rules_path,
            present_cfg,
            max_ocr_passes,
            scan_display,
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

    let popup = Popup::create(cfg.popup.exclude_from_capture).context("creating the popup window")?;

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
    let overlay = if scan_display.any() {
        match Overlay::create(cfg.popup.exclude_from_capture) {
            Ok(o) => Some(o),
            Err(e) => {
                eprintln!(
                    "chibipop: the scan overlay could not be created, continuing without it: {e:#}"
                );
                None
            }
        }
    } else {
        None
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

    // Default stays false - D5.
    if popup.capture_exclusion().needs_capture_guard()
        || overlay.as_ref().is_some_and(|ov| ov.capture_exclusion().needs_capture_guard())
    {
        capture_guard_active.store(true, Ordering::SeqCst);
    }

    let mut renderer =
        Renderer::new(popup.hwnd()).context("creating the D2D/DirectWrite renderer")?;
    let theme = theme_from_config(&cfg.popup);
    if cfg.debug.show_lookup_log {
        crate::ui::console::show();
    }
    let max_height_percent = i32::from(cfg.popup.max_height_percent);
    let max_width_percent = i32::from(cfg.popup.max_width_percent);
    let scroll_popup = cfg.popup.scroll_popup;

    let hooks = Hooks::install().context("installing the low-level input hooks")?;
    Hooks::set_mode(cfg.trigger.mode);

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
    // What is on screen now.
    let mut shown: Option<Shown> = None;
    // BACKLOG 7: no way in but this.
    let mut settings: Option<SettingsWindow> = match SettingsWindow::open(
        &form_with_library(&cfg, &dicts, &library),
        &settings::stale_order_entries(&cfg, &dicts),
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
                drain_capture_guard();
            });

            // SAFETY: `w.hwnd()` is live until the `SettingsWindow` is
            // dropped, and `msg` is this loop's own stack storage.
            let handled = unsafe { IsDialogMessageW(w.hwnd(), &msg) }.as_bool();
            service_settings_click(w);
            if handled {
                continue;
            }
        }

        if msg.message == WM_TIMER && msg.wParam.0 == timer_id {
            // Spec D7: the popup's own rect.
            let live = cursor_now();
            let armed = scroll_popup
                && shown
                    .as_ref()
                    .is_some_and(|s| s.popup.contains(live) && s.content_h > s.view_h);
            Hooks::set_scroll_armed(armed);

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
                        if let Err(e) = renderer.paint(&s.presentation, &theme, s.scroll) {
                            eprintln!("chibipop: repainting for scroll failed: {e:#}");
                        }
                    }
                }
            }

            if let Some(w) = &settings {
                if rebuild.is_some() {
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
                        Some(SettingsOutcome::Cancel) => settings = None,
                        // Already on the main thread.
                        Some(SettingsOutcome::Quit) => unsafe { PostQuitMessage(0) },
                        Some(SettingsOutcome::Apply) => {
                            let edited = w.read(&form_with_library(&cfg, &dicts, &library));
                            let updated = settings::apply_to(&edited, &cfg);
                            // Never half-apply.
                            if edited.has_staged() {
                                match start_rebuild(&edited, &library, &staged_db) {
                                    Err(e) => refuse_apply(w, &e),
                                    Ok(flight) => {
                                        begin_rebuild(w);
                                        pending_cfg = Some(updated);
                                        rebuild = Some(flight);
                                    }
                                }
                            } else if let Err(e) = updated.save(config_path) {
                                eprintln!("chibipop: could not save settings to {}: {e:#}",
                                          config_path.display());
                            } else if let Err(e) = restart_self() {
                                eprintln!("chibipop: settings saved, but the restart failed: {e:#}");
                                eprintln!("chibipop: they will apply next time you start chibipop.");
                            } else {
                                unsafe { PostQuitMessage(0) };
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
                if shown.is_some() {
                    let _ = popup.hide();
                    if let Some(ov) = overlay.as_ref() {
                        ov.hide();
                    }
                    shown = None;
                }
            }

            if let Some(cursor) = Hooks::take_pending() {
                // Spec D3: hold, do not resolve.
                let frozen = shown
                    .as_ref()
                    .is_some_and(|s| in_sticky(cursor, s.hold, s.popup));
                if !frozen {
                    next_id += 1;
                    latest_dispatched = RequestId(next_id);
                    let _ = trigger_tx.send(Trigger { cursor, id: latest_dispatched });
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
                } else {
                    handle_worker_outcome(
                        &popup,
                        &mut renderer,
                        &theme,
                        max_height_percent,
                        max_width_percent,
                        overlay.as_ref(),
                        &mut shown,
                        result.outcome,
                        cfg.debug.show_lookup_log,
                    );
                }
            }
        } else if msg.message == WM_APP_CAPTURE_GUARD {
            // Drain, never one per wakeup.
            drain_capture_guard();
        } else if let Some(cmd) = tray.handle_message(msg.message, msg.lParam, || {
            // The menu swallows WM_TIMER.
            Hooks::set_scroll_armed(false);
            drain_capture_guard();
        }) {
            match cmd {
                TrayCommand::OpenSettings => {
                    if let Some(w) = &settings {
                        w.focus();
                    } else {
                        let form = form_with_library(&cfg, &dicts, &library);
                        let stale = settings::stale_order_entries(&cfg, &dicts);
                        match SettingsWindow::open(&form, &stale) {
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
    running.store(false, Ordering::SeqCst); // 1. clear the run flag
    drop(trigger_tx); // 2. drop the sender - worker's recv() unblocks with Err and exits

    // I5: unhook before draining.
    drop(hooks); // 3. drop Hooks - unhooks both WH_MOUSE_LL and WH_KEYBOARD_LL

    // A bare join() would deadlock.
    while !worker_handle.is_finished() {
        while let Ok(req) = capture_guard_rx.try_recv() {
            match req {
                CaptureGuardMsg::Hide { ack } => {
                    let _ = popup.hide();
                    // Mirrors the main drain above.
                    if let Some(ov) = &overlay {
                        ov.hide();
                    }
                    let _ = ack.send(());
                }
                CaptureGuardMsg::Restore => {}
            }
        }
        thread::sleep(Duration::from_millis(2));
    }
    let _ = worker_handle.join(); // now instant: the worker has already finished.

    drop(tray); // 4. drop Tray - removes the notification-area icon and its owner window

    // 5. the database is closed.
    if let Some(staged) = promote {
        match rebuild::promote(&staged, &db_path) {
            Err(e) => {
                eprintln!("chibipop: the rebuilt dictionary could not be put in place: {e:#}");
                eprintln!("chibipop: the old one is still there, and the new one is at {}.",
                          staged.display());
                if let Some(flight) = &applied {
                    undo_apply(flight, &e);
                }
            }
            Ok(()) => {
                if let Some(flight) = &applied {
                    if let Err(e) = flight.pending.commit() {
                        eprintln!("chibipop: clearing the library's .removed folder failed: {e:#}");
                    }
                }
                println!("chibipop: rebuilt {}.", db_path.display());
            }
        }
    }
    if restart_at_exit {
        if let Err(e) = restart_self() {
            eprintln!("chibipop: the restart failed: {e:#}");
            eprintln!("chibipop: your settings are saved; start chibipop again.");
        }
    }

    Ok(())
}

/// Serves triggers, owns OCR.
#[allow(clippy::too_many_arguments)]
fn worker_main(
    dict_path: PathBuf,
    rules_path: PathBuf,
    present_cfg: PresentConfig,
    max_ocr_passes: u8,
    scan_display: ScanDisplay,
    main_tid: u32,
    trigger_rx: mpsc::Receiver<Trigger>,
    result_tx: mpsc::Sender<WorkerResult>,
    running: Arc<AtomicBool>,
    startup_tx: mpsc::Sender<Result<Vec<DictInfo>>>,
    capture_guard_active: Arc<AtomicBool>,
    capture_guard_tx: mpsc::Sender<CaptureGuardMsg>,
) {
    let ocr = match OcrTextSource::new(max_ocr_passes).context("creating the OCR text source") {
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

    loop {
        let mut trigger = match trigger_rx.recv() {
            Ok(t) => t,
            Err(_) => break, // sender dropped: shutdown (decision 5, step 2).
        };
        // The newest queued wins.
        while let Ok(newer) = trigger_rx.try_recv() {
            trigger = newer;
        }
        if !running.load(Ordering::SeqCst) {
            break;
        }

        // Fresh, so no ordering rule.
        let guard = if capture_guard_active.load(Ordering::SeqCst) {
            Some(&capture_guard)
        } else {
            None
        };

        // One bad frame is not fatal.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            resolve_trigger(
                &ocr,
                &dict,
                &engine,
                &dicts,
                &present_cfg,
                trigger.cursor,
                guard,
                scan_display,
            )
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
        presentation,
        anchor: resolved.span.anchor,
        orientation: resolved.orientation,
        matched,
        scan,
    }
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
}

/// Applies one outcome.
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
) {
    match outcome {
        WorkerOutcome::Hide => {
            let _ = popup.hide();
            if let Some(ov) = overlay {
                ov.hide();
            }
            *shown = None;
        }
        WorkerOutcome::Failed(msg) => {
            eprintln!("chibipop: hover lookup failed: {msg}");
            let _ = popup.hide();
            if let Some(ov) = overlay {
                ov.hide();
            }
            *shown = None;
        }
        WorkerOutcome::Ready { presentation, anchor, orientation, matched, scan } => {
            if shown.as_ref().is_some_and(|prev| same_content(prev, &presentation, anchor)) {
                return; // Already on screen, unchanged.
            }
            // Only changed popups.
            if log {
                if let Some(card) = &presentation.top {
                    let head = card.written.clone()
                        .or_else(|| card.reading.clone())
                        .unwrap_or_default();
                    println!("{head}  match={}", card.match_len);
                }
            }
            match show_presentation(
                popup,
                renderer,
                theme,
                max_height_percent,
                max_width_percent,
                &presentation,
                anchor,
                0, // A new word always starts at the top.
            ) {
                Err(e) => {
                    eprintln!("chibipop: showing the popup failed: {e:#}");
                    let _ = popup.hide();
                    if let Some(ov) = overlay {
                        ov.hide();
                    }
                    *shown = None;
                    Hooks::set_scroll_armed(false);
                }
                Ok((rect, content_h, view_h)) => {
                    // Old notches must not land.
                    Hooks::discard_scroll();
                    *shown = Some(Shown {
                        anchor,
                        popup: rect,
                        hold: hold_region(anchor, matched, orientation),
                        presentation,
                        scroll: 0,
                        content_h,
                        view_h,
                    });
                    if let Some(ov) = overlay {
                        if let Err(e) = ov.show_rects(&scan, theme) {
                            eprintln!("chibipop: showing the scan overlay failed: {e:#}");
                        }
                    }
                }
            }
        }
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
) -> Result<(PhysRect, i32, i32)> {
    let monitor = monitor_rect_for(anchor);
    let max_w = ((monitor.w * max_width_percent) / 100).max(1);
    let max_h = ((monitor.h * max_height_percent) / 100).max(1);

    // view_h, not content_h, below.
    let (w, view_h, content_h) = renderer
        .measure(presentation, theme, max_w, max_h)
        .context("measuring popup content")?;

    let rect = place_popup(anchor, (w, view_h), monitor, POPUP_GAP);
    popup.show_at(rect).context("moving/showing the popup")?;
    renderer.paint(presentation, theme, scroll).context("painting the popup")?;
    Ok((rect, content_h, view_h))
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

/// Palette by name, font on top.
fn theme_from_config(popup: &crate::config::PopupConfig) -> Theme {
    let mut theme = match popup.theme.as_str() {
        "light" => Theme::light(),
        _ => Theme::dark(),
    };
    theme.font_name = popup.font.clone();
    theme
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
        Presentation {
            top: Some(Card {
                written: Some(written.to_string()),
                reading: None,
                pos: vec![],
                freq: None,
                blocks: vec![],
                match_len: 2,
            }),
            collapsed: vec![],
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
}
