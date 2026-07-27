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
use crate::geom::{place_popup, PhysPoint, PhysRect};
use crate::input::hooks::Hooks;
use crate::lookup::deconj::Deconjugator;
use crate::lookup::engine::LookupEngine;
use crate::lookup::model::Dictionary;
use crate::lookup::rules::load_rules;
use crate::lookup::sqlite::SqliteDictionary;
use crate::present::{self, DictInfo, Presentation, PresentConfig};
use crate::text::ocr::OcrTextSource;
use crate::ui::render::Renderer;
use crate::ui::theme::Theme;
use crate::ui::window::Popup;
use anyhow::{Context, Result};
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
    DispatchMessageW, GetMessageW, KillTimer, PostThreadMessageW, SetTimer, TranslateMessage,
    MSG, WM_APP, WM_QUIT, WM_TIMER,
};

/// `WM_APP` (32768) is the first value Windows guarantees free for private
/// application messages (winuser.h). The worker posts this after pushing a
/// result, so the main thread's message loop knows to check `result_rx`.
const WM_APP_RESULT: u32 = WM_APP + 1;

/// How often the main thread checks `Hooks::take_pending()`. `take_pending`
/// is a single atomic swap - cheap even when nothing is pending - so polling
/// this often costs nothing noticeable; 50 Hz keeps dispatch latency low
/// without busy-waiting the thread.
const DISPATCH_TICK_MS: u32 = 20;

/// Gap between the hovered character and the popup (spec section 4.2).
const POPUP_GAP: i32 = 12;

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
    /// Nothing to show: either `resolve_at` found no text under the cursor,
    /// or it did but the dictionary had zero hits for it (e.g. punctuation).
    /// Neither is an error - most hover positions are not Japanese text, and
    /// logging that would be pure noise (decision 4).
    Hide,
    /// Capture, OCR or lookup failed. Logged once by the main thread; the
    /// popup hides and the app keeps running (spec section 6 - a failed
    /// hover is never fatal).
    Failed(String),
    Ready { presentation: Presentation, anchor: PhysRect },
}

/// Runs the popup application until the user quits. Installs the hooks,
/// creates the popup window, and pumps messages on this (the calling)
/// thread; spawns one worker thread for OCR/lookup/present.
pub fn run(cfg: Config, dict_path: &Path, rules_path: &Path) -> Result<()> {
    let dict_path = dict_path.to_path_buf();
    let rules_path = rules_path.to_path_buf();

    let running = Arc::new(AtomicBool::new(true));
    let (trigger_tx, trigger_rx) = mpsc::channel::<Trigger>();
    let (result_tx, result_rx) = mpsc::channel::<WorkerResult>();
    let (startup_tx, startup_rx) = mpsc::channel::<Result<()>>();

    // Safety: FFI call with no preconditions - always succeeds, returns the
    // id of whichever thread calls it.
    let main_tid = unsafe { GetCurrentThreadId() };
    let present_cfg = cfg.present_config();
    let worker_running = Arc::clone(&running);

    let worker_handle = thread::spawn(move || {
        worker_main(
            dict_path,
            rules_path,
            present_cfg,
            main_tid,
            trigger_rx,
            result_tx,
            worker_running,
            startup_tx,
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

    let popup = Popup::create().context("creating the popup window")?;

    // Contract 2: a false here must be loud. Silence is how the popup starts
    // photographing itself on every hover and feeding its own rendered text
    // back into the next OCR lookup.
    if popup.capture_excluded() {
        println!(
            "chibipop: capture exclusion active - the popup will not appear in its own OCR captures"
        );
    } else {
        eprintln!("chibipop: ============================================================");
        eprintln!("chibipop: WARNING: capture exclusion is NOT active for the popup window.");
        eprintln!("chibipop: SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE) was not accepted.");
        eprintln!("chibipop: Every hover will now photograph the popup itself and feed its own");
        eprintln!("chibipop: rendered text back into the next OCR lookup. This is a real defect");
        eprintln!("chibipop: - do not trust lookup results until it is fixed.");
        eprintln!("chibipop: ============================================================");
    }

    let mut renderer =
        Renderer::new(popup.hwnd()).context("creating the D2D/DirectWrite renderer")?;
    let theme = theme_from_config(&cfg.popup.theme);
    let max_height_percent = i32::from(cfg.popup.max_height_percent);

    let hooks = Hooks::install().context("installing the low-level input hooks")?;
    Hooks::set_mode(cfg.trigger.mode);

    // hwnd = None: a thread timer, not bound to any window. WM_TIMER is
    // delivered straight into this thread's message queue (msg.hwnd = NULL)
    // and picked up by the same GetMessageW loop below that also dispatches
    // the popup's own window messages.
    let timer_id = unsafe { SetTimer(None, 0, DISPATCH_TICK_MS, None) };
    if timer_id == 0 {
        anyhow::bail!("SetTimer failed to install the dispatch tick");
    }

    // Task 8 adds the tray's Quit menu item; until then, this is the only
    // user-facing way to stop `run`. PostThreadMessageW (not PostQuitMessage)
    // is required here specifically because this reader runs on its own
    // thread: PostQuitMessage always posts WM_QUIT to whichever thread calls
    // it, which would be this reader thread - one nobody ever pumps messages
    // for - not the main thread running the loop below.
    spawn_quit_reader(main_tid);
    println!("chibipop: running - hover Japanese text anywhere on screen.");
    println!("chibipop: type 'q' and press Enter in this console to quit.");

    let mut next_id: u64 = 0;
    let mut latest_dispatched = RequestId(0);
    let mut msg = MSG::default();

    loop {
        // hwnd = None: messages for any window this thread owns, PLUS
        // thread-targeted messages (WM_TIMER above, WM_APP_RESULT below,
        // WM_QUIT from the quit reader) whose own hwnd is NULL.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break; // 0 = WM_QUIT, -1 = error. Either way, stop pumping.
        }

        if msg.message == WM_TIMER && msg.wParam.0 == timer_id {
            if let Some(cursor) = Hooks::take_pending() {
                next_id += 1;
                latest_dispatched = RequestId(next_id);
                let _ = trigger_tx.send(Trigger { cursor, id: latest_dispatched });
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
                        result.outcome,
                    );
                }
            }
        } else {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    // Shutdown (decision 5), in order. PostQuitMessage-equivalent has
    // already fired - it is what ended the loop above - so the remaining
    // three steps happen here:
    unsafe {
        let _ = KillTimer(None, timer_id);
    }
    running.store(false, Ordering::SeqCst); // 1. clear the run flag
    drop(trigger_tx); // 2. drop the sender - worker's recv() unblocks with Err and exits
    let _ = worker_handle.join(); // wait for the worker to actually finish before tearing down further
    drop(hooks); // 3. drop Hooks - unhooks both WH_MOUSE_LL and WH_KEYBOARD_LL

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
    main_tid: u32,
    trigger_rx: mpsc::Receiver<Trigger>,
    result_tx: mpsc::Sender<WorkerResult>,
    running: Arc<AtomicBool>,
    startup_tx: mpsc::Sender<Result<()>>,
) {
    let ocr = match OcrTextSource::new().context("creating the OCR text source") {
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

        // One bad frame must not end the session - the same discipline
        // main.rs's `watch` command already applies per-iteration, extended
        // here to a real panic (not just a returned Err) so a single
        // unexpected failure can't silently kill hovering for the rest of
        // the run. AssertUnwindSafe: none of ocr/dict/engine/dicts retain
        // any externally-observable partial-mutation state across a panic
        // inside resolve_trigger - each is only ever used through shared,
        // read-style calls (resolve_at, run) - so there is nothing for a
        // caught panic to leave torn.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            resolve_trigger(&ocr, &dict, &engine, &dicts, &present_cfg, trigger.cursor)
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

/// One hover's worth of work: `OcrTextSource::resolve_at` -> lookup ->
/// `present::build`, exactly the pipeline the brief specifies.
fn resolve_trigger(
    ocr: &OcrTextSource,
    dict: &SqliteDictionary,
    engine: &LookupEngine,
    dicts: &[DictInfo],
    present_cfg: &PresentConfig,
    cursor: PhysPoint,
) -> WorkerOutcome {
    let resolved = match ocr.resolve_at(cursor) {
        Ok(Some(r)) => r,
        Ok(None) => return WorkerOutcome::Hide,
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
    WorkerOutcome::Ready { presentation, anchor: resolved.span.anchor }
}

fn handle_worker_outcome(
    popup: &Popup,
    renderer: &mut Renderer,
    theme: &Theme,
    max_height_percent: i32,
    outcome: WorkerOutcome,
) {
    match outcome {
        WorkerOutcome::Hide => {
            let _ = popup.hide();
        }
        WorkerOutcome::Failed(msg) => {
            eprintln!("chibipop: hover lookup failed: {msg}");
            let _ = popup.hide();
        }
        WorkerOutcome::Ready { presentation, anchor } => {
            if let Err(e) =
                show_presentation(popup, renderer, theme, max_height_percent, &presentation, anchor)
            {
                eprintln!("chibipop: showing the popup failed: {e:#}");
                let _ = popup.hide();
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
) -> Result<()> {
    let monitor = monitor_rect_for(anchor);
    let max_w = POPUP_MAX_WIDTH.min(monitor.w.max(1));
    let max_h = ((monitor.h * max_height_percent) / 100).max(1);

    // measure's third element reports whether it clamped to max_h; the
    // returned (w, h) is ALWAYS <= (max_w, max_h) by construction
    // (render.rs's own guarantee). That exact clamped pair - not a
    // recomputed or unclamped size - is what place_popup receives next:
    // geom.rs's anchor-never-covered proof (Task 3's 12,201-case sweep)
    // holds only when the height it is handed never exceeds the
    // 45%-of-monitor cap. Passing an unclamped height here would silently
    // break that guarantee for exactly the long entries where truncation
    // kicks in.
    let (w, h, _clamped) = renderer
        .measure(presentation, theme, max_w, max_h)
        .context("measuring popup content")?;

    let rect = place_popup(anchor, (w, h), monitor, POPUP_GAP);
    popup.show_at(rect).context("moving/showing the popup")?;
    renderer.paint(presentation, theme).context("painting the popup")?;
    Ok(())
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

fn theme_from_config(theme_name: &str) -> Theme {
    match theme_name {
        "light" => Theme::light(),
        _ => Theme::dark(),
    }
}

/// The interim way to stop `run` before Task 8's tray exists: typing 'q' +
/// Enter in the console. See the call site in `run` for why
/// `PostThreadMessageW` is required here rather than `PostQuitMessage`.
fn spawn_quit_reader(main_tid: u32) {
    thread::spawn(move || {
        let mut line = String::new();
        loop {
            line.clear();
            match std::io::stdin().read_line(&mut line) {
                Ok(0) => break, // stdin closed / EOF
                Ok(_) => {
                    if line.trim().eq_ignore_ascii_case("q") {
                        unsafe {
                            let _ = PostThreadMessageW(main_tid, WM_QUIT, WPARAM(0), LPARAM(0));
                        }
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
}
