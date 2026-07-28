//! Application wiring: the two-thread architecture from spec section 4.
//!
//! Everything else in this crate is a piece; this file is what makes hovering
//! Japanese text anywhere on screen show a popup. `run` owns the main thread
//! (message loop, hooks, the popup window) and spawns exactly one worker
//! thread (OCR resolve -> lookup -> present).
//!
//! **Thread ownership, and why it is split this way.** `SqliteDictionary`
//! wraps a `rusqlite::Connection` opened `SQLITE_OPEN_NO_MUTEX`: `Send` but
//! never `Sync` (Task 11's report, M1). `OcrTextSource` wraps a WinRT
//! `OcrEngine` created under `RoInitialize(RO_INIT_MULTITHREADED)`, and COM
//! apartment membership is established **per thread** - a thread that never
//! calls `RoInitialize` itself has no guaranteed standing to call methods on
//! a WinRT object, even one created by another thread in the same process.
//! Both constraints point the same way: `OcrTextSource`, `SqliteDictionary`
//! and `LookupEngine` are constructed **on the worker thread, inside its own
//! closure**, used only there, and never shared. `Popup` and `Renderer` are
//! the mirror case - HWND- and D2D-device-affine - so they are constructed
//! and used only on the main thread.
//!
//! **Where `SetProcessDpiAwarenessContext` actually happens.**
//! `OcrTextSource::new()` calls `text::capture::init_dpi_awareness()` as its
//! own first action (see `text/ocr.rs`). Since that constructor now runs on
//! the worker thread, and the main thread must not make any GDI/window call
//! before that has completed (contract 3), the main thread blocks on a
//! one-shot startup handshake (`startup_rx.recv()`) before touching
//! `ui::window::Popup` at all - see `run` below. This also means the call
//! happens exactly once: `SetProcessDpiAwarenessContext` is documented to
//! fail once a process's DPI awareness is already established, so calling it
//! a second time explicitly here (redundant with `OcrTextSource::new()`'s
//! own call) was deliberately avoided rather than risking that failure path
//! on every run.
//!
//! **The "single-slot channel" from spec section 4.** Implemented as a
//! `std::sync::mpsc::channel` where the worker drains to the newest queued
//! `Trigger` before processing (see the receive loop in `worker_main`),
//! rather than a hand-rolled overwrite cell. Behaviourally equivalent - the
//! worker never *acts* on a superseded position - and it comes with a real
//! `Sender` to drop at shutdown, which is literally one of the four steps
//! decision 5 names.

use crate::config::Config;
use crate::geom::{in_sticky, place_popup, PhysPoint, PhysRect, ScanDisplay, ScanKind, ScanRect};
use crate::input::hooks::Hooks;
use crate::lookup::deconj::Deconjugator;
use crate::lookup::engine::LookupEngine;
use crate::lookup::model::Dictionary;
use crate::lookup::rules::load_rules;
use crate::lookup::sqlite::SqliteDictionary;
use crate::present::{self, DictInfo, Presentation, PresentConfig};
use crate::text::ocr::OcrTextSource;
use crate::ui::overlay::Overlay;
use crate::ui::render::{max_scroll, Renderer};
use crate::ui::theme::Theme;
use crate::ui::tray::{Tray, TrayCommand};
use crate::ui::window::{CaptureExclusion, Popup};
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
    DispatchMessageW, GetCursorPos, GetMessageW, IsWindowVisible, KillTimer, PostQuitMessage,
    PostThreadMessageW, SetTimer, ShowWindow, TranslateMessage, MSG, SW_HIDE, SW_SHOWNOACTIVATE,
    WM_APP, WM_TIMER,
};

/// `WM_APP` (32768) is the first value Windows guarantees free for private
/// application messages (winuser.h). The worker posts this after pushing a
/// result, so the main thread's message loop knows to check `result_rx`.
const WM_APP_RESULT: u32 = WM_APP + 1;

/// `WM_APP + 2` is already `ui::tray`'s `WM_TRAYICON`. The worker posts this
/// to wake the main thread's `GetMessageW` loop whenever it queues a
/// `CaptureGuardMsg` - see `CaptureGuard`.
const WM_APP_CAPTURE_GUARD: u32 = WM_APP + 3;

/// How long `CaptureGuard::hide_for_capture` waits for the main thread to
/// confirm the popup is hidden before giving up and capturing anyway.
/// Generous relative to the sub-millisecond cost of `ShowWindow` itself -
/// this only matters if the main thread is unusually busy (or, at shutdown,
/// briefly not pumping at all; `run`'s shutdown sequence pumps
/// `capture_guard_rx` specifically to avoid ever needing this backstop, so
/// it firing in practice would itself be worth investigating, not just
/// silently tolerating).
const ACK_TIMEOUT: Duration = Duration::from_millis(250);

/// How often the main thread checks `Hooks::take_pending()`. `take_pending`
/// is a single atomic swap - cheap even when nothing is pending - so polling
/// this often costs nothing noticeable; 50 Hz keeps dispatch latency low
/// without busy-waiting the thread.
const DISPATCH_TICK_MS: u32 = 20;

/// Gap between the hovered character and the popup (spec section 4.2).
const POPUP_GAP: i32 = 12;

/// How far an anchor may move and still count as the same hover.
///
/// Not slop. `text::capture::UPSCALE` is 2, so every OCR word box is
/// re-measured through `PhysRect::scaled_down`'s integer division from a
/// differently-framed capture — ±1px per edge. An exact comparison would fail
/// spuriously and re-render on every hover, defeating the check.
const ANCHOR_JITTER_PX: i32 = 4;

/// Pixels of content scrolled per wheel notch.
///
/// A feel constant, not a measured one — expect to tune it against a real
/// overflowing entry, the way `REGION_W`/`REGION_H` were tuned.
const SCROLL_STEP_PX: i32 = 48;

/// How many consecutive ticks the wheel may stay armed before saying so once.
///
/// 250 ticks at `DISPATCH_TICK_MS` is about five seconds — far longer than any
/// real read of one popup, and short enough to appear in a log the user still
/// has open. This exists because a stuck arm swallows the wheel for *every*
/// application, and without it the bug report is "chibipop broke my mouse"
/// with nothing to search for.
const ARM_WARN_TICKS: u32 = 250;

/// The popup's width has no config knob (`config.rs`'s `PopupConfig` only
/// exposes `max_height_percent`) - 420 matches the width Task 5 verified
/// content wraps sensibly at. Also clamped to the target monitor's own width
/// below, so a hypothetical narrow monitor cannot be asked for a popup wider
/// than itself.
const POPUP_MAX_WIDTH: i32 = 420;

/// A monotonically increasing id assigned to every dispatched trigger.
/// Staleness is resolved purely by comparing ids - never by a sentinel value
/// (decision 1) - so a slow worker's answer to an old position is discarded
/// once a newer one has been dispatched, rather than racing it onto screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct RequestId(u64);

/// What the main thread dispatches to the worker for one gated cursor
/// movement.
struct Trigger {
    cursor: PhysPoint,
    id: RequestId,
}

/// What the worker sends back. Carries the same id it was given so the main
/// thread can apply the staleness rule without the worker knowing anything
/// about dispatch order itself.
struct WorkerResult {
    id: RequestId,
    outcome: WorkerOutcome,
}

enum WorkerOutcome {
    /// Nothing to show: either `resolve_at_tiled_scanned` found no text
    /// under the cursor, or it did but the dictionary had zero hits for it
    /// (e.g. punctuation). Neither is an error - most hover positions are
    /// not Japanese text, and logging that would be pure noise (decision 4).
    Hide,
    /// Capture, OCR or lookup failed. Logged once by the main thread; the
    /// popup hides and the app keeps running (spec section 6 - a failed
    /// hover is never fatal).
    Failed(String),
    /// `scan` is always empty when the debug toggle is off - see
    /// `resolve_at_tiled_scanned`'s own doc comment (M3 task 4).
    Ready { presentation: Presentation, anchor: PhysRect, scan: Vec<ScanRect> },
}

/// A request from the worker thread to the main thread, guarding one
/// capture against the popup's own pixels landing in it (spec §5.1). Only
/// ever sent when `Popup::capture_exclusion().needs_capture_guard()` is
/// true - seeing `Excluded` from the OS means M3-D7 already keeps the
/// popup out of every capture, and this whole dance is unnecessary
/// overhead on the default, overwhelmingly common path.
///
/// The `Popup` these act on lives on the main thread (created in `run`,
/// HWND-affine); the worker thread only ever reaches it through this
/// channel plus a `PostThreadMessageW` wake-up, never directly - see
/// `CaptureGuard` and the module docs' thread-ownership section.
enum CaptureGuardMsg {
    /// Hide the popup now; reply on `ack` once done, so the worker can be
    /// certain the popup is actually off-screen before it captures. Carries
    /// no position or content - `Popup::hide()` needs none.
    Hide { ack: mpsc::Sender<()> },
    /// Undo the most recent `Hide` - re-show the popup, but only if it was
    /// actually visible immediately beforehand (`run` remembers that in
    /// `capture_guard_prev_visible`, since this message carries nothing to
    /// decide it by). Fire-and-forget: nothing downstream waits for this to
    /// complete, since the capture it was guarding has already finished by
    /// the time it is sent.
    Restore,
}

/// The worker thread's handle onto the capture guard - `resolve_trigger`
/// holds `Option<&CaptureGuard>`, `None` whenever exclusion is already
/// active, so the default path sends no message, posts no thread message,
/// and waits on nothing at all.
struct CaptureGuard {
    main_tid: u32,
    request_tx: mpsc::Sender<CaptureGuardMsg>,
}

impl CaptureGuard {
    /// Blocks until the main thread confirms the popup is hidden, or until
    /// `ACK_TIMEOUT` elapses. The timeout exists purely as a backstop against
    /// the unforeseen - the ordinary case is a round trip through a main
    /// thread that is almost always idle inside `GetMessageW` - and giving up
    /// means capturing anyway rather than hanging the worker (and, by
    /// extension, every future hover) forever.
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

    /// Undoes `hide_for_capture`. Fire-and-forget - see `CaptureGuardMsg::Restore`.
    fn restore_after_capture(&self) {
        let _ = self.request_tx.send(CaptureGuardMsg::Restore);
        self.wake_main_thread();
    }

    /// `PostThreadMessageW`, not `PostMessageW`: this targets the main
    /// thread's queue directly (hwnd = NULL in the posted message, exactly
    /// like `WM_APP_RESULT` below), not any particular window. The main
    /// loop's `GetMessageW(&mut msg, None, 0, 0)` picks up thread messages
    /// and window messages alike.
    fn wake_main_thread(&self) {
        unsafe {
            let _ = PostThreadMessageW(self.main_tid, WM_APP_CAPTURE_GUARD, WPARAM(0), LPARAM(0));
        }
    }
}

/// Runs the popup application until the user quits. Installs the hooks,
/// creates the popup window and the tray icon, and pumps messages on this
/// (the calling) thread; spawns one worker thread for OCR/lookup/present.
///
/// Also creates the scan overlay (`cfg.debug.show_scan_region`) alongside
/// the popup - `None` when the toggle is off, so the feature stays inert
/// (M3 task 4). It tracks the popup through both `handle_worker_outcome`'s
/// ordinary show/hide and `drain_capture_guard`'s capture-time hide/reshow.
///
/// `config_path` is where `cfg` was loaded from (`main.rs`'s
/// `default_config_path()` or `--config`) - needed here, not just at load
/// time, because a tray mode change must persist back to the same file or
/// the setting silently reverts on the next restart.
pub fn run(mut cfg: Config, dict_path: &Path, rules_path: &Path, config_path: &Path) -> Result<()> {
    let dict_path = dict_path.to_path_buf();
    let rules_path = rules_path.to_path_buf();

    let running = Arc::new(AtomicBool::new(true));
    let (trigger_tx, trigger_rx) = mpsc::channel::<Trigger>();
    let (result_tx, result_rx) = mpsc::channel::<WorkerResult>();
    let (startup_tx, startup_rx) = mpsc::channel::<Result<()>>();
    // Starts `false` regardless of `cfg.popup.exclude_from_capture`: this
    // thread cannot yet know whether the OS actually accepted
    // `WDA_EXCLUDEFROMCAPTURE` (or whether it was even asked to) until
    // `Popup::create` runs below, which - per contract 3 - cannot happen
    // until *after* the worker thread has completed its own DPI-awareness
    // setup. Read fresh by the worker on every trigger (never cached), so
    // there is no ordering requirement between that later `.store()` and
    // whichever trigger happens to be first - see `worker_main`.
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

    // Contract 3: SetProcessDpiAwarenessContext must run before any GDI
    // call. It happens inside OcrTextSource::new(), which worker_main
    // constructs as its very first action - see the module docs above. This
    // recv() blocks the main thread, which owns every later GDI/window call
    // (Popup::create, Renderer::new, ...), until that has genuinely
    // completed. The ordering is enforced by the handshake itself, not by
    // hoping the worker thread happens to win a race.
    startup_rx
        .recv()
        .context("worker thread ended before completing startup")??;

    let popup = Popup::create(cfg.popup.exclude_from_capture).context("creating the popup window")?;

    // Contract 2: anything other than clean exclusion must be loud - and the
    // two ways to land there (asked-and-refused vs. deliberately-off) must
    // stay distinguishable here, or a genuine OS-level failure gets read as
    // an intentional setting (or vice versa). Both still need the capture
    // guard below - `needs_capture_guard` does not distinguish them, only
    // this report does.
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
    let max_height_percent = i32::from(cfg.popup.max_height_percent);
    let scroll_popup = cfg.popup.scroll_popup;

    let hooks = Hooks::install().context("installing the low-level input hooks")?;
    Hooks::set_mode(cfg.trigger.mode);

    // Hard failure per spec section 6: the tray is the only way to change
    // mode or quit, so a running process without one cannot be controlled.
    // Uses popup.hwnd() only as Shell_NotifyIconW's message target - see
    // ui::tray's module docs for why the menu itself does not use popup as
    // its owner.
    let tray = Tray::create(popup.hwnd(), cfg.trigger.mode).context("creating the tray icon")?;

    // hwnd = None: a thread timer, not bound to any window. WM_TIMER is
    // delivered straight into this thread's message queue (msg.hwnd = NULL)
    // and picked up by the same GetMessageW loop below that also dispatches
    // the popup's own window messages.
    let timer_id = unsafe { SetTimer(None, 0, DISPATCH_TICK_MS, None) };
    if timer_id == 0 {
        anyhow::bail!("SetTimer failed to install the dispatch tick");
    }

    println!("chibipop: running - hover Japanese text anywhere on screen.");
    println!("chibipop: right-click the tray icon to change mode or quit.");

    let mut next_id: u64 = 0;
    let mut latest_dispatched = RequestId(0);
    // Whether the popup was visible immediately before the most recent
    // `CaptureGuardMsg::Hide` - the one piece of state that message cannot
    // carry itself (see its doc comment), and the thing that keeps
    // `Restore` from forcing the popup to appear when it was rightfully
    // hidden already (e.g. the cursor is over non-text and nothing is
    // showing).
    let mut capture_guard_prev_visible = false;
    // Overlay's own visibility.
    let mut overlay_prev_visible = false;
    // What is on screen, and therefore what holds the cursor - see `Shown`.
    let mut shown: Option<Shown> = None;
    // Consecutive ticks the wheel has been captured - see ARM_WARN_TICKS.
    let mut armed_ticks: u32 = 0;

    // I4: kept in one place.
    let mut drain_capture_guard = || {
        while let Ok(req) = capture_guard_rx.try_recv() {
            match req {
                CaptureGuardMsg::Hide { ack } => {
                    capture_guard_prev_visible = popup.is_visible();
                    let _ = popup.hide();
                    if let Some(hwnd) = overlay_hwnd {
                        // SAFETY: `hwnd` is `Overlay::hwnd()`'s own handle;
                        // the `Overlay` that owns it lives in `run`'s local
                        // `overlay` for this whole loop, so the window is
                        // still live here. Both calls only read/set
                        // visibility - no other precondition applies.
                        overlay_prev_visible = unsafe { IsWindowVisible(hwnd).as_bool() };
                        unsafe {
                            let _ = ShowWindow(hwnd, SW_HIDE);
                        }
                    }
                    let _ = ack.send(());
                }
                CaptureGuardMsg::Restore => {
                    if capture_guard_prev_visible {
                        let _ = popup.show_without_activating();
                    }
                    if let Some(hwnd) = overlay_hwnd {
                        if overlay_prev_visible {
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
        // hwnd = None: messages for any window this thread owns, PLUS
        // thread-targeted messages (WM_TIMER above, WM_APP_RESULT below,
        // WM_QUIT from the tray's Quit item below) whose own hwnd is NULL.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break; // 0 = WM_QUIT, -1 = error. Either way, stop pumping.
        }

        if msg.message == WM_TIMER && msg.wParam.0 == timer_id {
            // Spec D7: the popup's OWN rect, never the sticky region. The
            // sticky region includes the hovered word, and the cursor is on
            // the hovered word by construction whenever a popup appears - so
            // arming on it would swallow the wheel while the user was merely
            // reading, and scrolling the page would silently scroll the popup
            // instead.
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
                    // Wheel-up is positive, and moves content toward the top.
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

            if let Some(cursor) = Hooks::take_pending() {
                // Spec D3: on the word or its popup, change nothing.
                let frozen = shown
                    .as_ref()
                    .is_some_and(|s| in_sticky(cursor, s.anchor, s.popup));
                if !frozen {
                    next_id += 1;
                    latest_dispatched = RequestId(next_id);
                    let _ = trigger_tx.send(Trigger { cursor, id: latest_dispatched });
                }
            }
        } else if msg.message == WM_APP_RESULT {
            // Drain to the freshest result actually queued right now - a
            // burst of WM_APP_RESULT posts (unlikely but not impossible)
            // collapses to one apply, matching the same "only the latest
            // matters" spirit as the trigger side.
            let mut freshest: Option<WorkerResult> = None;
            while let Ok(r) = result_rx.try_recv() {
                freshest = Some(r);
            }
            if let Some(result) = freshest {
                if result.id < latest_dispatched {
                    // Stale: a newer trigger has been dispatched since this
                    // one was sent. Drop silently (decision 1) - a
                    // superseded answer is not an error.
                } else {
                    handle_worker_outcome(
                        &popup,
                        &mut renderer,
                        &theme,
                        max_height_percent,
                        overlay.as_ref(),
                        &mut shown,
                        result.outcome,
                    );
                }
            }
        } else if msg.message == WM_APP_CAPTURE_GUARD {
            // Drain everything queued right now rather than one-per-wakeup:
            // harmless even though the worker only ever has one request
            // outstanding at a time (it blocks on `Hide`'s ack before doing
            // anything else, and only ever processes one trigger at once -
            // see `worker_main`), and it means a burst can never pile up
            // behind a slow main thread the way it could if this only ever
            // handled a single message per wakeup.
            drain_capture_guard();
        } else if let Some(cmd) = tray.handle_message(msg.message, msg.lParam, || {
            // Spec D9's one exception. `TrackPopupMenuEx` runs its own message
            // pump and discards thread-targeted messages, so `WM_TIMER` stops
            // arriving while the menu is open - while the hook keeps being
            // called. That is the only way the arm can latch, and this is the
            // callback that exists to service what the menu would swallow.
            Hooks::set_scroll_armed(false);
            drain_capture_guard();
        }) {
            match cmd {
                TrayCommand::SetMode(mode) => {
                    // AppState, hooks, and disk all updated together here -
                    // skipping any one of the three is exactly how "the
                    // toggle looks broken after a restart" bugs happen.
                    cfg.trigger.mode = mode;
                    Hooks::set_mode(mode);
                    // No sticky region may survive a mode change.
                    shown = None;
                    Hooks::set_scroll_armed(false);
                    if let Err(e) = cfg.save(config_path) {
                        eprintln!("chibipop: failed to save the mode change to {}: {e:#}",
                                  config_path.display());
                    }
                }
                TrayCommand::Quit => unsafe {
                    // Literal PostQuitMessage, not PostThreadMessageW: unlike
                    // Task 7's interim console reader, this runs ON the main
                    // thread (inside this same GetMessageW loop), so posting
                    // to "whichever thread calls this" is already correct.
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

    // Shutdown (decision 5), in order. PostQuitMessage has already fired -
    // it is what ended the loop above - so the remaining steps happen here:
    unsafe {
        let _ = KillTimer(None, timer_id);
    }
    running.store(false, Ordering::SeqCst); // 1. clear the run flag
    drop(trigger_tx); // 2. drop the sender - worker's recv() unblocks with Err and exits

    // I5: unhook before draining.
    drop(hooks); // 3. drop Hooks - unhooks both WH_MOUSE_LL and WH_KEYBOARD_LL

    // The worker may right now be blocked inside `CaptureGuard::hide_for_capture`,
    // waiting for this thread to service a `Hide` - but this thread has
    // already left the `GetMessageW` loop above (that is what ended it), so
    // a bare `worker_handle.join()` here would deadlock: nothing would ever
    // answer that `Hide`, and nothing would ever unblock this join. Keep
    // draining `capture_guard_rx` - answering `Hide` so the worker can
    // finish its current capture, discarding `Restore` since a popup about
    // to be destroyed has nothing worth restoring to - until the worker
    // actually finishes. `is_finished` is a non-blocking poll, not a wait,
    // so this loop is what does the waiting; `hide_for_capture`'s own
    // `ACK_TIMEOUT` is a second, independent backstop, not relied on here.
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

    Ok(())
}

/// Constructs everything OCR/SQLite-related on this thread (see the module
/// docs for why), reports startup success/failure once via `startup_tx`,
/// then services triggers until the channel closes.
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
    startup_tx: mpsc::Sender<Result<()>>,
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
            "opening {} - build it with tools/build-dict/build.py",
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

    // Decision 2: called once, here, and reused for every trigger below -
    // never re-queried per lookup.
    let dicts: Vec<DictInfo> = match dict.dicts().context("reading dictionary identities") {
        Ok(d) => d,
        Err(e) => {
            let _ = startup_tx.send(Err(e));
            return;
        }
    };

    // Built once and reused for every trigger below, same as `engine`/`dicts`
    // above - cheap regardless (a `u32` and a channel `Sender`), but there is
    // also no reason to rebuild it per hover.
    let capture_guard = CaptureGuard { main_tid, request_tx: capture_guard_tx };

    if startup_tx.send(Ok(())).is_err() {
        return; // main thread gave up waiting; nothing left to do.
    }

    loop {
        let mut trigger = match trigger_rx.recv() {
            Ok(t) => t,
            Err(_) => break, // sender dropped: shutdown (decision 5, step 2).
        };
        // Drain to the newest queued trigger before doing any work - see the
        // module docs on why mpsc-plus-drain stands in for a literal
        // single-slot cell.
        while let Ok(newer) = trigger_rx.try_recv() {
            trigger = newer;
        }
        if !running.load(Ordering::SeqCst) {
            break;
        }

        // Read fresh every trigger rather than once at the top of this
        // function: cheap (one atomic load), and it removes any need to
        // reason about exactly when `run` finishes setting this relative to
        // when the first trigger could possibly arrive - see this flag's
        // own doc comment in `run`.
        let guard = if capture_guard_active.load(Ordering::SeqCst) {
            Some(&capture_guard)
        } else {
            None
        };

        // One bad frame must not end the session - the same discipline
        // main.rs's `watch` command already applies per-iteration, extended
        // here to a real panic (not just a returned Err) so a single
        // unexpected failure can't silently kill hovering for the rest of
        // the run. AssertUnwindSafe: none of ocr/dict/engine/dicts/guard
        // retain any externally-observable partial-mutation state across a
        // panic inside resolve_trigger - each is only ever used through
        // shared, read-style calls (resolve_at_tiled_scanned, run,
        // hide_for_capture / restore_after_capture, both of which only ever
        // send on an mpsc
        // `Sender` - never poisoned by a panicking receiver the way a
        // `Mutex` would be) - so there is nothing for a caught panic to
        // leave torn.
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

/// One hover's worth of work: `OcrTextSource::resolve_at_tiled_scanned` ->
/// lookup -> `present::build`, exactly the pipeline the brief specifies.
///
/// `capture_guard`, when `Some`, wraps the capture inside
/// `resolve_at_tiled_scanned` with a hide-before/reshow-after around it
/// (spec §5.1) - `text::capture` stays exactly as M2 left it (spec section
/// 4: "M3 adds no lookup logic"), so the guard wraps the call from here
/// rather than reaching inside it. This does mean the popup stays hidden
/// for the OCR recognition step too, not only the BitBlt itself - a longer
/// hidden window than the minimum that correctness requires, traded
/// deliberately for not touching M2 at all; see the task report.
///
/// `scan_display` decides what the overlay is handed: its `captures` half is
/// the old `cfg.debug.show_scan_region` and is the only thing the OCR layer
/// ever collects, while its `highlight` half is computed here from the top
/// card. That split is what keeps the default path to **one** rectangle
/// instead of four - the capture kinds are never collected rather than
/// collected and filtered (spec D2).
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
    if scan_display.highlight {
        if let Some(rect) = present::match_highlight(&resolved.span, presentation.top.as_ref()) {
            scan.push(ScanRect { rect, kind: ScanKind::Match });
        }
    }
    WorkerOutcome::Ready { presentation, anchor: resolved.span.anchor, scan }
}


/// What is on screen right now. `None` whenever the popup is hidden.
///
/// **Must never outlive the visible window.** While this is `Some`,
/// [`crate::geom::in_sticky`] suppresses trigger dispatch for its region; a
/// `Shown` left behind by a hidden popup would therefore turn a patch of the
/// screen into a dead zone where hovering silently does nothing. It is set
/// only after `show_presentation` has succeeded, and cleared on every path
/// that hides the popup.
struct Shown {
    /// The hovered character's own box.
    anchor: PhysRect,
    /// Where `place_popup` put the window — stored, never re-derived.
    popup: PhysRect,
    presentation: Presentation,
    /// Vertical content offset in physical pixels; 0 is the top.
    scroll: i32,
    /// Natural content height, unclamped — what `scroll` ranges against.
    content_h: i32,
    /// Visible height, which is the window's height.
    view_h: i32,
}

/// Applies one `WorkerOutcome` to the popup and - when the debug toggle
/// created one - the scan overlay, which moves in lock-step with the popup
/// on every arm below (M3 task 4).
///
/// Maintains `shown` in step with what is actually visible - see [`Shown`] on
/// why a stale one is the defect to avoid.
#[allow(clippy::too_many_arguments)]
fn handle_worker_outcome(
    popup: &Popup,
    renderer: &mut Renderer,
    theme: &Theme,
    max_height_percent: i32,
    overlay: Option<&Overlay>,
    shown: &mut Option<Shown>,
    outcome: WorkerOutcome,
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
        WorkerOutcome::Ready { presentation, anchor, scan } => {
            if shown.as_ref().is_some_and(|prev| same_content(prev, &presentation, anchor)) {
                return; // Already on screen, unchanged.
            }
            match show_presentation(
                popup,
                renderer,
                theme,
                max_height_percent,
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
                    // Notches meant for the previous popup must not land here.
                    Hooks::discard_scroll();
                    *shown = Some(Shown {
                        anchor,
                        popup: rect,
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

/// `measure`, `place_popup` against the monitor containing the anchor,
/// `show_at`, `paint` - the exact sequence the brief specifies.
fn show_presentation(
    popup: &Popup,
    renderer: &mut Renderer,
    theme: &Theme,
    max_height_percent: i32,
    presentation: &Presentation,
    anchor: PhysRect,
    scroll: i32,
) -> Result<(PhysRect, i32, i32)> {
    let monitor = monitor_rect_for(anchor);
    let max_w = POPUP_MAX_WIDTH.min(monitor.w.max(1));
    let max_h = ((monitor.h * max_height_percent) / 100).max(1);

    // `view_h` is ALWAYS <= max_h by construction (render.rs's own
    // guarantee), and that capped value - not the natural `content_h`, and
    // not a recomputed size - is what place_popup receives next: geom.rs's
    // anchor-never-covered proof (Task 3's 12,201-case sweep) holds only
    // when the height it is handed never exceeds the 45%-of-monitor cap.
    // Passing `content_h` here would silently break that guarantee for
    // exactly the long entries scrolling exists to serve.
    let (w, view_h, content_h) = renderer
        .measure(presentation, theme, max_w, max_h)
        .context("measuring popup content")?;

    let rect = place_popup(anchor, (w, view_h), monitor, POPUP_GAP);
    popup.show_at(rect).context("moving/showing the popup")?;
    renderer.paint(presentation, theme, scroll).context("painting the popup")?;
    Ok((rect, content_h, view_h))
}

/// Whether a new outcome would draw exactly what is already on screen.
///
/// Keyed on **content**, because the question is literally "would the rendered
/// output be identical". That also covers the case `main.rs`'s `watch`
/// documents: the recognised line gains or loses characters at either end as
/// the capture region slides, changing `cursor_byte_offset` while the hovered
/// glyph — and therefore the hits — stay the same.
///
/// The anchor is part of the key regardless, because two occurrences of one
/// word on screen produce equal presentations and the popup must still move.
fn same_content(prev: &Shown, new: &Presentation, anchor: PhysRect) -> bool {
    prev.presentation == *new
        && (prev.anchor.x - anchor.x).abs() <= ANCHOR_JITTER_PX
        && (prev.anchor.y - anchor.y).abs() <= ANCHOR_JITTER_PX
}

/// The pointer's position right now.
///
/// Read live rather than from `Hooks::take_pending`, which is gated at
/// `MOVEMENT_GATE_PX`: easing into the popup in small steps could otherwise
/// leave the wheel arm stale. From the live position the arm is correct within
/// one tick regardless of movement history, and it self-corrects from any wrong
/// state instead of latching.
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

/// The monitor containing `anchor`'s centre - never the primary monitor
/// unconditionally (decision 3). This machine has two monitors of different
/// orientations; hardcoding one would look correct only by accident.
fn monitor_rect_for(anchor: PhysRect) -> PhysRect {
    let c = anchor.center();
    let pt = POINT { x: c.x, y: c.y };
    unsafe {
        // MONITOR_DEFAULTTONEAREST never returns a null HMONITOR, even for
        // an out-of-bounds point, so hmon itself needs no failure handling.
        let hmon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(hmon, &mut mi).as_bool() {
            let rc = mi.rcMonitor;
            PhysRect { x: rc.left, y: rc.top, w: rc.right - rc.left, h: rc.bottom - rc.top }
        } else {
            eprintln!("chibipop: GetMonitorInfoW failed; placing against a 1920x1080 fallback");
            PhysRect { x: 0, y: 0, w: 1920, h: 1080 }
        }
    }
}

/// Builds the runtime `Theme` from `[popup]`: the palette from `popup.theme`,
/// then `popup.font` overlaid - the one place config crosses into theme.rs's
/// deliberately config-free struct.
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
            max_height_percent: 45,
            summary_chars: 40,
            font: font.to_string(),
            highlight_match: true,
            scroll_popup: true,
        }
    }

    /// I1: `[popup] font` must reach the built `Theme`, not just round-trip
    /// through `config.rs` unused.
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
            presentation: presentation_of(written),
            scroll: 0,
            content_h: 300,
            view_h: 300,
        }
    }

    /// UPSCALE is 2, so every anchor is re-measured through integer division
    /// from a differently-framed capture: ±1px per edge. An exact-match test
    /// would fail spuriously and re-render anyway.
    #[test]
    fn an_equal_card_with_a_jittered_anchor_is_the_same_content() {
        let a = PhysRect { x: 100, y: 200, w: 26, h: 27 };
        let prev = shown_of("宿舎", a);
        let jittered = PhysRect { x: 101, y: 199, w: 26, h: 27 };
        assert!(same_content(&prev, &presentation_of("宿舎"), jittered));
    }

    /// Two occurrences of one word on screen produce identical presentations,
    /// and the popup genuinely has to move.
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

    /// Exactly at the tolerance must still count as unchanged; one past it
    /// must not.
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
