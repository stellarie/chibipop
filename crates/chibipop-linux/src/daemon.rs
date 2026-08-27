//! The daemon: calloop pump + instance lock + control socket + logging
//! (ADR-0001: all sync, calloop as the Linux pump), the popup's layer
//! surfaces (ADR-0004), and the capture channel's startup half — the
//! ADR-0002 backend ladder and, when it picks the portal rung, the
//! eager consent that has to finish before anything reports a channel
//! state. OCR is the one channel still to plug into this loop.

use crate::capture;
use crate::capture::backend::{self as capture_backend, Backend};
use crate::capture::portal::{self, PortalCapture, PortalSession};
use crate::capture::software_cursor;
use crate::clipboard;
use crate::control::{ControlSocket, StubState, Verb};
use crate::cursor::{self, budget, hyprctl, image_copy};
use crate::cursor::image_copy::{CursorHandler, CursorState};
use crate::settings::child::{self, SettingsChild, SpawnOutcome};
use crate::lock::{self, LockError};
use crate::logging::Log;
use crate::paths::Paths;
use crate::tray::status::{ChannelId, ChannelState, ChannelStatuses};
use crate::tray::{self, TrayHandle, TrayRequest};
use crate::overlay::{self, Outline};
use crate::popup::{self, Demo, Popup, ShowRequest};
use crate::select::{self, Pick, Selector};
use crate::shortcuts;
use crate::trigger::{self, Hold};
use crate::wayland;
use crate::worker;
use anyhow::{bail, Context, Result};
use calloop::generic::Generic;
use calloop::signals::{Signal, Signals};
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction, RegistrationToken};
use calloop_wayland_source::WaylandSource;
use chibipop::controller::{Command, Controller, ControllerConfig, Event, RequestId};
use chibipop::geom::{PhysPoint, PhysRect, ScanKind, ScanRect};
use chibipop::present::DictInfo;
use chibipop::text::layout::OcrLine;
use chibipop::text::mask::{CaptureMask, CaptureMode};
use chibipop::text::Frame;
use chibipop::worker::{Hover, Trigger, TriggerKind, Worker};
use std::collections::{HashMap, HashSet};
use std::os::fd::{AsFd, BorrowedFd};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use smithay_client_toolkit::seat::pointer::PointerEvent;
use smithay_client_toolkit::shell::wlr_layer::LayerSurface;
use wayland_client::delegate_dispatch;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_registry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::globals::registry_queue_init;
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::ext::image_capture_source::v1::client::ext_image_capture_source_v1::ExtImageCaptureSourceV1;
use wayland_protocols::ext::image_capture_source::v1::client::ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_cursor_session_v1::ExtImageCopyCaptureCursorSessionV1;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::ZxdgOutputV1;

/// The controller's tick length. No dispatch timer exists on Linux
/// (ADR-0010: event-paced, Worker-throttled); this only scales the
/// controller's tick-derived warning arithmetic.
const DISPATCH_TICK_MS: u32 = 20;

/// The surface probe's env hook and its pick deadline. See
/// [`App::probe_surfaces`].
const SURFACE_PROBE_ENV: &str = "CHIBIPOP_SURFACE_PROBE";
const SURFACE_PROBE_DEADLINE: Duration = Duration::from_millis(400);

/// A demo anchor's box when no cursor sample has arrived yet: the
/// canned popup still has to land somewhere.
const DEMO_ANCHOR: PhysRect = PhysRect { x: 200, y: 200, w: 120, h: 32 };

/// The pump's shared state.
///
/// `pub(crate)` because the popup's Wayland dispatch impls are written
/// against it (`popup::surface`): SCTK 0.21 delegates through a blanket
/// impl this state cannot use, so the forwarding impls live beside the
/// code they serve and reach back through the accessors below.
pub(crate) struct App {
    log: Log,
    stub: StubState,
    /// Every directory this daemon reads or writes, resolved once at
    /// startup: the published trigger state (ticket 36), the config
    /// file a reload re-reads, and the screenshots folder a mined
    /// picture lands in (`Paths::screenshots_dir`, ticket 02).
    paths: Paths,
    /// The config as loaded, so a reload can rebuild what the Worker
    /// owns from the same source of truth the Controller reads.
    config: chibipop::config::Config,
    signal: LoopSignal,
    /// The cursor channel's Wayland side (ticket 33).
    cursor: CursorState,
    /// Driven by cursor Events and the trigger verbs; its Commands are
    /// executed below.
    controller: Controller,
    /// CHIBIPOP_CURSOR_TRACE=1: log every sample and poll interval.
    trace: bool,
    /// Last hyprctl sample (logical), for move detection.
    last_poll: Option<(i32, i32)>,
    /// The newest cursor sample, global physical: where a press looks.
    last_cursor: Option<PhysPoint>,
    /// Which cursor rung serves this session, so a press knows whether
    /// it can ask for a fresh sample or must use the newest event.
    cursor_rung: Option<cursor::Rung>,
    /// When the hyprctl rung last saw the cursor move.
    last_move: Instant,
    /// At most one settings child (ADR-0005), spawned from the tray's
    /// Settings item; the settings-scoped flock is the cross-process
    /// guard, this is the daemon's own.
    settings: SettingsChild,
    /// Channel health plus the SNI tray mirroring it (ADR-0006). Also
    /// the daemon's own view: it works unchanged when there is no tray.
    tray: TrayHandle,
    /// The popup's layer surfaces (ADR-0004). `None` only where there
    /// is no compositor to bind against: a unit test, or a session
    /// missing the layer shell — the daemon stays up either way.
    popup: Option<Popup>,
    /// The region selector's layer surfaces (spec D5). `None` on a
    /// compositor with no layer shell, and for the same reason the
    /// popup's shell is: absence is a state, not an error.
    selector: Option<Selector>,
    /// One pick in flight, while [`Selector::pick`]'s nested pump has
    /// the thread. It lives here rather than inside the `Selector`
    /// because the SCTK handlers that drive the drag are written on
    /// `App` and the pump dispatches `&mut App` into them.
    pick: Option<Pick>,
    /// The scan overlay's layer surfaces: click-through, frame-only
    /// rects outlining what this hover captured and the word it
    /// defined (`Command::ShowScanOverlay`). `None` for the same
    /// reason as `selector`. The static region's border is a second
    /// `Outline` beside this one, because the two have independent
    /// lifetimes - a hover repainting its boxes must not rub out a
    /// static region, and the two Windows windows this replaces
    /// (`ui/overlay.rs`, `ui/static_overlay.rs`) are separate for the
    /// same reason.
    scan_outline: Option<Outline>,
    /// The static sentence region's border, the other half of that pair
    /// (`ui/static_overlay.rs`'s counterpart). Shown and hidden by one
    /// predicate, [`static_overlay_region`], so no call site can
    /// disagree about when a border belongs on screen.
    static_outline: Option<Outline>,
    /// `CHIBIPOP_POPUP_DEMO=1`: the trigger verbs show and hide the
    /// canned popup instead of looking anything up, so the surface can
    /// be driven without a dictionary.
    demo: Demo,
    /// A scripted pointer pass is running (`CHIBIPOP_POINTER_SCRIPT`),
    /// so the repaints its own steps cause do not start another.
    scripting: bool,
    /// The core pipeline: capture + OCR + dictionary on their own
    /// thread (ADR-0001). `None` when it could not be built - no
    /// capture protocol, no OCR models, a refused portal - and the
    /// daemon stays up saying so.
    worker: Option<Worker>,
    /// What a spawn needs, kept so a granted portal retry can hand the
    /// new session to a fresh pipeline.
    worker_setup: worker::Setup,
    /// The wake the worker thread pings when a result is queued.
    worker_ping: calloop::ping::Ping,
    /// Where an AnkiConnect call's answer comes back. The calls are
    /// blocking HTTP on their own threads, so the pump hears about
    /// them the way it hears about the Worker: as an event (ADR-0001).
    anki_tx: calloop::channel::Sender<AnkiOutcome>,
    /// Where a mined region's pixels come back. Same shape and same
    /// reason as `anki_tx`: opening a capture backend and grabbing a
    /// frame blocks, so it happens on a thread and arrives as an
    /// event (spec D6).
    shot_tx: calloop::channel::Sender<Result<Frame, String>>,
    /// The one screenshot in flight, if any (see [`Shot`]).
    shot: Option<Shot>,
    /// The one-off OCR queue into the Worker's thread-affine engine,
    /// plus the wake that makes it arrive. Rebuilt on every respawn,
    /// because the nudge belongs to a particular Worker; while there is
    /// no pipeline it is [`worker::OcrJobs::disconnected`], so a copy
    /// fails loudly instead of parking forever.
    ocr_jobs: worker::OcrJobs,
    /// Where a one-off OCR job's lines come back. Cloned into each
    /// request, so the answer is an event on this pump and the Worker
    /// thread never waits for us (ADR-0001).
    ocr_tx: calloop::channel::Sender<Result<Vec<OcrLine>, String>>,
    /// The region a queued OCR job was read out of, while one is in
    /// flight: what the answer's diagnostic names, and the guard that
    /// keeps one key press from queueing two.
    ocr_job: Option<PhysRect>,
    /// The writable selection (spec D2). `None` on a compositor that
    /// advertises no data-control protocol - stock GNOME - which is a
    /// state discovered at bind time, exactly like a missing layer
    /// shell, and costs this one action and nothing else.
    clipboard: Option<clipboard::Clipboard>,
    /// Dictionary identities the pipeline last reported.
    dicts: Vec<DictInfo>,
    /// Trigger mode's hold, while one is held (ADR-0010).
    hold: Option<Hold>,
    /// The last lookup failure logged, so a moving cursor cannot repeat
    /// one line hundreds of times.
    last_warning: Option<String>,
    /// Whether a portal session is already serving pixels. The backend
    /// itself lives on the worker thread, so this is what the retry
    /// checks instead of holding it.
    portal_serving: bool,
    /// What the ladder picked, so `reload` knows whether a retry is
    /// even meaningful.
    capture_selection: capture_backend::Selection,
    /// Ticket 52: what the Capture row must say *besides* which backend
    /// serves it, when this compositor paints the pointer into the
    /// frames we OCR. Probed once at startup - the option cannot change
    /// without a compositor reload - and folded into every later
    /// Capture transition, so a portal retry cannot quietly drop the
    /// defect from the row.
    pointer_defect: Option<String>,
    /// Everything the portal retry needs to run again from here.
    portal_retry: Option<PortalRetry>,
    /// Where a new source goes. The dwell watch is the one source this
    /// daemon adds and drops at runtime, so the pump's own handle has
    /// to be reachable from the state that decides to.
    pump: LoopHandle<'static, App>,
    /// The dwell re-check's timer while one is armed (ADR-0010).
    dwell: Option<RegistrationToken>,
}

/// One blocking job for the popup's sake, as handed to the thread that
/// will run it.
enum AnkiCall {
    Dupes { generation: u64, exprs: Vec<String> },
    Add { expr: String, fields: HashMap<String, String> },
    /// A mined region's pixels: encode, write the PNG, and file the
    /// card that points at it. Here rather than on the grabbing thread
    /// because filing it *is* an AnkiConnect call and reads the same
    /// `[anki]` snapshot every other one does; the encode rides along
    /// because deflating a 4K region is not pump work either.
    Shot { plan: chibipop::shot::ShotPlan, bgra: Vec<u8>, w: i32, h: i32, files_a_card: bool },
}

/// One answer, as it comes back to the pump.
///
/// Failures travel as text rather than being printed where they
/// happen: the log lives on the pump thread.
enum AnkiOutcome {
    /// `Err` = AnkiConnect refused, or is not running at all.
    Dupes { generation: u64, dupes: Result<HashSet<String>, String> },
    Added { expr: String, note: Result<i64, String> },
    /// A mined picture's whole answer. `Ok(Some(note))` is saved and
    /// filed, `Ok(None)` saved with no card to file it on, `Err` a step
    /// that did not get there. `dir` is the folder it went to and never
    /// the file: the filename carries the word, which is screen content
    /// and does not belong in diagnostics (ADR-0006).
    Shot { expr: String, dir: PathBuf, filed: Result<Option<i64>, String> },
}

/// One screenshot's whole life on this side of the seam.
///
/// The two states are the two things a shot waits for, and they are
/// exclusive by construction: [`App::drain_shot`] takes the parked plan,
/// picks a region with the pump's own thread, and hands the same plan
/// straight to the grabbing state - so `Option<Shot>` also *is* the
/// "one pick at a time" rule (see [`App::park_shot`]).
enum Shot {
    /// Authorised, waiting for the region the user has yet to drag.
    ///
    /// Parked by [`App::execute`]'s `AddNote` arm or by
    /// `Verb::Screenshot`, drained at the top level of the pump, for the
    /// reason the Windows bin parks it at the bottom of its message loop
    /// (`crates/chibipop-windows/src/app.rs`): picking a region runs a
    /// nested pump, and entering one half-way through a command batch
    /// would re-enter the Controller's own dispatch.
    Parked(Pending),
    /// Region picked; a thread of its own is grabbing the pixels and
    /// the popup is off screen until they arrive, because a popup put
    /// back sooner would be *in* them.
    Grabbing(Pending),
}

/// One screenshot's plan, and which feature asked for it.
struct Pending {
    /// Every rule about the file and the note is core's (`chibipop::shot`,
    /// spec D4); this bin only picks a region and grabs pixels.
    plan: chibipop::shot::ShotPlan,
    kind: ShotKind,
}

/// Which of the two screenshot features owns a plan.
///
/// It decides exactly two things: what a shot with no picture falls back
/// to, and whether AnkiConnect is asked for a card at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShotKind {
    /// `actions.screenshot.include_on_add`: the Controller has already
    /// marked the popup adding, so the card is owed either way and a
    /// pick that answers nothing still files it - without a picture.
    Add,
    /// The mining screenshot (`actions.screenshot`, Windows'
    /// `MiningContextScreenshot`). Nothing is waiting on it, and
    /// `files_a_card` is the popup's own view of AnkiConnect at the
    /// moment the verb arrived: false still writes the PNG, it just has
    /// no card to ride on.
    Mining { files_a_card: bool },
}

impl ShotKind {
    /// Whether this shot's pixels are on their way to AnkiConnect. An
    /// add always is: `plan_add` only answers for a popup with a card
    /// on it, and the plain add it replaced would have gone out too.
    fn files_a_card(self) -> bool {
        match self {
            ShotKind::Add => true,
            ShotKind::Mining { files_a_card } => files_a_card,
        }
    }
}

impl AnkiCall {
    /// The blocking half, off the pump.
    fn run(self, anki: &chibipop::config::AnkiConfig) -> AnkiOutcome {
        match self {
            AnkiCall::Dupes { generation, exprs } => {
                let refs: Vec<&str> = exprs.iter().map(String::as_str).collect();
                let dupes =
                    chibipop::anki::find_duplicates(&anki.url, &anki.deck, &anki.model, &refs);
                AnkiOutcome::Dupes { generation, dupes: dupes.map_err(|e| format!("{e:#}")) }
            }
            AnkiCall::Add { expr, fields } => {
                let note = chibipop::anki::add_note(
                    &anki.url,
                    &anki.deck,
                    &anki.model,
                    &fields,
                    &anki.field_map,
                );
                AnkiOutcome::Added { expr, note: note.map_err(|e| format!("{e:#}")) }
            }
            // Every rule here is core's (spec D4): the filename, the
            // `source = "screenshot"` field lookup, the base64 payload
            // and the AnkiConnect call all live in `chibipop::shot`, so
            // this arm is three of its functions and no decisions.
            AnkiCall::Shot { plan, bgra, w, h, files_a_card } => {
                let filed = (|| -> Result<Option<i64>> {
                    let png = chibipop::image::encode_bgra_to_png(&bgra, w, h)?;
                    if files_a_card {
                        return chibipop::shot::save_and_add(&png, &plan, anki).map(Some);
                    }
                    // Anki cannot take a card: the picture is still
                    // worth writing, it just has nothing to ride on.
                    chibipop::shot::save(&png, &plan)?;
                    Ok(None)
                })();
                AnkiOutcome::Shot {
                    dir: plan.path.parent().unwrap_or(plan.path.as_path()).to_path_buf(),
                    expr: plan.expr,
                    filed: filed.map_err(|e| format!("{e:#}")),
                }
            }
        }
    }
}

/// What a second consent attempt needs, kept so the retry is the same
/// code path as the startup one.
struct PortalRetry {
    state_dir: PathBuf,
    globals: Vec<wayland::Advertised>,
    /// `Some` only when the cursor ladder actually selected rung 2;
    /// a rung that was never chosen must not be resurrected by a retry.
    cursor: Option<portal::CursorSink>,
}

/// ADR-0002's eager consent, start to finish.
///
/// Answers the backend when the portal said yes, and the capture
/// channel's row either way: a refusal is a status with a retry in it,
/// never an exit and never a panic.
fn open_portal(retry: &PortalRetry, log: &mut Log) -> (Option<PortalCapture>, ChannelState) {
    // The monitors have to be anchorable before the tray is published,
    // and the pump does not exist yet - hence the throwaway probe.
    let outputs = image_copy::probe_geometry(&retry.globals);
    // The layout origin, until the first grab moves us: the connected
    // stream follows the region being read (`PortalCapture::grab`), so
    // the startup guess only decides which monitor warms up first.
    let at = PhysPoint { x: 0, y: 0 };
    let opened = PortalSession::open(
        &retry.state_dir,
        &outputs,
        at,
        retry.cursor.clone(),
        |line| log.diag(&line),
    );
    match opened {
        Ok(session) => {
            let detail = format!(
                "portal ScreenCast + PipeWire, {} monitor(s) approved",
                session.monitors().len()
            );
            log.diag(&format!(
                "capture: {detail}; session {}, node {}, stream {:?}",
                session.session_path(),
                session.node_id(),
                session.health()
            ));
            (Some(PortalCapture::new(session)), ChannelState::up(detail))
        }
        Err(e) => {
            log.diag(&format!("capture: portal consent failed - {e}"));
            (None, ChannelState::down(e.detail()))
        }
    }
}

impl App {
    fn handle_request(&mut self, request: &str, verb: Option<Verb>) {
        let Some(verb) = verb else {
            self.log.diag(&format!("control: rejected {request:?}"));
            return;
        };
        let outcome = self.stub.apply(verb);
        self.log.diag(&format!("control: {} - {}", verb.as_str(), outcome));
        self.apply_verb(verb);
    }

    /// One verb's effect, whichever channel delivered it: the control
    /// socket (ADR-0003's rung 2, always bound) or the GlobalShortcuts
    /// portal (rung 1). Both land here on purpose — a portal press and a
    /// `chibipop ctl trigger-down` that could drift apart would be two
    /// trigger semantics, and the product has one. The same holds for
    /// the add: `Verb::AnkiAdd` is the *only* place the keyboard path to
    /// AnkiConnect exists, so `anki-add` over the socket and the portal
    /// `anki-add` shortcut cannot diverge.
    fn apply_verb(&mut self, verb: Verb) {
        match verb {
            Verb::Reload => self.reload_config(),
            // The canned popup stands in for a lookup, so the surface
            // can be driven on a machine with no dictionary at all.
            Verb::TriggerDown | Verb::Toggle if self.demo.armed => self.demo_show(),
            Verb::TriggerUp if self.demo.armed => self.hide_popup(),
            Verb::TriggerDown => self.trigger(trigger::down(self.hold)),
            Verb::TriggerUp => self.trigger(trigger::up(self.hold)),
            Verb::Toggle => self.trigger(trigger::toggle(self.hold)),
            // The same Event the in-panel Anki slot raises, so every
            // route to a card is one AnkiConnect flow.
            Verb::AnkiAdd => self.feed(Event::AddRequested),
            // Native-channel only (D1), like `static-region` below.
            Verb::Screenshot => self.mining_screenshot(),
            // Native-channel only (D1) too.
            Verb::OcrClipboard => self.ocr_to_clipboard(),
            // Native-channel only (D1): there is no portal id for this,
            // so the socket is the whole global channel and this arm is
            // the whole action.
            Verb::StaticRegion => self.pick_static_region(),
        }
    }

    /// One event from the GlobalShortcuts session's thread (ticket 36).
    ///
    /// The portal is an *additional* source of the same presses the
    /// socket carries, never a replacement, so a press goes through
    /// [`App::apply_verb`] and everything else here is observability:
    /// which channel owns the binding, what key it reports, and what
    /// the settings window is allowed to claim about it.
    fn handle_shortcut(&mut self, event: shortcuts::Event) {
        match event {
            shortcuts::Event::Bound(bindings) => self.trigger_bound("bound", bindings),
            shortcuts::Event::Changed(bindings) => self.trigger_bound("re-bound", bindings),
            shortcuts::Event::Fired { id, activated } => {
                self.log.diag(&format!(
                    "trigger: portal {} {}",
                    if activated { "activated" } else { "deactivated" },
                    id.as_str()
                ));
                match shortcuts::action(id, activated) {
                    shortcuts::Action::Verb(verb) => self.apply_verb(verb),
                    shortcuts::Action::Nothing => {}
                }
            }
            // The rung is not serving. The socket is, so this is a
            // status with a reason in it, never an exit.
            shortcuts::Event::Unavailable { reason, advice } => {
                self.log.diag(&format!("trigger: portal rung unavailable - {reason}"));
                // The row gets the short clause (ADR-0006: one line);
                // the way out belongs in the log, where there is room
                // for it.
                if let Some(advice) = advice {
                    self.log.diag(&format!("trigger: {advice}"));
                }
                self.note_channel(
                    ChannelId::Trigger,
                    ChannelState::up(shortcuts::native_detail(&reason)),
                );
                self.publish_trigger(&shortcuts::state::Published::native());
            }
            shortcuts::Event::Note(line) => self.log.diag(&line),
        }
    }

    /// The portal answered `BindShortcuts`, or the user re-bound a key
    /// in the desktop's own UI (`ShortcutsChanged`). Same three effects
    /// either way: a log line, the trigger row, and the file the
    /// settings window reads.
    fn trigger_bound(&mut self, what: &str, bindings: Vec<shortcuts::Binding>) {
        let detail = shortcuts::portal_detail(&bindings);
        self.log.diag(&format!("trigger: portal {what} - {detail}"));
        self.note_channel(ChannelId::Trigger, ChannelState::up(detail));
        self.publish_trigger(&shortcuts::state::Published::portal(bindings));
    }

    /// Tell the settings window who owns the binding (ADR-0005: the UI
    /// never lies about that). A file it cannot write is a diagnostic,
    /// not a failure — the trigger keeps working either way.
    fn publish_trigger(&mut self, published: &shortcuts::state::Published) {
        if let Err(e) = shortcuts::state::publish(&self.paths.state_dir, published) {
            self.log.diag(&format!("trigger: could not publish the channel state - {e}"));
        }
    }

    /// One trigger verb's effect (ADR-0010).
    ///
    /// A press freezes the output under the cursor and looks up what is
    /// there; a release drops the frame and retracts the popup. The
    /// grab is sent to the Worker *before* the lookup that follows it
    /// and the Worker serves its queue in order, so "the grab predates
    /// the popup" is a property of the ordering rather than a hope.
    fn trigger(&mut self, step: trigger::Step) {
        match step {
            trigger::Step::Freeze { latched } => {
                let Some(at) = self.cursor_now() else {
                    self.log.diag("trigger: no cursor sample yet - nothing to look up");
                    return;
                };
                let output = self.output_containing(at);
                self.freeze_at(at, output);
                self.hold = Some(Hold { output, latched });
                self.feed(Event::TriggerDown);
                // The press is its own first cursor sample: nothing has
                // to move for the first lookup to run.
                self.feed(Event::CursorMoved { pos: at });
            }
            trigger::Step::Release => {
                self.hold = None;
                self.thaw();
                self.feed(Event::TriggerUp);
            }
            trigger::Step::Nothing(why) => self.log.diag(&format!("trigger: {why}")),
        }
    }

    /// Take the press-time grab of `output`, on the Worker's thread.
    fn freeze_at(&mut self, at: PhysPoint, output: PhysRect) {
        let Some(worker) = self.worker.as_ref() else {
            self.log.diag(
                "trigger: no pipeline - a lookup needs capture, OCR models and a dictionary",
            );
            return;
        };
        // Freezes answer nothing, so the id is never matched against a
        // result; a failed grab is reported by the lookups behind it.
        let sent = worker
            .trigger()
            .send(Trigger { kind: TriggerKind::Freeze(at), id: RequestId(0) })
            .is_ok();
        if !sent {
            self.log.diag("trigger: the pipeline has gone away");
            self.worker = None;
            return;
        }
        self.log.diag(&format!(
            "trigger: frozen grab of output {},{} {}x{} for cursor {},{}",
            output.x, output.y, output.w, output.h, at.x, at.y
        ));
    }

    /// Drop the hold's frozen frame; grabs go live again.
    fn thaw(&mut self) {
        let Some(worker) = self.worker.as_ref() else { return };
        let _ = worker.trigger().send(Trigger { kind: TriggerKind::Thaw, id: RequestId(0) });
        self.log.diag("trigger: hold released, frozen grab dropped");
    }

    /// Where the cursor is *now*, for a press.
    ///
    /// The polling rung is asked directly: a press is exactly when its
    /// adaptive interval may be at its slowest (ADR-0010), and reading
    /// the position is free. The event rungs have already delivered
    /// their newest sample.
    fn cursor_now(&mut self) -> Option<PhysPoint> {
        if self.cursor_rung == Some(cursor::Rung::HyprctlPoll) {
            if let Some((lx, ly)) = hyprctl::sample() {
                self.last_poll = Some((lx, ly));
                if let Some(pos) = self.cursor.logical_to_global(f64::from(lx), f64::from(ly)) {
                    self.last_cursor = Some(pos);
                }
            }
        }
        self.last_cursor
    }

    /// The output box holding `at`, by the same arithmetic the capture
    /// backend uses - so the box the daemon records for a hold is the
    /// box the Worker actually froze.
    fn output_containing(&self, at: PhysPoint) -> PhysRect {
        crate::capture::geometry::bounds_containing(&self.cursor.geometries(), at)
    }

    /// The popup, from a Wayland dispatch. `run` always builds one -
    /// a compositor with no layer shell still gets a popup, it just has
    /// no shell to draw on (ticket 49), and a popup that cannot bind
    /// `wl_compositor`/`wl_shm` at all ends startup before any handler
    /// can run. The `Option` is what lets the daemon's own tests build
    /// an `App` with no compositor behind it.
    pub(crate) fn popup_mut(&mut self) -> &mut Popup {
        self.popup.as_mut().expect("a popup dispatch arrived with no popup bound")
    }

    /// Is there a popup that can actually put a panel on screen? False
    /// on stock GNOME, and the reason every draw path is gated rather
    /// than assumed.
    pub(crate) fn popup_can_draw(&self) -> bool {
        self.popup.as_ref().is_some_and(Popup::available)
    }

    /// Move the popup's diagnostics into the log. The popup owns no
    /// log: this thread does.
    pub(crate) fn flush_popup_notes(&mut self) {
        let Some(popup) = self.popup.as_mut() else { return };
        for line in popup.drain_notes() {
            self.log.diag(&line);
        }
    }

    // ---- the surfaces beside the popup (spec D5, ticket 03) ----
    //
    // Three kinds of layer surface answer into this one state, so every
    // shared SCTK handler routes by surface identity here rather than
    // forwarding to the popup and hoping. The popup's own behaviour is
    // unchanged: it still gets every event that names one of its
    // panels, and nothing else ever did.

    pub(crate) fn selector_mut(&mut self) -> Option<&mut Selector> {
        self.selector.as_mut()
    }

    /// The connection a pick makes its own queue on, and the daemon's
    /// queue handle for the wake it fires on the way out.
    pub(crate) fn selector_handles(&self) -> Result<(Connection, QueueHandle<App>)> {
        let selector = self
            .selector
            .as_ref()
            .context("this compositor advertises no layer shell, so there is nothing to drag on")?;
        Ok(selector.handles())
    }

    /// One selector diagnostic, from the pick's own code.
    pub(crate) fn selector_note(&mut self, line: String) {
        match self.selector.as_mut() {
            Some(selector) => selector.note(line),
            // No selector to hang it on, but the line is still true.
            None => self.log.diag(&line),
        }
    }

    /// Move the selector's, the pick's and both outlines' diagnostics
    /// into the log. Same seam as `flush_popup_notes`: none of the four
    /// owns a log.
    pub(crate) fn flush_surface_notes(&mut self) {
        let mut lines = Vec::new();
        if let Some(selector) = self.selector.as_mut() {
            lines.extend(selector.drain_notes());
        }
        if let Some(pick) = self.pick.as_mut() {
            lines.extend(pick.drain_notes());
        }
        if let Some(outline) = self.scan_outline.as_mut() {
            lines.extend(outline.drain_notes());
        }
        if let Some(outline) = self.static_outline.as_mut() {
            lines.extend(outline.drain_notes());
        }
        for line in lines {
            self.log.diag(&line);
        }
    }

    /// The outputs, as the selector and the outline need them. Read off
    /// the popup so all three surfaces share one `OutputState` and one
    /// idea of the global physical space.
    pub(crate) fn screens(&self) -> Vec<popup::Screen> {
        self.popup.as_ref().map(Popup::screens).unwrap_or_default()
    }

    /// The seat, for the pointer and keyboard a pick creates of its own.
    pub(crate) fn seat(&mut self) -> Option<WlSeat> {
        self.popup.as_mut()?.seats().seats().next()
    }

    // ---- one pick's lifecycle, driven by `select::Selector::pick` ----

    pub(crate) fn pick_start(&mut self, pick: Pick) {
        self.pick = Some(pick);
    }

    pub(crate) fn pick_arm(&mut self, signal: calloop::LoopSignal) {
        if let Some(pick) = self.pick.as_mut() {
            pick.arm(signal);
        }
    }

    /// One pump iteration: a drag delivers a motion per compositor
    /// frame, and one commit per frame is the pacing the popup gets from
    /// its frame callbacks. Also the place a decided pick leaves the
    /// loop from - see [`Pick::tick`].
    pub(crate) fn pick_tick(&mut self) {
        let (Some(pick), Some(selector)) = (self.pick.as_mut(), self.selector.as_mut()) else {
            return;
        };
        pick.tick(selector.pool());
    }

    pub(crate) fn pick_outcome(&self) -> select::Outcome {
        self.pick.as_ref().map_or(select::Outcome::Cancelled, Pick::outcome)
    }

    pub(crate) fn pick_expired(&mut self) {
        if let Some(pick) = self.pick.as_mut() {
            pick.expired();
        }
    }

    pub(crate) fn pick_key(&mut self, code: u32, pressed: bool) {
        if let Some(pick) = self.pick.as_mut() {
            pick.key(code, pressed);
        }
    }

    /// Take the selector down and answer how many surfaces went.
    pub(crate) fn pick_finish(&mut self) -> usize {
        self.flush_surface_notes();
        self.pick.take().map_or(0, Pick::destroy)
    }

    /// Drag a region on the dimmed screen, blocking until the user
    /// decides (spec D5). `None` is cancelled, under the threshold, or
    /// no selector on this compositor - all of them states, none an
    /// error.
    ///
    /// The popup is hidden for the duration: it must not be in the
    /// pixels a caller grabs afterwards, and the selector is modal
    /// anyway.
    pub(crate) fn pick_region(&mut self, deadline: Option<Duration>) -> Option<PhysRect> {
        let was_shown = self.popup.as_ref().and_then(Popup::shown).is_some();
        if was_shown {
            self.hide_popup();
        }
        let screens = self.screens();
        let started = Instant::now();
        let picked = Selector::pick(self, &screens, deadline);
        self.flush_surface_notes();
        self.log.diag(&format!(
            "select: pick took {} ms and answered {}",
            started.elapsed().as_millis(),
            match picked {
                Some(r) => format!("{}x{} at {},{}", r.w, r.h, r.x, r.y),
                None => "nothing".to_string(),
            }
        ));
        picked
    }

    // ---- the static sentence region ----

    /// `static-region`: draw the box
    /// [`chibipop::config::SentenceMode::Static`] reads the Anki
    /// sentence from.
    ///
    /// Works in **any** sentence mode, which is deliberate and matches
    /// Windows (`crates/chibipop-windows/src/app.rs`'s slot-1 hotkey):
    /// drawing the box is how a user decides to switch to Static, so
    /// refusing until the mode is already Static would be a chicken and
    /// egg. Setting one shows the border immediately, if the predicate
    /// wants it there.
    ///
    /// The border comes down first: it sits on `Layer::Overlay` like the
    /// selector, so leaving it up would draw last frame's box over the
    /// one being dragged.
    fn pick_static_region(&mut self) {
        if let Some(outline) = self.static_outline.as_mut() {
            outline.hide();
        }
        // `None` is the product's own deadline (`select::PICK_TIMEOUT`);
        // a second constant here would be a second answer to "how long
        // may a pick hold the pump".
        let picked = self.pick_region(None);
        self.took_static_region(picked);
    }

    /// What a finished pick means. `None` - cancelled, under the drag
    /// threshold, expired, or no layer shell to drag on - leaves
    /// everything exactly as it was, which is the whole contract of a
    /// cancel: nothing saved, no reload, and the border back where the
    /// predicate says it belongs.
    ///
    /// Split from [`App::pick_static_region`] because the pick is a
    /// nested pump that needs a compositor and this half is pure state:
    /// it is the seam the daemon tests drive.
    fn took_static_region(&mut self, picked: Option<PhysRect>) {
        let Some(rect) = picked else {
            self.log.diag("static region: pick cancelled - nothing changed");
            self.sync_static_outline();
            return;
        };
        self.config.anki.static_region = Some([rect.x, rect.y, rect.w, rect.h]);
        // The config file is the sole source of truth (ADR-0005), so the
        // region has to be *in* it before anything is re-derived from
        // it. Synchronous: this is a few KB of TOML on local disk at the
        // end of an interaction that just held the thread for as long as
        // the user took to drag, so a thread for it would buy nothing
        // and cost an ordering question. A failed write is a diagnostic
        // and a region that lasts until the next reload, not an exit.
        match self.config.save(&self.paths.config_file) {
            Ok(()) => self.log.diag(&format!(
                "static region: set to {}x{} at {},{} and saved to {}",
                rect.w,
                rect.h,
                rect.x,
                rect.y,
                self.paths.config_file.display()
            )),
            Err(e) => self.log.diag(&format!(
                "static region: set to {}x{} at {},{} but saving {} failed: {e:#}",
                rect.w,
                rect.h,
                rect.x,
                rect.y,
                self.paths.config_file.display()
            )),
        }
        // The Controller answers this with `RequestReload`, which is
        // what carries fresh `WorkerSettings` - and therefore the new
        // region - into the pipeline. Same push `reload_config` makes,
        // without re-reading a file we just wrote.
        let cfg = controller_config(&self.config);
        self.feed(Event::ConfigReloaded(Box::new(cfg)));
        self.sync_static_outline();
    }

    /// Put the static region's border where [`static_overlay_region`]
    /// says it belongs, or take it down.
    ///
    /// One function for all three call sites - startup, every config
    /// reload, and a region set - so the three-way condition is asked
    /// once and cannot drift between them (the Windows bin's rule, whose
    /// `LiveSettings::static_overlay_region` this predicate is).
    fn sync_static_outline(&mut self) {
        let wanted = static_overlay_region(&self.config);
        let screens = self.screens();
        let Some(outline) = self.static_outline.as_mut() else {
            // No layer shell, so no border - an honest degradation, said
            // once and only when something actually wanted one.
            if wanted.is_some() {
                self.log.diag(
                    "static region: no outline on this compositor (no zwlr_layer_shell_v1), \
                     so the border cannot be drawn; the region itself still serves lookups",
                );
            }
            return;
        };
        let was = outline.marks().first().map(|m| m.rect);
        // Told every time rather than only on a change: a compositor that
        // closed a pane leaves the desired state intact but the surface
        // gone, so re-asserting is what puts it back. The *log* is what
        // is gated below, because a reload in Line mode has nothing to
        // report.
        match wanted {
            Some(rect) => outline.show(&[overlay::Mark { rect, colour: overlay::BORDER }], &screens),
            None => outline.hide(),
        }
        let count = outline.surface_count();
        self.flush_surface_notes();
        if was == wanted {
            return;
        }
        self.log.diag(&match wanted {
            Some(r) => format!(
                "static region: outlining {}x{} at {},{} on {count} surface(s)",
                r.w, r.h, r.x, r.y
            ),
            None => "static region: outline hidden".to_string(),
        });
    }

    // ---- the mining screenshot and the picture that rides an add ----

    /// Where `actions.screenshot.save_dir` resolves to (ticket 02):
    /// absolute as typed, otherwise beside the exe in portable mode and
    /// under the XDG data dir everywhere else.
    fn screenshots_dir(&self) -> PathBuf {
        self.paths.screenshots_dir(&self.config.actions.screenshot.save_dir)
    }

    /// The picture that rides along with the add the Controller just
    /// authorised, or `None` when none does.
    ///
    /// Every part of that decision is core's (`chibipop::shot::plan_add`,
    /// spec D4): the `include_on_add` gate, the blank-expression and
    /// already-added guards, the filename and the picture field. This
    /// only hands it the popup, the config and the clock.
    fn plan_shot_for_add(&self) -> Option<chibipop::shot::ShotPlan> {
        let view = self.controller.popup()?;
        chibipop::shot::plan_add(&view, &self.config, &self.screenshots_dir(), epoch_secs())
    }

    /// `screenshot`: grab a region and file it as the mining context for
    /// the lookup on screen - Windows' `MiningContextScreenshot`.
    ///
    /// Windows gates the action on `popup_visible` and a top card
    /// (`action/screenshot.rs::is_available`) and does *nothing at all*
    /// when either is missing. The same gate holds here, because the
    /// picture is filed against the word on screen and there is no word
    /// without one - but the silence does not: a key the user bound and
    /// pressed gets a line saying why nothing happened, which is the
    /// only diagnosis available on a compositor bind (no dialog, no
    /// return code the user sees).
    fn mining_screenshot(&mut self) {
        let planned = self.controller.popup().map(|view| {
            (
                view.presentation.top.is_some(),
                // The ungated plan: the mining screenshot files a
                // picture whatever the popup's add state is, so it takes
                // none of `plan_add`'s guards and does not read
                // `include_on_add`.
                chibipop::shot::plan(&view, &self.config, &self.screenshots_dir(), epoch_secs()),
                // The popup's own view of AnkiConnect. False still
                // writes the PNG; it just has no card to ride on.
                view.anki.enabled && view.anki.connected,
            )
        });
        match planned {
            Some((true, plan, files_a_card)) => {
                self.log.diag(&format!(
                    "screenshot: mining context wanted; the picture {}",
                    if files_a_card {
                        "will be filed on a card"
                    } else {
                        "will be saved with no card (AnkiConnect is not serving this popup)"
                    }
                ));
                self.park_shot(Pending { plan, kind: ShotKind::Mining { files_a_card } });
            }
            _ => self.log.diag(
                "screenshot: nothing to file - the mining screenshot captures the context of \
                 the lookup on screen, and no popup is showing one",
            ),
        }
    }

    /// Park a plan and ask the pump to drain it once this batch is over.
    ///
    /// The idle callback is calloop's own "the loop finished dispatching
    /// and has nothing left to do", which is exactly the seam the
    /// Windows bin gets from the bottom of its message loop: the pick
    /// below runs a nested pump, and running one inside a command batch
    /// would re-enter the Controller's dispatch half-way through one.
    fn park_shot(&mut self, shot: Pending) {
        if self.shot.is_some() {
            // Two picks cannot share the screen, and the second would be
            // dragged over a dim the first put there. Reachable: the
            // control socket drains every waiting connection in one
            // callback, so two `chibipop ctl` presses can land together.
            self.log
                .diag("screenshot: a region pick is already owed, so this one is refused");
            self.shot_without_picture(shot, "another pick is in flight");
            return;
        }
        self.shot = Some(Shot::Parked(shot));
        self.pump.insert_idle(|app: &mut App| app.drain_shot());
    }

    /// The OS half of a parked shot: hide, drag, grab (spec D5/D6).
    ///
    /// The hide is [`App::pick_region`]'s; the popup stays down until the
    /// pixels are in, because the whole point of the sequence is that it
    /// is not in them.
    fn drain_shot(&mut self) {
        let Some(Shot::Parked(shot)) = self.shot.take() else { return };
        // `None` is the product's own deadline (`select::PICK_TIMEOUT`),
        // the same one `static-region` takes: a second constant here
        // would be a second answer to "how long may a pick hold the
        // pump".
        let picked = self.pick_region(None);
        self.took_shot_region(picked, shot);
    }

    /// What a finished pick means. Split from [`App::drain_shot`] for the
    /// reason `took_static_region` is split from `pick_static_region`:
    /// the pick is a nested pump that needs a compositor and this half
    /// is pure state, so this is the seam the daemon tests drive.
    fn took_shot_region(&mut self, picked: Option<PhysRect>, shot: Pending) {
        match picked {
            Some(region) => self.spawn_shot(region, shot),
            None => {
                self.restore_popup();
                self.shot_without_picture(shot, "no region was picked");
            }
        }
    }

    /// One arbitrary-rect grab, on a thread of its own (spec D6).
    ///
    /// The plan stays here rather than riding into the closure: a
    /// `Builder::spawn` that fails drops its closure, and a plan lost
    /// that way would leave the popup at "Adding…" for good.
    fn spawn_shot(&mut self, region: PhysRect, shot: Pending) {
        self.shot = Some(Shot::Grabbing(shot));
        let setup = self.capture_setup();
        let tx = self.shot_tx.clone();
        let spawned = std::thread::Builder::new()
            .name("chibipop-shot".to_string())
            .spawn(move || {
                let grabbed = capture::oneshot(&setup, region).map_err(|e| format!("{e:#}"));
                let _ = tx.send(grabbed);
            });
        if let Err(e) = spawned {
            self.log.diag(&format!("screenshot: no thread for the grab - {e}"));
            self.restore_popup();
            if let Some(Shot::Grabbing(shot)) = self.shot.take() {
                self.shot_without_picture(shot, "the grab could not be started");
            }
        }
    }

    /// What a one-shot grab needs to open a backend of its own: the
    /// startup capability probe, the rung the ADR-0002 ladder picked, and
    /// the state dir the portal rung keeps its restore token in - so a
    /// second session on rung 2 is silent instead of prompting again.
    fn capture_setup(&self) -> capture::Setup {
        capture::Setup {
            globals: self.worker_setup.globals.clone(),
            backend: self.worker_setup.backend,
            state_dir: self.paths.state_dir.clone(),
        }
    }

    /// The grabbing thread answered. The popup goes back up here and
    /// nowhere earlier, and the pixels go on to the call that writes and
    /// files them - off this thread again (ADR-0001).
    fn handle_shot(&mut self, grabbed: Result<Frame, String>) {
        let Some(Shot::Grabbing(shot)) = self.shot.take() else {
            self.log.diag("screenshot: pixels arrived with no shot waiting for them");
            return;
        };
        self.restore_popup();
        match grabbed {
            Ok(frame) => {
                self.log.diag(&format!(
                    "screenshot: grabbed {}x{} from {}",
                    frame.w, frame.h, frame.source
                ));
                self.spawn_anki(AnkiCall::Shot {
                    plan: shot.plan,
                    bgra: frame.buf,
                    w: frame.w,
                    h: frame.h,
                    files_a_card: shot.kind.files_a_card(),
                });
            }
            // Windows' rule exactly: a grab that failed is a card
            // without a picture, not a card that never happens.
            Err(e) => {
                self.log.diag(&format!("screenshot: the grab failed - {e}"));
                self.shot_without_picture(shot, "the grab failed");
            }
        }
    }

    /// A shot that will carry no picture - cancelled, expired, or a grab
    /// that did not come back.
    ///
    /// An add still files its card: `start_add` marked the popup adding
    /// before it authorised the picture, so this dispatch is the only
    /// thing that ever clears "Adding…". A mining screenshot has nobody
    /// waiting on it and stops here.
    fn shot_without_picture(&mut self, shot: Pending, why: &str) {
        match shot.kind {
            ShotKind::Add => {
                self.log.diag(&format!(
                    "screenshot: {why} - the card goes in without a picture"
                ));
                let Pending { plan, .. } = shot;
                self.spawn_anki(AnkiCall::Add { expr: plan.expr, fields: plan.fields });
            }
            ShotKind::Mining { .. } => {
                self.log.diag(&format!("screenshot: {why} - nothing was saved"));
            }
        }
    }

    /// Put the popup back after a pick and a grab took it off screen.
    ///
    /// Two guards, because the pump ran while the grab did: something
    /// already on screen is newer than what we took down, and a
    /// Controller with no popup retracted it (`HidePopup`) while we were
    /// away - re-showing then would resurrect a popup the state machine
    /// has finished with. The Anki state comes from the Controller for
    /// the reason [`App::sync_anki_slot`] exists: the slot is painted
    /// into the panel, so a re-raster carries whatever it says now.
    fn restore_popup(&mut self) {
        if self.popup.as_ref().and_then(Popup::shown).is_some() || !self.controller.is_shown() {
            return;
        }
        let Some(req) = self.popup.as_ref().and_then(Popup::request).cloned() else {
            return;
        };
        self.show_popup(&ShowRequest { anki: self.controller.anki().cloned(), ..req });
    }

    // ---- OCR to the clipboard ----

    /// `ocr-clipboard`: pick a region, read it, and take the selection.
    ///
    /// Both refusals happen *before* the pick, because dragging a box
    /// that can go nowhere is worse than being told so up front: a
    /// compositor with no data-control protocol has no clipboard
    /// chibipop can write (spec D2), and a region still waiting on the
    /// recogniser owns the answer channel this one would arrive on.
    fn ocr_to_clipboard(&mut self) {
        if self.clipboard.is_none() {
            self.log.diag(&clipboard::unavailable_line());
            return;
        }
        if let Some(busy) = self.ocr_job {
            self.log.diag(&format!(
                "ocr-clipboard: a {}x{} region is still waiting on the recogniser, so this \
                 press is refused",
                busy.w, busy.h
            ));
            return;
        }
        // `None` is the product's own deadline (`select::PICK_TIMEOUT`);
        // a second constant here would be a second answer to "how long
        // may a pick hold the pump".
        let picked = self.pick_region(None);
        self.took_ocr_region(picked);
    }

    /// What a finished pick means.
    ///
    /// Split from [`App::ocr_to_clipboard`] for the reason
    /// [`App::took_static_region`] is: the pick is a nested pump that
    /// needs a compositor and this half is state, so this is the seam
    /// the daemon tests drive.
    fn took_ocr_region(&mut self, picked: Option<PhysRect>) {
        let Some(region) = picked else {
            self.log.diag("ocr-clipboard: pick cancelled - the clipboard is untouched");
            self.restore_popup();
            return;
        };
        self.ocr_job = Some(region);
        self.spawn_ocr_read(region);
    }

    /// Grab the region and hand its pixels to the Worker's engine.
    ///
    /// Two off-pump stages, one thread each and neither of them this
    /// one: the grab opens a capture backend of its own (spec D6), and
    /// the recogniser is thread-affine to the Worker (ADR-0009), so the
    /// grab thread forwards straight into [`worker::OcrJobs`] rather
    /// than bouncing a whole frame off the pump that has no use for it.
    /// Only the text comes back here, as an event (ADR-0001).
    fn spawn_ocr_read(&mut self, region: PhysRect) {
        let setup = self.capture_setup();
        let jobs = self.ocr_jobs.clone();
        let answer = self.ocr_tx.clone();
        let spawned = std::thread::Builder::new()
            .name("chibipop-ocr-clip".to_string())
            .spawn(move || {
                // Native resolution: this adapter never upscales, and
                // meikiocr is strictly worse on 2x crops on every
                // benchmark slice (ADR-0009). The Windows twin's 2x is
                // its engine's fact, not this one's.
                match capture::oneshot(&setup, region) {
                    Ok(frame) => {
                        let request = worker::OcrRequest {
                            bgra: frame.buf,
                            w: frame.w,
                            h: frame.h,
                            answer: answer.clone(),
                        };
                        // A queue with no pipeline behind it answers
                        // here rather than parking the job forever.
                        if let Err(e) = jobs.send(request) {
                            let _ = answer.send(Err(format!("{e:#}")));
                        }
                    }
                    Err(e) => {
                        let _ = answer.send(Err(format!("grabbing the region failed - {e:#}")));
                    }
                }
            });
        if let Err(e) = spawned {
            self.log.diag(&format!("ocr-clipboard: no thread for the grab - {e}"));
            self.ocr_job = None;
            self.restore_popup();
        }
    }

    /// The recogniser answered: join the lines and take the selection.
    ///
    /// The joining rule is core's ([`chibipop::text::layout::join_lines`]),
    /// shared with the Windows action so one region read on either
    /// platform copies the same text.
    fn handle_ocr_text(&mut self, read: Result<Vec<OcrLine>, String>) {
        let Some(region) = self.ocr_job.take() else {
            self.log.diag("ocr-clipboard: text arrived with no region waiting for it");
            return;
        };
        self.restore_popup();
        let lines = match read {
            Ok(lines) => lines,
            Err(e) => {
                self.log.diag(&format!("ocr-clipboard: {e}"));
                return;
            }
        };
        let text = chibipop::text::layout::join_lines(&lines);
        if text.is_empty() {
            self.log.diag(&format!(
                "ocr-clipboard: nothing readable in the {}x{} region - the clipboard is untouched",
                region.w, region.h
            ));
            return;
        }
        // Counts, never the text: what the user read is screen content
        // and diagnostics are not opted in to (ADR-0006).
        let chars = text.chars().count();
        let Some(board) = self.clipboard.as_ref() else {
            self.log.diag(&clipboard::unavailable_line());
            return;
        };
        match board.set(&text) {
            Ok(()) => self.log.diag(&format!(
                "ocr-clipboard: {chars} character(s) on {} line(s) copied from the {}x{} region \
                 via {}",
                lines.len(),
                region.w,
                region.h,
                board.rung().global()
            )),
            Err(e) => self.log.diag(&format!("ocr-clipboard: the copy failed - {e:#}")),
        }
    }

    /// `CHIBIPOP_SURFACE_PROBE=1`: drive the outline and the region
    /// selector once at startup and say what they did.
    ///
    /// The role `CHIBIPOP_POPUP_DEMO` plays for the popup and
    /// `capture-dump` plays for the capture ladder. These two surfaces
    /// are otherwise only reachable from features that do not exist yet,
    /// and a surface nobody can put on screen is a surface nobody can
    /// check - so this maps them both against a real compositor, paints
    /// them, and takes them down. It synthesizes no seat input: the pick
    /// is left to hit its deadline, which is also how the wedge guard
    /// gets exercised.
    fn probe_surfaces(&mut self, queue: &mut wayland_client::EventQueue<App>) {
        let screens = self.screens();
        let Some(screen) = screens.first().cloned() else {
            self.log.diag("probe: no output to put a surface on");
            return;
        };
        self.log.diag(&format!(
            "probe: {SURFACE_PROBE_ENV}=1 on {} ({}x{} at {:.3}x)",
            screen.name, screen.rect.w, screen.rect.h, screen.scale
        ));

        // Two boxes well inside the output, so the run is reproducible
        // on any monitor: one wide, one square, far enough apart that
        // their bounding box is bigger than either. They go in through
        // the shipped consumer rather than straight at `Outline::show`,
        // so what the probe puts on screen is exactly what a hover puts
        // on screen - two kinds, two theme colours, both outset by the
        // border (`Command::ShowScanOverlay`).
        let scan = vec![
            ScanRect {
                rect: PhysRect { x: screen.rect.x + 100, y: screen.rect.y + 100, w: 240, h: 60 },
                kind: ScanKind::Pass1,
            },
            ScanRect {
                rect: PhysRect { x: screen.rect.x + 500, y: screen.rect.y + 300, w: 80, h: 80 },
                kind: ScanKind::Match,
            },
        ];
        self.execute(Command::ShowScanOverlay { rects: scan });
        match self.scan_outline.as_ref().map(Outline::surface_count) {
            Some(count) => {
                // The outline's surfaces live on the daemon's queue, so
                // the round trip is what carries their configures back
                // and puts the strips on screen; without it the probe
                // would prove only that the requests were sent.
                if let Err(e) = queue.roundtrip(self) {
                    self.log.diag(&format!("probe: outline round trip failed: {e}"));
                }
                self.flush_surface_notes();
                self.log.diag(&format!("probe: outline shown on {count} surface(s)"));
            }
            None => self.log.diag("probe: no outline on this compositor"),
        }

        // One pick, left to expire: no input is synthesized, so this
        // proves map, configure, paint, nested pump, deadline and
        // teardown without touching the user's pointer.
        let picked = self.pick_region(Some(SURFACE_PROBE_DEADLINE));
        self.log.diag(&format!("probe: pick answered {}", picked.is_some()));

        // The empty vector is the hide, on both platforms
        // (`controller.rs`), so the probe takes the outline down the way
        // a retracted popup does.
        self.execute(Command::ShowScanOverlay { rects: Vec::new() });
        if let Some(left) = self.scan_outline.as_ref().map(|o| o.marks().len()) {
            if let Err(e) = queue.roundtrip(self) {
                self.log.diag(&format!("probe: outline round trip failed: {e}"));
            }
            self.flush_surface_notes();
            self.log.diag(&format!("probe: outline hidden, {left} rect(s) left"));
        }
    }

    // ---- routing ----

    /// A layer surface was configured. Whose it is decides everything:
    /// the pick paints its dim, either outline its strips, the popup its
    /// panel.
    pub(crate) fn layer_configured(&mut self, layer: &LayerSurface, size: (u32, u32)) {
        if self.pick.as_ref().is_some_and(|p| p.owns_layer(layer)) {
            if let Some(pick) = self.pick.as_mut() {
                pick.configured(layer, size);
            }
            // The dim has to be on screen before the drag starts, and
            // the pump's own repaint is one iteration away.
            self.pick_tick();
            self.flush_surface_notes();
            return;
        }
        if self.scan_outline.as_ref().is_some_and(|o| o.owns_layer(layer)) {
            if let Some(outline) = self.scan_outline.as_mut() {
                outline.configured(layer, size);
            }
            self.flush_surface_notes();
            return;
        }
        if self.static_outline.as_ref().is_some_and(|o| o.owns_layer(layer)) {
            if let Some(outline) = self.static_outline.as_mut() {
                outline.configured(layer, size);
            }
            self.flush_surface_notes();
            return;
        }
        if self.popup.as_ref().is_some_and(|p| p.owns_layer(layer)) {
            self.popup_mut().configured(layer, size);
            self.flush_popup_notes();
            // A resize painted here rather than in `show`, so this is
            // where a scripted pointer pass finds its frame.
            self.run_pointer_script();
        }
    }

    /// The compositor closed a layer surface. For the popup that is
    /// routine recreation (ADR-0004); for a pick it is a cancel; for an
    /// outline it is one surface fewer until the next show.
    pub(crate) fn layer_closed(&mut self, layer: &LayerSurface) {
        if self.pick.as_ref().is_some_and(|p| p.owns_layer(layer)) {
            if let Some(pick) = self.pick.as_mut() {
                pick.closed(layer);
            }
            self.flush_surface_notes();
            return;
        }
        if self.scan_outline.as_ref().is_some_and(|o| o.owns_layer(layer)) {
            if let Some(outline) = self.scan_outline.as_mut() {
                outline.drop_layer(layer);
            }
            self.flush_surface_notes();
            return;
        }
        if self.static_outline.as_ref().is_some_and(|o| o.owns_layer(layer)) {
            if let Some(outline) = self.static_outline.as_mut() {
                outline.drop_layer(layer);
            }
            self.flush_surface_notes();
            return;
        }
        if self.popup.is_some() {
            self.popup_mut().drop_layer(layer);
            self.flush_popup_notes();
        }
    }

    /// A `wl_surface.frame` callback. Only the popup asks for one - it
    /// is the only surface here whose commits are paced by the refresh
    /// rate - so anything else naming one is not ours to act on.
    pub(crate) fn surface_frame(&mut self, surface: &WlSurface) {
        if !self.popup.as_ref().is_some_and(|p| p.owns(surface)) {
            return;
        }
        self.popup_mut().frame_done(surface);
        self.flush_popup_notes();
        self.run_pointer_script();
    }

    /// One `wl_pointer` frame. A pick owns the pointer while it is up -
    /// its surfaces cover the whole output, so nothing else can be under
    /// the cursor - and the popup gets every frame that is not a pick's.
    pub(crate) fn pointer_frame(&mut self, events: &[PointerEvent]) {
        if let Some(pick) = self.pick.as_mut() {
            if select::pointer_frame(pick, events) {
                self.flush_surface_notes();
                return;
            }
        }
        if self.popup.is_none() {
            return;
        }
        let interactions = popup::pointer_frame(self.popup_mut(), events);
        self.flush_popup_notes();
        self.pointer_interactions(interactions);
    }

    /// A `preferred_scale` arrived. The scale is never latched
    /// (ADR-0004): Hyprland may send 1.0 first and correct it later, so
    /// a change to the surface currently showing is re-rastered and
    /// re-placed, and the Controller hears the new rect.
    pub(crate) fn popup_rescaled(&mut self, idx: usize, scale_120ths: u32) {
        let moved = self.popup.as_mut().is_some_and(|p| p.preferred_scale(idx, scale_120ths));
        self.flush_popup_notes();
        if !moved {
            return;
        }
        self.log.diag(&format!(
            "popup: surface {idx} preferred scale {:.3} - re-rendering",
            f64::from(scale_120ths) / 120.0
        ));
        let outcome = self.popup.as_mut().map(Popup::reshow);
        self.flush_popup_notes();
        match outcome {
            Some(Ok(Some(placed))) => self.placed(placed),
            Some(Err(e)) => self.log.diag(&format!("popup: re-render failed: {e:#}")),
            _ => {}
        }
    }

    /// Popup-local pointer input (ticket 38) -> Controller Events.
    ///
    /// The other half of ADR-0003's contextual-interaction bargain:
    /// there is no global wheel or click channel on Wayland, so these
    /// arrive from the popup's own input region and nowhere else.
    pub(crate) fn pointer_interactions(&mut self, interactions: Vec<popup::Interaction>) {
        for interaction in interactions {
            match interaction {
                popup::Interaction::Scroll { notches } => {
                    self.log.diag(&format!("pointer: wheel {notches:+} notch(es) over the panel"));
                    self.feed(Event::Scrolled { notches });
                }
                popup::Interaction::Click { local, hit } => {
                    self.log.diag(&format!(
                        "pointer: click at panel {},{} -> {}",
                        local.x,
                        local.y,
                        match &hit {
                            Some(hit) => format!("{hit:?}"),
                            None => "no target".to_string(),
                        }
                    ));
                    self.feed(Event::Clicked { local, hit });
                }
                // Core reserves the slot and the painter fills it
                // (ADR-0004); the Controller decides whether a click
                // on it is an add at all.
                popup::Interaction::Anki { local } => {
                    self.log.diag(&format!(
                        "pointer: click at panel {},{} -> the Anki slot",
                        local.x, local.y
                    ));
                    self.feed(Event::AddRequested);
                }
            }
        }
    }

    /// The scripted pointer passes (`CHIBIPOP_POINTER_SCRIPT`), driven
    /// from here rather than inside the popup.
    ///
    /// Why here: each step's effect has to reach the Controller and
    /// come back as a repaint *before* the next step resolves, or a
    /// scripted scroll would be followed by a click against the frame
    /// it just replaced. Called after every path that may have painted
    /// (a synchronous show, a configure, a frame callback), because
    /// which of the three actually rasters depends on whether the
    /// surface had to be resized first. Re-entrant calls return at once
    /// and the loop below picks up the pass they armed.
    pub(crate) fn run_pointer_script(&mut self) {
        if self.scripting {
            return;
        }
        self.scripting = true;
        while let Some(steps) = self.popup.as_mut().and_then(Popup::take_pass) {
            let Some(panel) = self.popup.as_ref().and_then(Popup::shown).map(|s| s.output) else {
                break;
            };
            for step in steps {
                self.pointer_step(panel, step);
            }
        }
        self.scripting = false;
    }

    /// One scripted step, through the same entry points a real
    /// `wl_pointer` frame drives.
    fn pointer_step(&mut self, panel: usize, step: popup::Step) {
        if self.popup.is_none() {
            return;
        }
        let interaction = match step {
            popup::Step::Enter(x, y) => {
                self.log.diag(&format!("pointer: script enter at {x},{y} logical"));
                self.popup_mut().pointer_enter(panel, (x, y), None);
                None
            }
            popup::Step::Motion(x, y) => {
                self.popup_mut().pointer_motion((x, y));
                let at = self.popup_mut().hit_at((x, y));
                self.log.diag(&format!("pointer: script motion at {x},{y} logical -> {at}"));
                None
            }
            popup::Step::Click(x, y) => {
                self.log.diag(&format!("pointer: script click at {x},{y} logical"));
                self.popup_mut().pointer_button((x, y))
            }
            popup::Step::Wheel(value120) => {
                self.log.diag(&format!("pointer: script wheel value120 {value120}"));
                self.popup_mut().pointer_wheel_120(value120)
            }
            popup::Step::Leave => {
                self.popup_mut().pointer_leave(panel);
                None
            }
            popup::Step::Dump => {
                self.popup_mut().dump_hits();
                None
            }
        };
        self.flush_popup_notes();
        self.pointer_interactions(interaction.into_iter().collect());
    }

    /// One Event through the Controller, and every Command it answers
    /// with executed - then the dwell watch brought in line with what
    /// is now on screen (ADR-0010).
    fn feed(&mut self, event: Event) {
        for cmd in self.controller.handle(event) {
            self.execute(cmd);
        }
        self.sync_dwell();
    }

    /// Whether the dwell re-check has anything to watch here and now.
    fn dwell_wanted(&self) -> bool {
        dwell_wanted(self.hold, self.controller.dwell_armed())
    }

    /// Arm the dwell watch when there is something to watch.
    ///
    /// Disarming is the timer's own job (see [`App::dwell_tick`]): a
    /// source must not be removed from inside its own dispatch, and a
    /// watch that retires on its next deadline still leaves an idle
    /// daemon holding no timed source at all - which is what ADR-0010's
    /// zero idle wakeups means on an event-driven cursor rung.
    fn sync_dwell(&mut self) {
        if self.dwell.is_some() || !self.dwell_wanted() {
            return;
        }
        self.arm_dwell();
    }

    /// One dwell watch, from now. Unconditional: `sync_dwell` owns the
    /// decision, and the tests arm one by hand because a popup with a
    /// rect needs a compositor.
    fn arm_dwell(&mut self) {
        let timer = Timer::from_duration(budget::DWELL);
        match self.pump.insert_source(timer, |_, _, app: &mut App| app.dwell_tick()) {
            Ok(token) => {
                self.dwell = Some(token);
                if self.trace {
                    self.log.diag("dwell: watch armed");
                }
            }
            Err(e) => self.log.diag(&format!("dwell: no watch could be armed - {e}")),
        }
    }

    /// One dwell deadline: re-ask the shown popup's own question, then
    /// decide whether this watch is still wanted (ADR-0010).
    ///
    /// The re-grab races damage on the same deadline below the seams, so
    /// an unchanged screen costs no copy and no OCR pass, and the
    /// Controller re-presents nothing; only a change reaches the popup.
    fn dwell_tick(&mut self) -> TimeoutAction {
        if self.trace {
            self.log.diag("dwell: deadline");
        }
        self.feed(Event::DwellElapsed);
        if self.dwell_wanted() {
            return TimeoutAction::ToDuration(budget::DWELL);
        }
        self.dwell = None;
        if self.trace {
            self.log.diag("dwell: nothing shown - watch retired");
        }
        TimeoutAction::Drop
    }

    fn execute(&mut self, cmd: Command) {
        match cmd {
            // What OCR must not read is our own popup, and only on a
            // live grab: a frozen hold's pixels predate it (ADR-0008,
            // ADR-0010). Wayland has no protocol-level surface
            // exclusion, so this rect is the whole mechanism.
            Command::RequestLookup { id, point, popup } => {
                let mask = CaptureMask::for_mode(self.capture_mode(), popup);
                self.send_trigger(TriggerKind::Hover(Hover { at: point, mask }), id);
            }
            Command::RequestDrillDown { id, text } => {
                self.send_trigger(TriggerKind::DrillDown(text), id);
            }
            Command::RequestReload { id } => {
                let settings = worker::settings(&self.config, &self.dicts);
                self.send_trigger(TriggerKind::Reload(Box::new(settings)), id);
            }
            // New content, so a scripted pass is owed one frame from
            // now. A `RepaintPopup` below is deliberately *not* armed:
            // a pass that re-ran on its own scroll repaint would drive
            // itself in a circle.
            Command::ShowPopup { presentation, anchor, scroll, show_back } => {
                if let Some(popup) = self.popup.as_mut() {
                    popup.arm_script();
                }
                self.show_popup(&ShowRequest {
                    presentation: *presentation,
                    anchor,
                    scroll,
                    show_back,
                    // The slot is painted into the panel here, not
                    // hung beside it as on Windows (ADR-0004), so
                    // every raster carries the affordance's own state.
                    anki: self.controller.anki().cloned(),
                });
            }
            Command::RepaintPopup { scroll, show_back } => {
                let anki = self.controller.anki().cloned();
                let req = self.popup.as_ref().and_then(Popup::request).map(|req| ShowRequest {
                    scroll,
                    show_back,
                    anki,
                    ..req.clone()
                });
                if let Some(req) = req {
                    self.show_popup(&req);
                }
            }
            Command::HidePopup => self.hide_popup(),
            // Two shipped settings draw through here and nowhere else:
            // `debug.show_scan_region` (the capture boxes) and
            // `popup.highlight_match` (the word being defined, which
            // core's `matched` field otherwise only feeds hold-region
            // arithmetic). An empty vector is the hide.
            Command::ShowScanOverlay { rects } => self.show_scan_overlay(&rects),
            // A fresh popup replaces the old one: the sub-notch delta
            // banked against the entry that just went away must not
            // nudge the new one (ticket 38).
            Command::DiscardScroll => {
                if let Some(popup) = self.popup.as_mut() {
                    popup.discard_scroll();
                }
            }
            // Screen content: written only where the user opted in.
            Command::LogLookup { headword, match_len } => {
                self.log.lookup(&format!("{headword}  match={match_len}"));
            }
            // A cursor crossing text the dictionary cannot serve would
            // otherwise repeat one line per sample.
            Command::WarnLookupFailed(msg) => {
                if self.last_warning.as_deref() != Some(msg.as_str()) {
                    self.log.diag(&format!("lookup failed: {msg}"));
                    self.last_warning = Some(msg);
                }
            }
            Command::SyncAnkiButton => self.sync_anki_slot(),
            Command::CheckDupes { generation, exprs } => {
                self.spawn_anki(AnkiCall::Dupes { generation, exprs });
            }
            // The screenshot-on-add seam (spec D4, ticket 01). A plan
            // means a picture rides along, so the plain add is *not*
            // dispatched: the call that carries the picture files the
            // card. The OS half cannot happen here - see
            // [`App::park_shot`].
            Command::AddNote { expr, fields } => match self.plan_shot_for_add() {
                Some(plan) => self.park_shot(Pending { plan, kind: ShotKind::Add }),
                None => self.spawn_anki(AnkiCall::Add { expr, fields }),
            },
            // The arming rows (`Set*Armed`, `SetCursorShape`) come off
            // the Windows dispatch tick, which this daemon does not
            // have: nothing here is armed per tick, because nothing
            // here hooks the seat (ADR-0003, ADR-0010).
            other => self.log.diag(&format!("controller: {other:?} (no-op)")),
        }
    }

    /// One trigger into the pipeline, or one line saying there is none.
    fn send_trigger(&mut self, kind: TriggerKind, id: RequestId) {
        let Some(worker) = self.worker.as_ref() else {
            self.log.diag("lookup: no pipeline - nothing to ask");
            return;
        };
        if worker.trigger().send(Trigger { kind, id }).is_err() {
            self.log.diag("lookup: the pipeline has gone away");
            self.worker = None;
        }
    }

    /// How this lookup's pixels relate to the popup, in time: a hold
    /// reads the press-time frame, everything else reads the screen.
    fn capture_mode(&self) -> CaptureMode {
        match self.hold {
            Some(_) => CaptureMode::Frozen,
            None => CaptureMode::Live,
        }
    }

    /// The Worker answered. Only the freshest queued result matters:
    /// the older ones were superseded before they arrived (latest-wins,
    /// as on Windows).
    fn drain_results(&mut self) {
        let mut freshest = None;
        if let Some(worker) = self.worker.as_ref() {
            while let Ok(result) = worker.results().try_recv() {
                freshest = Some(result);
            }
        }
        if let Some(result) = freshest {
            self.feed(Event::LookupResult { id: result.id, outcome: result.outcome });
        }
    }

    /// One AnkiConnect call, off the pump.
    ///
    /// Every one of them is a blocking `ureq` request to a server that
    /// may not be running at all, so none may happen on this thread: a
    /// two-second connect timeout here would be two seconds of frozen
    /// popup. The answer comes back as an event, like the Worker's
    /// (ADR-0001).
    fn spawn_anki(&mut self, call: AnkiCall) {
        let anki = self.config.anki.clone();
        let tx = self.anki_tx.clone();
        let spawned = std::thread::Builder::new()
            .name("chibipop-anki".to_string())
            .spawn(move || {
                let _ = tx.send(call.run(&anki));
            });
        if let Err(e) = spawned {
            self.log.diag(&format!("anki: no thread for the AnkiConnect call - {e}"));
        }
    }

    /// One AnkiConnect answer, back on the pump thread.
    ///
    /// The lines carry counts and note ids and never the expression
    /// itself: what the user read is screen content, and diagnostics
    /// are not opted in to (ADR-0006).
    fn handle_anki(&mut self, outcome: AnkiOutcome) {
        match outcome {
            AnkiOutcome::Dupes { generation, dupes } => {
                let dupes = match dupes {
                    Ok(dupes) => {
                        self.log.diag(&format!(
                            "anki: dupe check answered - {} of the popup's expressions are already in the deck",
                            dupes.len()
                        ));
                        Some(dupes)
                    }
                    Err(e) => {
                        self.log.diag(&format!("anki: dupe check failed - {e}"));
                        None
                    }
                };
                self.feed(Event::DupesChecked { generation, dupes });
            }
            AnkiOutcome::Added { expr, note } => {
                let failed = match note {
                    Ok(id) => {
                        self.log.diag(&format!("anki: card added as note {id}"));
                        false
                    }
                    Err(e) => {
                        self.log.diag(&format!("anki: adding the card failed - {e}"));
                        true
                    }
                };
                self.feed(Event::NoteAdded { expr, failed });
            }
            AnkiOutcome::Shot { expr, dir, filed } => {
                // `None` is a picture with no card behind it, so there
                // is no add lifecycle to close and nothing may claim
                // the word was filed - the other two both answer the
                // popup, which `start_add` marked adding before it
                // authorised the picture.
                let failed = match filed {
                    Ok(Some(id)) => {
                        self.log.diag(&format!(
                            "anki: card added with a screenshot as note {id} (picture in {})",
                            dir.display()
                        ));
                        Some(false)
                    }
                    Ok(None) => {
                        self.log.diag(&format!(
                            "screenshot: saved to {} - no card to file it on",
                            dir.display()
                        ));
                        None
                    }
                    Err(e) => {
                        self.log.diag(&format!("screenshot: the picture never landed - {e}"));
                        Some(true)
                    }
                };
                if let Some(failed) = failed {
                    self.feed(Event::NoteAdded { expr, failed });
                }
            }
        }
    }

    /// The Anki affordance's state moved.
    ///
    /// Windows has a button window of its own to place, hide and
    /// repaint; here the slot is part of the panel (ADR-0004), so every
    /// paint above already carries the current state and this only has
    /// to catch a state that moved after the last raster.
    fn sync_anki_slot(&mut self) {
        // Nothing shown is nothing to reconcile: retracting is
        // `HidePopup`'s job, and the request left on the surface must
        // never be re-shown from here.
        let Some(want) = self.controller.anki().cloned() else { return };
        let req = self.popup.as_ref().and_then(Popup::request);
        let Some(req) = req.filter(|req| req.anki.as_ref() != Some(&want)).cloned() else {
            return;
        };
        self.show_popup(&ShowRequest { anki: Some(want), ..req });
    }

    /// Measure, place, raster, commit — then tell the Controller where
    /// it landed. The bin owns the measurer, so this round-trip is how
    /// the Controller learns a rect it cannot compute (ADR-0004).
    fn show_popup(&mut self, req: &ShowRequest) {
        let started = Instant::now();
        let shown = match self.popup.as_mut() {
            Some(popup) => popup.show(req),
            None => Err(anyhow::anyhow!("the popup has no layer surface on this compositor")),
        };
        self.flush_popup_notes();
        match shown {
            Ok(placed) => {
                self.log.diag(&format!(
                    "popup: shown on surface {} at {},{} {}x{} at {:.3}x (view {} of {} px) in {} us",
                    placed.output,
                    placed.rect.x,
                    placed.rect.y,
                    placed.rect.w,
                    placed.rect.h,
                    placed.scale,
                    placed.view_h,
                    placed.content_h,
                    started.elapsed().as_micros(),
                ));
                self.placed(placed);
                // A same-size show rasters synchronously, so the frame
                // a scripted pass needs already exists.
                self.run_pointer_script();
            }
            Err(e) => {
                self.log.diag(&format!("popup: place failed: {e:#}"));
                self.feed(Event::PopupPlaceFailed);
            }
        }
    }

    fn placed(&mut self, placed: popup::Placed) {
        self.feed(Event::PopupPlaced {
            rect: placed.rect,
            content_h: placed.content_h,
            view_h: placed.view_h,
        });
    }

    /// Hide: a transparent buffer, never an unmap (ADR-0004), so
    /// Hyprland's layer animation never fires and this stays instant.
    ///
    /// The scan outline goes with it. A lookup's boxes belong to the
    /// popup that is showing its answer; leaving them up would outline
    /// a word nothing is defining any more. This is where Windows hides
    /// its overlay too (`app.rs`'s `Command::HidePopup`), and it also
    /// covers the one Linux-only caller, [`App::pick_region`], which
    /// must not leave a stale frame under the region selector.
    fn hide_popup(&mut self) {
        let started = Instant::now();
        let was = self.popup.as_ref().and_then(Popup::shown).is_some();
        if let Some(popup) = self.popup.as_mut() {
            popup.hide();
        }
        if let Some(outline) = self.scan_outline.as_mut() {
            outline.hide();
        }
        self.flush_popup_notes();
        self.flush_surface_notes();
        if was {
            self.log.diag(&format!("popup: hidden in {} us", started.elapsed().as_micros()));
        }
    }

    /// Outline what this hover captured and box the word it defined.
    ///
    /// `Command::ShowScanOverlay`'s Linux half: core decides *which*
    /// rects to send (`debug.show_scan_region` gives the capture boxes,
    /// `popup.highlight_match` the `Match` one, both off means an empty
    /// vector), and this decides what they look like. The colours are
    /// the popup's own theme, so the outline follows a theme change with
    /// the panel and there is no second palette to keep in step.
    fn show_scan_overlay(&mut self, rects: &[ScanRect]) {
        let screens = self.screens();
        // The outline borrows the popup's `wl_shm`, `wl_compositor`,
        // `wp_viewporter` and theme, so a session has both or neither.
        let (Some(popup), Some(outline)) = (self.popup.as_ref(), self.scan_outline.as_mut()) else {
            if !rects.is_empty() {
                self.log.diag(&format!(
                    "overlay: {} scan rect(s) and no outline on this compositor",
                    rects.len()
                ));
            }
            return;
        };
        let marks = overlay::scan_marks(rects, popup.theme());
        outline.show(&marks, &screens);
        self.flush_surface_notes();
    }

    /// The canned popup (`CHIBIPOP_POPUP_DEMO=1`). It goes through the
    /// exact path a lookup will: measure, place, commit, `PopupPlaced`.
    fn demo_show(&mut self) {
        let anchor = self.demo.anchor.or_else(|| self.cursor_anchor()).unwrap_or(DEMO_ANCHOR);
        self.log.diag(&format!(
            "popup: demo show at anchor {},{} {}x{}",
            anchor.x, anchor.y, anchor.w, anchor.h
        ));
        // The demo bypasses the Controller, so it arms the scripted
        // pointer pass itself.
        if let Some(popup) = self.popup.as_mut() {
            popup.arm_script();
        }
        self.show_popup(&ShowRequest {
            presentation: popup::canned(),
            anchor,
            scroll: 0,
            show_back: false,
            // The demo asks for the slot so the in-panel Anki
            // affordance is painted and can be inspected; production
            // asks for it only when AnkiConnect answered.
            anki: Some(chibipop::present::AnkiPopupState {
                enabled: true,
                connected: true,
                ..chibipop::present::AnkiPopupState::disabled()
            }),
        });
    }

    /// The last cursor sample as an anchor box, so a demo popup lands
    /// where a lookup would have.
    fn cursor_anchor(&mut self) -> Option<PhysRect> {
        let (lx, ly) = self.last_poll?;
        let pos = self.cursor.logical_to_global(f64::from(lx), f64::from(ly))?;
        Some(PhysRect { x: pos.x, y: pos.y, w: DEMO_ANCHOR.w, h: DEMO_ANCHOR.h })
    }

    /// One message from the tray thread, executed here on the daemon
    /// thread where the log, the settings guard and the loop signal
    /// live (ADR-0006).
    fn handle_tray(&mut self, request: TrayRequest) {
        match request {
            TrayRequest::OpenSettings => self.spawn_settings(),
            TrayRequest::Quit => {
                self.log.diag("tray: quit requested - shutting down");
                self.signal.stop();
            }
            TrayRequest::Diagnostic(line) => self.log.diag(&line),
        }
    }

    /// Record a channel transition: the registry, the tray's rows and
    /// SNI status, and one log line — only when something moved.
    ///
    /// A known software cursor rides along on the Capture row: the
    /// backend serving pixels does not stop the pointer being in them
    /// (ticket 52).
    fn note_channel(&mut self, id: ChannelId, state: ChannelState) {
        let state = match (id, &self.pointer_defect) {
            (ChannelId::Capture, Some(defect)) => state.degraded_by(defect),
            _ => state,
        };
        if self.tray.set_channel(id, state) {
            let row = self.tray.statuses().row(id);
            self.log.diag(&format!("channel: {row}"));
        }
    }

    /// Spawn the settings window unless one child already runs. The
    /// tray's Settings item calls this; the guard is daemon-side
    /// discipline, the settings-scoped flock the cross-process one.
    fn spawn_settings(&mut self) {
        let outcome = child::settings_command().and_then(|mut c| self.settings.spawn_if_absent(&mut c));
        match outcome {
            Ok(SpawnOutcome::Spawned(pid)) => self.log.diag(&format!("settings: spawned pid {pid}")),
            Ok(SpawnOutcome::AlreadyRunning(pid)) => {
                self.log.diag(&format!("settings: already running as pid {pid}"));
            }
            Err(e) => self.log.diag(&format!("settings: spawn failed: {e}")),
        }
    }

    /// `reload` re-reads the file and re-applies everything the daemon
    /// honors: the lookup-log gate, the popup's own settings -
    /// `popup.layer` needs no surface recreation, which is exactly why
    /// it is a runtime toggle - and, per ADR-0002's "denial never exits,
    /// hover shows one actionable error state with in-app retry", a
    /// second go at the portal consent. The config file is the sole
    /// source of
    /// truth (ADR-0005); nothing structured crosses the socket.
    fn reload_config(&mut self) {
        self.retry_portal_capture();
        match chibipop::config::load_or_create(&self.paths.config_file) {
            Ok(config) => {
                let was = self.log.show_lookup();
                let now = config.debug.show_lookup_log;
                self.log.set_show_lookup(now);
                self.log.diag(&format!(
                    "config: reloaded {}; lookup log {} -> {}",
                    self.paths.config_file.display(),
                    on_off(was),
                    on_off(now),
                ));
                if let Some(popup) = self.popup.as_mut() {
                    popup.reconfigure(&config);
                }
                self.flush_popup_notes();
                // The Controller answers this with `RequestReload`,
                // which is what pushes the new settings into the Worker
                // and reopens the dictionary a rebuild renamed over.
                self.config = config;
                let cfg = controller_config(&self.config);
                self.feed(Event::ConfigReloaded(Box::new(cfg)));
                // The mode, the checkbox and the region are all editable
                // in the settings window, so a reload is the second of
                // the predicate's three call sites: switching away from
                // Static is what takes the border down.
                self.sync_static_outline();
            }
            Err(e) => self.log.diag(&format!("config: reload failed: {e:#}")),
        }
    }

    /// Build (or rebuild) the core pipeline on its own thread.
    ///
    /// Dropping the old handle first ends the old thread: its trigger
    /// channel closes and it returns from `recv`. A pipeline that
    /// cannot be built is a log line and a daemon that still runs -
    /// cursor, tray, settings and the popup are all unaffected.
    fn spawn_worker(&mut self, portal: Option<PortalCapture>) {
        self.worker = None;
        // A queue with no pipeline behind it, from here until a spawn
        // succeeds: an `ocr-clipboard` press in that window is refused
        // with a reason rather than parked on a thread that no longer
        // exists. The old queue's sender goes with it, so a job the dead
        // Worker never drained cannot resurface on the new one.
        self.ocr_jobs = worker::OcrJobs::disconnected();
        let settings = worker::settings(&self.config, &self.dicts);
        // Resolved against whatever identities we already hold, which on
        // the first spawn of a session is nothing - see `rescope_lookups`.
        let sent_scope = settings.present_cfg.clone();
        let started = Instant::now();
        // A fresh queue per spawn, because the nudge that wakes the hook
        // belongs to a particular Worker's trigger channel.
        let (jobs_tx, jobs_rx) = std::sync::mpsc::channel::<worker::OcrRequest>();
        match worker::spawn(&self.worker_setup, settings, portal, self.worker_ping.clone(), jobs_rx)
        {
            Ok((worker, dicts)) => {
                self.log.diag(&format!(
                    "worker: pipeline up in {} ms; {}",
                    started.elapsed().as_millis(),
                    worker::dict_line(&self.worker_setup.db, &dicts),
                ));
                self.dicts = dicts;
                self.ocr_jobs = worker::OcrJobs::new(jobs_tx, worker.serve_nudge());
                self.worker = Some(worker);
                self.rescope_lookups(&sent_scope);
                self.look_where_the_cursor_is();
            }
            Err(e) => self.log.diag(&format!("worker: unavailable - {e:#}")),
        }
    }

    /// The "Not searched" split, once the identities are known.
    ///
    /// `Config::present_config` matches the split against the installed
    /// dictionary names, and the pipeline's own first read is where those
    /// names come from - so the settings `spawn_worker` handed over were
    /// resolved against an empty library and came out unrestricted. Push
    /// the real answer now that it can be computed: a fresh daemon must
    /// honour the setting from its first lookup, not from the first
    /// reload. Nothing to say when it did not change, which is every
    /// respawn and every config with no split.
    fn rescope_lookups(&mut self, sent: &chibipop::present::PresentConfig) {
        let settings = worker::settings(&self.config, &self.dicts);
        if settings.present_cfg == *sent {
            return;
        }
        self.log.diag(&format!(
            "worker: {} searches {} of {} dictionary/ies",
            self.config.ocr.language,
            settings.present_cfg.dict_order.len(),
            self.dicts.len(),
        ));
        self.send_trigger(TriggerKind::Reload(Box::new(settings)), RequestId(0));
    }

    /// One lookup at the cursor's present position, if it is known.
    ///
    /// The event rungs deliver a position when their session opens and
    /// then only on movement (ADR-0003), so a daemon that came up with
    /// the cursor already resting on a word has exactly one sample and
    /// no pipeline to spend it on. Asking here is what makes live mode
    /// true the moment it can be - the same reason a trigger press is
    /// its own first cursor sample (ADR-0010).
    fn look_where_the_cursor_is(&mut self) {
        let Some(pos) = self.last_cursor else { return };
        self.log.diag(&format!("lookup: asking where the cursor already is ({}, {})", pos.x, pos.y));
        self.feed(Event::CursorMoved { pos });
    }

    /// The in-app retry ADR-0002 requires: ask the portal again.
    ///
    /// Only when the ladder picked the portal rung and the backend is
    /// not already serving - a granted session must not be torn down
    /// and re-prompted just because someone edited the config file.
    /// The verb is `reload` on purpose: the tray's Settings window and
    /// a shell one-liner reach the same hook, and the trigger channel
    /// stays the minimal verb set ADR-0003 argues for.
    fn retry_portal_capture(&mut self) {
        if self.portal_serving || self.capture_selection.backend() != Some(Backend::Portal) {
            return;
        }
        let Some(retry) = self.portal_retry.take() else { return };
        self.log.diag("capture: retrying the portal consent (reload)");
        let (capture, state) = open_portal(&retry, &mut self.log);
        self.portal_retry = Some(retry);
        self.note_channel(ChannelId::Capture, state);
        // The Worker thread is what reads through the session, so a
        // granted retry hands it to a fresh pipeline.
        if capture.is_some() {
            self.portal_serving = true;
            self.spawn_worker(capture);
        }
    }

    /// One `hyprctl cursorpos` poll tick: sample, feed the seam on
    /// change, re-arm at the adaptive cadence (ADR-0010).
    ///
    /// A sample that stops answering is the one channel failure this
    /// daemon can observe live (the compositor went away, or `hyprctl`
    /// did), so it is reported to the status registry either way — this
    /// is what the tray's Cursor row and NeedsAttention track at
    /// runtime.
    fn poll_hyprctl(&mut self) -> TimeoutAction {
        match hyprctl::sample() {
            Some((lx, ly)) => {
                self.note_channel(
                    ChannelId::Cursor,
                    ChannelState::up(tray::status::rung_detail(cursor::Rung::HyprctlPoll)),
                );
                if self.last_poll != Some((lx, ly)) {
                    self.last_poll = Some((lx, ly));
                    self.last_move = Instant::now();
                    if let Some(pos) = self.cursor.logical_to_global(f64::from(lx), f64::from(ly)) {
                        self.on_cursor_position(pos);
                    }
                }
            }
            None => self.note_channel(
                ChannelId::Cursor,
                ChannelState::down("hyprctl cursorpos is not answering"),
            ),
        }
        let interval = hyprctl::next_interval(self.last_move.elapsed());
        if self.trace {
            self.log.diag(&format!("cursor: poll tick, next in {} ms", interval.as_millis()));
        }
        TimeoutAction::ToDuration(interval)
    }
}

/// The one seam (ticket 33): channel -> `Event::CursorMoved` ->
/// Controller. Both rungs land here with global physical pixels.
impl CursorHandler for App {
    fn cursor(&mut self) -> &mut CursorState {
        &mut self.cursor
    }

    fn on_cursor_position(&mut self, pos: PhysPoint) {
        if self.trace {
            self.log.diag(&format!("cursor: ({}, {})", pos.x, pos.y));
        }
        self.last_cursor = Some(pos);
        // Crossing outputs mid-hold takes one fresh full grab of the
        // entered output (ADR-0010), and it has to be taken before the
        // lookup that noticed the crossing - so it happens here, not
        // behind the Controller.
        if let Some(hold) = self.hold {
            if let Some(output) = trigger::regrab(hold, &self.cursor.geometries(), pos) {
                self.log.diag("trigger: the cursor crossed onto another output");
                self.freeze_at(pos, output);
                self.hold = Some(Hold { output, ..hold });
            }
        }
        // A sample with no pipeline behind it would spend the
        // Controller's movement gate on a lookup nobody can answer -
        // and say so once per sample. The newest position is kept
        // instead, and `look_where_the_cursor_is` spends it the moment
        // a pipeline exists.
        if self.worker.is_none() {
            return;
        }
        self.feed(Event::CursorMoved { pos });
    }
}

delegate_dispatch!(App: [WlOutput: u32] => CursorState);
delegate_dispatch!(App: [ZxdgOutputV1: u32] => CursorState);
delegate_dispatch!(App: [ZxdgOutputManagerV1: ()] => CursorState);
delegate_dispatch!(App: [WlSeat: ()] => CursorState);
delegate_dispatch!(App: [WlPointer: ()] => CursorState);
delegate_dispatch!(App: [ExtOutputImageCaptureSourceManagerV1: ()] => CursorState);
delegate_dispatch!(App: [ExtImageCaptureSourceV1: ()] => CursorState);
delegate_dispatch!(App: [ExtImageCopyCaptureManagerV1: ()] => CursorState);
delegate_dispatch!(App: [ExtImageCopyCaptureCursorSessionV1: u32] => CursorState);

/// What the Controller reads (mirrors the Windows bin's builder).
fn controller_config(config: &chibipop::config::Config) -> ControllerConfig {
    ControllerConfig {
        trigger_mode: config.trigger.mode,
        per_character_lookup: config.trigger.per_character_lookup,
        scroll_popup: config.popup.scroll_popup,
        anki_enabled: config.anki.enabled,
        first_dict_only: config.anki.first_dict_only,
        summary_chars: config.popup.summary_chars,
        log_lookups: config.debug.show_lookup_log,
        tick_ms: DISPATCH_TICK_MS,
    }
}

/// Where the static-region border belongs, when it belongs anywhere:
/// Static sentence mode, the outline switched on, and a region the user
/// has actually drawn.
///
/// The Windows bin's `LiveSettings::static_overlay_region`, and here for
/// the same reason it is one function there: startup, every config
/// reload and a fresh region all ask this, so a three-way condition
/// written out three times would eventually be three conditions. A mode
/// switched away from Static answers `None`, which is what takes the
/// border down.
fn static_overlay_region(config: &chibipop::config::Config) -> Option<PhysRect> {
    if config.anki.sentence_mode != chibipop::config::SentenceMode::Static
        || !config.anki.show_static_overlay
    {
        return None;
    }
    worker::static_region(&config.anki)
}

/// Whether the dwell re-check has anything to watch (ADR-0010), in two
/// halves.
///
/// `armed` is the Controller's: live mode, a popup with a rect and no
/// drill-down over it. `hold` is this daemon's own, because only it
/// knows about the frozen grab - a hold's pixels predate the popup and
/// cannot change, so trigger mode has no re-check by construction.
fn dwell_wanted(hold: Option<Hold>, armed: bool) -> bool {
    hold.is_none() && armed
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// Registry events on the long-lived queue. The startup report already
/// printed the full table; only later changes are worth a line.
impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        app: &mut App,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let wl_registry::Event::GlobalRemove { name } = event {
            app.log.diag(&format!("wayland: global {name} removed"));
        }
    }
}

/// `AsFd` so the socket registers straight into calloop.
struct Listening(ControlSocket);

impl AsFd for Listening {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.listener().as_fd()
    }
}

/// The AnkiConnect answer channel, registered on the pump.
///
/// One helper rather than two call sites: the tests build an `App`
/// too, and an answer that reached no `Event` would make the whole add
/// lifecycle - adding, added, failed - untestable.
fn anki_channel(pump: &LoopHandle<'static, App>) -> Result<calloop::channel::Sender<AnkiOutcome>> {
    let (tx, rx) = calloop::channel::channel::<AnkiOutcome>();
    pump.insert_source(rx, |event, _, app: &mut App| {
        if let calloop::channel::Event::Msg(outcome) = event {
            app.handle_anki(outcome);
        }
    })
    .map_err(|e| anyhow::anyhow!("registering the AnkiConnect answer channel: {e}"))?;
    Ok(tx)
}

/// The mined region's pixel channel, registered on the pump.
///
/// `anki_channel`'s twin, for the same reason: the grab happens on a
/// thread of its own (spec D6) and the tests build an `App` too, so a
/// grab that reached no `App` method would make the whole screenshot
/// flow untestable.
fn shot_channel(
    pump: &LoopHandle<'static, App>,
) -> Result<calloop::channel::Sender<Result<Frame, String>>> {
    let (tx, rx) = calloop::channel::channel::<Result<Frame, String>>();
    pump.insert_source(rx, |event, _, app: &mut App| {
        if let calloop::channel::Event::Msg(grabbed) = event {
            app.handle_shot(grabbed);
        }
    })
    .map_err(|e| anyhow::anyhow!("registering the screenshot pixel channel: {e}"))?;
    Ok(tx)
}

/// The one-off OCR job's answer channel, registered on the pump.
///
/// `shot_channel`'s twin one stage further along: the recogniser runs on
/// the Worker's thread (ADR-0009 - the engine is thread-affine), so its
/// lines arrive here as an event and never as a blocked pump.
fn ocr_text_channel(
    pump: &LoopHandle<'static, App>,
) -> Result<calloop::channel::Sender<Result<Vec<OcrLine>, String>>> {
    let (tx, rx) = calloop::channel::channel::<Result<Vec<OcrLine>, String>>();
    pump.insert_source(rx, |event, _, app: &mut App| {
        if let calloop::channel::Event::Msg(read) = event {
            app.handle_ocr_text(read);
        }
    })
    .map_err(|e| anyhow::anyhow!("registering the OCR text channel: {e}"))?;
    Ok(tx)
}

/// The clipboard thread's diagnostic channel, registered on the pump.
///
/// The offer lives on a connection and a thread of its own
/// (`clipboard`), and the log lives here (ADR-0006), so its lines travel
/// as lines - exactly as an AnkiConnect failure does.
fn clipboard_notes(pump: &LoopHandle<'static, App>) -> Result<calloop::channel::Sender<String>> {
    let (tx, rx) = calloop::channel::channel::<String>();
    pump.insert_source(rx, |event, _, app: &mut App| {
        if let calloop::channel::Event::Msg(line) = event {
            app.log.diag(&line);
        }
    })
    .map_err(|e| anyhow::anyhow!("registering the clipboard note channel: {e}"))?;
    Ok(tx)
}

/// Now, as `chibipop::shot` names files by (the Windows bin's
/// `epoch_secs`, so a screenshots folder carried between the two
/// platforms is named one way).
fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn run(paths: Paths) -> Result<()> {
    let display = wayland::display_name()?;
    let runtime_dir = paths.runtime_dir()?;

    // Lock FIRST: a second launch must exit before it can touch the
    // running daemon's logfile (Log::open truncates).
    let lock = match lock::acquire(runtime_dir, &display) {
        Ok(lock) => lock,
        Err(LockError::AlreadyRunning { path, pid }) => bail!("{}", lock::refusal(&display, &path, pid)),
        Err(LockError::Io(e)) => {
            return Err(e).with_context(|| format!("acquiring the instance lock in {}", runtime_dir.display()))
        }
    };

    let mut log = Log::open(&paths.log_file(), false);
    if let Some(path) = log.path() {
        let line = format!("log: writing {} (truncated on start)", path.display());
        log.diag(&line);
    }
    log.diag(&format!("chibipop {} starting on WAYLAND_DISPLAY={display}", env!("CARGO_PKG_VERSION")));
    log.diag(&format!("paths: {} mode", paths.mode.describe()));
    log.diag(&format!("paths: config {}", paths.config_file.display()));
    log.diag(&format!("paths: data {}", paths.data_dir.display()));
    log.diag(&format!("paths: state {}", paths.state_dir.display()));
    log.diag(&format!("paths: cache {}", paths.cache_dir.display()));
    log.diag(&format!("lock: holding {}", lock.path().display()));

    if let Some(parent) = paths.config_file.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating the config dir {}", parent.display()))?;
    }
    let config = chibipop::config::load_or_create(&paths.config_file)?;
    log.set_show_lookup(config.debug.show_lookup_log);
    log.diag(&format!(
        "config: loaded; lookup log {} (debug.show_lookup_log)",
        if config.debug.show_lookup_log { "on" } else { "off" }
    ));

    let conn = Connection::connect_to_env().context("connecting to the Wayland display")?;
    let globals = wayland::collect_globals(&conn)?;
    for line in wayland::report(&globals) {
        log.diag(&line);
    }

    // The capture backend (ADR-0002's ladder, ticket 34). Decided
    // first, because ADR-0003's rung 2 only exists when the portal
    // rung is the one serving pixels - it rides that same stream.
    let (capture_override, capture_warning) = capture_backend::BackendOverride::from_env();
    if let Some(w) = &capture_warning {
        log.diag(w);
    }
    if capture_override != capture_backend::BackendOverride::Auto {
        log.diag(&format!(
            "capture: {}={capture_override:?} override active (test hook)",
            capture_backend::BackendOverride::ENV
        ));
    }
    let capture_caps = capture_backend::Capabilities::scan(&globals, portal::available());
    let capture_selection = capture_backend::select(&capture_caps, capture_override);
    log.diag(&capture_selection.startup_line());
    // Ticket 52: neither rung's pixels can exclude a pointer the
    // compositor already painted into its own framebuffer, so say so
    // here rather than letting OCR read arrows as glyphs. Backend-
    // independent on purpose - the portal's own backend on a wlr desk
    // copies through the same framebuffer.
    let pointer_in_frames = software_cursor::probe();
    if let Some(line) = pointer_in_frames.startup_line() {
        log.diag(&line);
    }
    let pointer_defect = pointer_in_frames.row_defect();
    let portal_metadata = capture_selection.backend() == Some(Backend::Portal)
        && portal::cursor_metadata_available();

    // The cursor channel (ticket 33): one rung by advertised
    // capability (ADR-0003), or a diagnostic naming exactly what is
    // missing — and the daemon stays up either way.
    let (ladder_override, override_warning) = cursor::LadderOverride::from_env();
    if let Some(w) = &override_warning {
        log.diag(w);
    }
    if ladder_override != cursor::LadderOverride::Auto {
        log.diag(&format!(
            "cursor: {}={ladder_override:?} override active (test hook)",
            cursor::LadderOverride::ENV
        ));
    }
    let caps = cursor::Capabilities::scan(&globals, portal_metadata, hyprctl::available());
    let selection = cursor::select(&caps, ladder_override);
    log.diag(&selection.startup_line());
    let trace = std::env::var("CHIBIPOP_CURSOR_TRACE").is_ok_and(|v| v == "1");

    // Rung 2's samples arrive on PipeWire's thread and must reach the
    // pump like every other event: a bounded calloop channel, so a
    // burst of cursor metadata can never grow without limit and the
    // daemon stays sync (ADR-0001).
    let (cursor_tx, cursor_rx) = calloop::channel::sync_channel::<PhysPoint>(64);
    let cursor_sink: Option<portal::CursorSink> =
        if selection == cursor::Selection::Rung(cursor::Rung::PortalMetadata) {
            let tx = cursor_tx.clone();
            Some(std::sync::Arc::new(move |p: PhysPoint| {
                // A full queue means the pump is already behind on
                // cursor news; dropping the sample is right, blocking
                // the stream thread is not.
                let _ = tx.send(p);
            }))
        } else {
            None
        };

    // ADR-0002's eager consent: the dialog belongs in the launch
    // context, not in the middle of a hover, and the channel row has
    // to be true before the tray is ever published.
    let portal_retry = (capture_selection.backend() == Some(Backend::Portal)).then(|| PortalRetry {
        state_dir: paths.state_dir.clone(),
        globals: globals.clone(),
        cursor: cursor_sink,
    });
    let (capture, capture_state) = match &portal_retry {
        Some(retry) => open_portal(retry, &mut log),
        None => (None, tray::status::capture_state(&capture_selection)),
    };

    let socket = ControlSocket::bind(runtime_dir, &display)
        .with_context(|| format!("binding the control socket in {}", runtime_dir.display()))?;
    log.diag(&format!("control: listening on {}", socket.path().display()));

    // The trigger channel's ladder (ADR-0003, ticket 36). The socket
    // above is rung 2 and is now listening, so this decides only one
    // thing: whether the GlobalShortcuts portal is *also* asked to
    // carry the two shortcuts. Its session runs on its own thread and
    // its news arrives here as events, so the pump stays sync
    // (ADR-0001).
    let (trigger_override, trigger_warning) = shortcuts::ChannelOverride::from_env();
    if let Some(w) = &trigger_warning {
        log.diag(w);
    }
    let trigger_selection = shortcuts::select(shortcuts::portal::probe(), trigger_override);
    // The advice half of this line is a bind the user pastes, so it
    // names this binary rather than a bare `chibipop` PATH may lack
    // (ticket 51).
    log.diag(&trigger_selection.startup_line(&crate::paths::exec_name()));
    // Until the portal answers, the honest published state is "the
    // compositor owns the key": that is what the settings window must
    // show, and a stale portal binding from a previous run must not
    // outlive it.
    let published = shortcuts::state::Published::native();
    if let Err(e) = shortcuts::state::publish(&paths.state_dir, &published) {
        log.diag(&format!("trigger: could not publish the channel state - {e}"));
    }
    let (shortcut_tx, shortcut_rx) = calloop::channel::sync_channel::<shortcuts::Event>(32);
    let trigger_state = match trigger_selection {
        shortcuts::Selection::Portal => {
            match shortcuts::portal::spawn(shortcuts::preferred(&config), shortcut_tx) {
                Ok(()) => ChannelState::up(shortcuts::pending_detail()),
                Err(e) => {
                    let why = format!("no thread for the portal session: {e}");
                    log.diag(&format!("trigger: {why}"));
                    ChannelState::up(shortcuts::native_detail(&why))
                }
            }
        }
        shortcuts::Selection::Native(reason) => {
            ChannelState::up(shortcuts::native_detail(&shortcuts::native_reason(reason)))
        }
    };

    // The SNI tray (ADR-0006). It runs its own D-Bus thread and its
    // activations arrive here as `TrayRequest`s, so the pump stays sync.
    // Non-fatal by construction: `spawn` hands back diagnostics instead
    // of an error, because a trayless session is normal (stock GNOME,
    // bare Hyprland) and must cost nothing. The registry it carries is
    // the daemon's own view of channel health, tray or no tray.
    let (tray_tx, tray_rx) = calloop::channel::channel::<TrayRequest>();
    let mut statuses = ChannelStatuses::startup(
        match &pointer_defect {
            Some(defect) => capture_state.degraded_by(defect),
            None => capture_state,
        },
        &selection,
        tray::status::popup_state(wayland::popup_shell_advertised(&globals)),
    );
    // The trigger row's default is the always-bound socket; the ladder
    // above knows which rung actually owns the binding, and the tray
    // must be right the first time it is published.
    statuses.set(ChannelId::Trigger, trigger_state);
    let (mut tray_handle, tray_diagnostics) = tray::spawn(statuses, tray_tx);
    for line in tray_diagnostics {
        log.diag(&line);
    }
    for row in tray_handle.statuses().rows() {
        log.diag(&format!("channel: {row}"));
    }

    let mut event_loop: EventLoop<App> = EventLoop::try_new().context("creating the event loop")?;

    // The long-lived Wayland queue. `registry_queue_init` is what SCTK
    // needs to bind the popup's globals from; the second, hand-made
    // registry beside it is the cursor channel's, and it is also how
    // this daemon sees dynamic global changes.
    let (globals_list, mut queue) =
        registry_queue_init::<App>(&conn).context("initialising the Wayland registry")?;
    let registry = conn.display().get_registry(&queue.handle(), ());

    // The popup (ADR-0004). A compositor without the layer shell keeps
    // the daemon up: everything else - capture, cursor, trigger, tray,
    // settings - still works, the capability report already named the
    // missing global, and the Popup channel row says so where a user
    // looks. What it must NOT do is drop the popup's other Wayland
    // objects on the floor: their events keep arriving, and a handler
    // with nothing behind it is a panic (ticket 49 found exactly that
    // on a layer-shell-less session). So the popup is always built, and
    // a bind error here is the fatal kind the report already called
    // fatal.
    let mut popup = Popup::bind(&globals_list, &queue.handle(), &config)
        .context("binding the popup's Wayland globals")?;
    for line in popup.drain_notes() {
        log.diag(&line);
    }
    if !popup.available() {
        log.diag(
            "popup: unavailable - this compositor advertises no zwlr_layer_shell_v1, so the \
             hover loop cannot show anything here; every other channel keeps running",
        );
        if tray_handle.set_channel(ChannelId::Popup, tray::status::popup_state(false)) {
            log.diag(&format!("channel: {}", tray_handle.statuses().row(ChannelId::Popup)));
        }
    }

    // The three surfaces beside the popup (spec D5, ticket 03). All
    // borrow the popup's process-wide handles - one `wl_compositor`, one
    // `wl_shm`, one `wl_viewporter`, one `OutputState` - and all answer
    // `None` on the same missing global the popup already reported, so a
    // layer-shell-less session says it once and keeps every other
    // channel. Two outlines, because the scan rects and the static
    // region's border have independent lifetimes.
    let selector = Selector::bind(&conn, &globals_list, &queue.handle(), &popup);
    let scan_outline = Outline::bind(&globals_list, &queue.handle(), &popup);
    let static_outline = Outline::bind(&globals_list, &queue.handle(), &popup);
    for (what, present) in [
        ("selector", selector.is_some()),
        ("outline", scan_outline.is_some()),
        ("static outline", static_outline.is_some()),
    ] {
        if !present {
            log.diag(&format!(
                "{what}: unavailable - no zwlr_layer_shell_v1 or no shm pool on this compositor"
            ));
        }
    }
    let mut selector = selector;
    if let Some(selector) = selector.as_mut() {
        for line in selector.drain_notes() {
            log.diag(&line);
        }
    }
    let popup = Some(popup);
    let demo = Demo::from_env();
    if demo.armed {
        log.diag(&format!(
            "popup: {}=1 - trigger-down shows the canned popup, trigger-up hides it{}",
            Demo::ENV,
            match demo.anchor {
                Some(a) => format!(" (anchor {},{} {}x{})", a.x, a.y, a.w, a.h),
                None => String::new(),
            }
        ));
    }

    // The Worker's wake: a result queued on its thread becomes one
    // event-loop wakeup here (ADR-0001 - the pump stays sync).
    let (worker_ping, worker_pings) =
        calloop::ping::make_ping().context("creating the worker wake")?;

    // AnkiConnect's answers, from the threads that made the calls; the
    // mined region's pixels, from the thread that grabbed them; and the
    // one-off OCR job's lines, from the Worker's own thread.
    let anki_tx = anki_channel(&event_loop.handle())?;
    let shot_tx = shot_channel(&event_loop.handle())?;
    let ocr_tx = ocr_text_channel(&event_loop.handle())?;

    // The writable selection (spec D2), on its own connection and its
    // own thread. A compositor with no data-control protocol - stock
    // GNOME - is a state named once here, exactly like a missing layer
    // shell above: it costs `ocr-clipboard` and nothing else, and
    // naming both globals is what lets a compositor upgrade self-heal
    // the install (ADR-0002's rule). A *failure* to open one is also
    // not fatal: the daemon says so and keeps every other channel.
    let clipboard = match clipboard::Clipboard::bind(&globals, clipboard_notes(&event_loop.handle())?)
    {
        Ok(Some(board)) => {
            log.diag(&format!(
                "clipboard: {} bound on its own connection - `ocr-clipboard` can copy here",
                board.rung().global()
            ));
            Some(board)
        }
        Ok(None) => {
            log.diag(&clipboard::unavailable_line());
            None
        }
        Err(e) => {
            log.diag(&format!("clipboard: unavailable - {e:#}"));
            None
        }
    };

    let mut app = App {
        log,
        stub: StubState::default(),
        paths: paths.clone(),
        signal: event_loop.get_signal(),
        pump: event_loop.handle(),
        dwell: None,
        cursor: CursorState::default(),
        controller: Controller::new(controller_config(&config)),
        trace,
        last_poll: None,
        last_cursor: None,
        cursor_rung: match &selection {
            cursor::Selection::Rung(rung) => Some(*rung),
            cursor::Selection::Unsupported { .. } => None,
        },
        last_move: Instant::now(),
        settings: SettingsChild::new(),
        tray: tray_handle,
        worker: None,
        worker_setup: worker::Setup {
            globals: globals.clone(),
            backend: capture_selection.backend(),
            db: paths.data_dir.join("chibipop.sqlite"),
        },
        worker_ping,
        anki_tx,
        shot_tx,
        ocr_jobs: worker::OcrJobs::disconnected(),
        ocr_tx,
        ocr_job: None,
        clipboard,
        shot: None,
        dicts: Vec::new(),
        hold: None,
        last_warning: None,
        portal_serving: capture.is_some(),
        capture_selection,
        pointer_defect,
        portal_retry,
        popup,
        selector,
        pick: None,
        scan_outline,
        static_outline,
        demo,
        scripting: false,
        config,
    };

    // Bind what the selected rung needs and settle it before the pump
    // starts: output geometry (both rungs), and for rung 1 the seat's
    // pointer plus the per-output cursor sessions, created inside
    // these dispatches.
    let qh = queue.handle();
    match &selection {
        cursor::Selection::Rung(cursor::Rung::ImageCopyCapture) => {
            app.cursor.bind_outputs(&registry, &globals, &qh);
            app.cursor.bind_capture(&registry, &globals, &qh);
        }
        // Rung 2 needs the same layout facts and nothing Wayland-side
        // beyond them: its samples come off the PipeWire stream the
        // portal backend already opened.
        cursor::Selection::Rung(cursor::Rung::PortalMetadata) => {
            app.cursor.bind_outputs(&registry, &globals, &qh);
        }
        cursor::Selection::Rung(cursor::Rung::HyprctlPoll) => {
            app.cursor.bind_outputs(&registry, &globals, &qh);
        }
        cursor::Selection::Unsupported { .. } => {}
    }
    if !matches!(selection, cursor::Selection::Unsupported { .. }) {
        queue.roundtrip(&mut app).context("settling the cursor channel")?;
        queue.roundtrip(&mut app).context("settling the cursor channel")?;
    }
    if matches!(selection, cursor::Selection::Rung(cursor::Rung::ImageCopyCapture)) {
        app.log
            .diag(&format!("cursor: {} output cursor session(s) live", app.cursor.session_count()));
    }

    // The popup's surfaces: one per output, mapped now and never
    // unmapped (ADR-0004). The output roundtrip above has already run,
    // so every surface is created against known geometry; the second
    // roundtrip lets the initial configures arrive and each surface map
    // itself hidden before the pump starts.
    if app.popup_can_draw() {
        app.popup_mut().map_all();
        app.flush_popup_notes();
        queue.roundtrip(&mut app).context("mapping the popup's layer surfaces")?;
        app.flush_popup_notes();
        let mapped = app.popup_mut().surface_count();
        app.log.diag(&format!("popup: {mapped} layer surface(s) mapped hidden"));
    }

    // The static region's border, if this config wants one: the first of
    // the predicate's three call sites, so a daemon started in Static
    // mode comes up with the box already outlined (the Windows bin does
    // the same at startup). The round trip is what carries the outline's
    // configures back and puts the strips on screen - without it the
    // border would appear only when calloop next woke.
    app.sync_static_outline();
    queue.roundtrip(&mut app).context("mapping the static region's outline")?;
    app.flush_surface_notes();

    // The surface probe, if it was asked for: here, because the outline
    // and the selector are mapped against the geometry the popup's
    // surfaces were just mapped against, and because this is the last
    // point at which the daemon's own queue can still be round-tripped
    // by hand - a moment later it belongs to calloop.
    if std::env::var(SURFACE_PROBE_ENV).is_ok_and(|v| v == "1") {
        app.probe_surfaces(&mut queue);
    }

    WaylandSource::new(conn.clone(), queue)
        .insert(event_loop.handle())
        .context("registering the Wayland source")?;

    // Rung 3 is the only timed source; event rungs cost zero idle
    // wakeups (ADR-0010).
    if matches!(selection, cursor::Selection::Rung(cursor::Rung::HyprctlPoll)) {
        event_loop
            .handle()
            .insert_source(Timer::from_duration(budget::POLL_ACTIVE), |_, _, app: &mut App| {
                app.poll_hyprctl()
            })
            .map_err(|e| anyhow::anyhow!("registering the cursor poll timer: {e}"))?;
    }

    event_loop
        .handle()
        .insert_source(Generic::new(Listening(socket), Interest::READ, Mode::Level), |_, listening, app: &mut App| {
            for (request, verb) in listening.0.drain() {
                app.handle_request(&request, verb);
            }
            Ok(PostAction::Continue)
        })
        .map_err(|e| anyhow::anyhow!("registering the control socket: {e}"))?;

    // Rung 2's samples, already in global physical pixels, arriving
    // from the portal stream's thread.
    if selection == cursor::Selection::Rung(cursor::Rung::PortalMetadata) {
        event_loop
            .handle()
            .insert_source(cursor_rx, |event, _, app: &mut App| {
                if let calloop::channel::Event::Msg(pos) = event {
                    app.note_channel(
                        ChannelId::Cursor,
                        ChannelState::up(tray::status::rung_detail(cursor::Rung::PortalMetadata)),
                    );
                    app.on_cursor_position(pos);
                }
            })
            .map_err(|e| anyhow::anyhow!("registering the portal cursor channel: {e}"))?;
    } else {
        // Nothing will ever send; dropping the receiver keeps a stray
        // sample a cheap error instead of an unbounded queue.
        drop(cursor_rx);
    }

    // The portal session's news: the bound set, every press and
    // release, and its own diagnostics (ticket 36). Registered
    // whichever rung was picked, because the receiver has to outlive
    // the sender either way: on the native rung nothing was spawned to
    // send, and an idle channel costs no wakeups.
    event_loop
        .handle()
        .insert_source(shortcut_rx, |event, _, app: &mut App| {
            if let calloop::channel::Event::Msg(event) = event {
                app.handle_shortcut(event);
            }
        })
        .map_err(|e| anyhow::anyhow!("registering the shortcut channel: {e}"))?;

    // Menu activations and the tray thread's own diagnostics, executed
    // on this thread. `Event::Closed` needs no handling: the tray
    // thread going away is exactly the trayless case, which is fine.
    event_loop
        .handle()
        .insert_source(tray_rx, |event, _, app: &mut App| {
            if let calloop::channel::Event::Msg(request) = event {
                app.handle_tray(request);
            }
        })
        .map_err(|e| anyhow::anyhow!("registering the tray channel: {e}"))?;

    event_loop
        .handle()
        .insert_source(
            Signals::new(&[Signal::SIGINT, Signal::SIGTERM]).context("registering signal handling")?,
            |event, _, app: &mut App| {
                app.log.diag(&format!("signal: {:?} - shutting down", event.signal()));
                app.signal.stop();
            },
        )
        .map_err(|e| anyhow::anyhow!("registering the signal source: {e}"))?;

    // The Worker's results, drained on its wake.
    event_loop
        .handle()
        .insert_source(worker_pings, |_, _, app: &mut App| app.drain_results())
        .map_err(|e| anyhow::anyhow!("registering the worker wake: {e}"))?;

    // The pipeline itself, last: opening the OCR models and the
    // dictionary blocks, and everything above must already be true
    // before a lookup can be asked for. On the portal rung the
    // consented session moves onto the worker thread here.
    app.spawn_worker(capture);

    app.log.diag(&format!(
        "ready: pump running (cursor channel wired; popup {}; capture {}; tray {}; lookups {})",
        match app.popup.as_ref().filter(|p| p.available()).map(Popup::surface_count) {
            Some(n) => format!("on {n} output(s)"),
            None => "unavailable (no layer shell)".to_string(),
        },
        match app.capture_selection.backend() {
            Some(Backend::WlrScreencopy) => "wlr-screencopy",
            Some(Backend::Portal) => "portal",
            None => "unsupported",
        },
        if app.tray.is_connected() { "published" } else { "trayless" },
        if app.worker.is_some() { "ready" } else { "unavailable" },
    ));

    event_loop.run(None, &mut app, |_| {}).context("running the event loop")?;

    // Dropping the loop drops the control source, which unlinks the
    // socket file; the lock file stays (see lock.rs) and the kernel
    // releases the flock when `lock` drops.
    drop(event_loop);
    app.log.diag("shutdown: control socket unlinked, instance lock released");
    drop(lock);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcuts::ShortcutId;

    /// The concrete hot-reload contract: `reload` re-reads the file,
    /// so the lookup-log gate follows the config without a restart.
    #[test]
    fn reload_rereads_the_config_and_flips_the_lookup_gate() {
        let dir = std::env::temp_dir()
            .join(format!("chibipop_daemon_reload_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config_file = dir.join("chibipop.toml");
        let mut cfg = chibipop::config::load_or_create(&config_file).unwrap();
        assert!(!cfg.debug.show_lookup_log, "the default must start off");

        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);

        cfg.debug.show_lookup_log = true;
        cfg.save(&config_file).unwrap();
        app.handle_request("reload", Some(Verb::Reload));
        assert!(app.log.show_lookup(), "reload must re-read the file");

        cfg.debug.show_lookup_log = false;
        cfg.save(&config_file).unwrap();
        app.handle_request("reload", Some(Verb::Reload));
        assert!(!app.log.show_lookup(), "and follow it back down");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The tray thread owns no log, so its diagnostics travel as
    /// requests and are written here. A trayless run relies on this
    /// path for its one "no tray host" line.
    #[test]
    fn a_tray_diagnostic_reaches_the_daemon_log() {
        let dir =
            std::env::temp_dir().join(format!("chibipop_daemon_traydiag_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log_file = dir.join("chibipop.log");

        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        app.handle_tray(TrayRequest::Diagnostic("tray: no StatusNotifier host".to_string()));

        let written = std::fs::read_to_string(&log_file).unwrap();
        assert!(written.contains("tray: no StatusNotifier host"), "log was: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Quit stops the pump, through the same calloop channel the tray
    /// thread uses. `run` resets and then watches the loop signal, so
    /// "the pump made no further pass" is the observable contract; the
    /// escape hatch keeps a regression a failure instead of a hang.
    #[test]
    fn tray_quit_stops_the_pump() {
        let dir =
            std::env::temp_dir().join(format!("chibipop_daemon_trayquit_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);
        let (tray_tx, tray_rx) = calloop::channel::channel::<TrayRequest>();
        event_loop
            .handle()
            .insert_source(tray_rx, |event, _, app: &mut App| {
                if let calloop::channel::Event::Msg(request) = event {
                    app.handle_tray(request);
                }
            })
            .unwrap();
        tray_tx.send(TrayRequest::Quit).unwrap();

        let escape = event_loop.get_signal();
        let mut passes = 0;
        event_loop
            .run(Some(std::time::Duration::from_millis(20)), &mut app, |_| {
                passes += 1;
                if passes >= 4 {
                    escape.stop();
                }
            })
            .unwrap();
        assert_eq!(1, passes, "Quit must stop the pump on the pass that delivered it");

        let written = std::fs::read_to_string(dir.join("chibipop.log")).unwrap();
        assert!(written.contains("tray: quit requested"), "log was: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A channel transition is logged once and only once, so a failing
    /// poll cannot flood the log at the poll cadence.
    #[test]
    fn a_channel_transition_is_logged_once() {
        let dir =
            std::env::temp_dir().join(format!("chibipop_daemon_channel_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log_file = dir.join("chibipop.log");

        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        for _ in 0..3 {
            app.note_channel(ChannelId::Cursor, ChannelState::down("hyprctl is gone"));
        }

        let written = std::fs::read_to_string(&log_file).unwrap();
        assert_eq!(1, written.matches("channel: Cursor: hyprctl is gone").count(), "log was: {written}");
        assert_eq!(ksni::Status::NeedsAttention, app.tray.statuses().sni_status());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A screencopy session with no pipeline, which is what every test
    /// here wants: the promptless rung, so no test can wander into a
    /// portal dialog, and no OCR models are opened for a log assertion.
    fn test_app(
        dir: &std::path::Path,
        log_file: &std::path::Path,
        // The pump's own lifetime, because `App` holds its handle.
        event_loop: &EventLoop<'static, App>,
    ) -> App {
        let capture = capture_backend::Selection::Backend(Backend::WlrScreencopy);
        let (worker_ping, _pings) = calloop::ping::make_ping().unwrap();
        App {
            log: Log::open(log_file, false),
            stub: StubState::default(),
            // Every directory under the scratch dir, in the XDG shape:
            // a relative `save_dir` resolves under `data_dir`, so a
            // screenshot test's PNG lands inside its own scratch.
            paths: Paths {
                mode: crate::paths::Mode::Xdg,
                config_file: dir.join("chibipop.toml"),
                data_dir: dir.to_path_buf(),
                state_dir: dir.to_path_buf(),
                cache_dir: dir.to_path_buf(),
                runtime_dir: Some(dir.to_path_buf()),
            },
            config: chibipop::config::Config::default(),
            signal: event_loop.get_signal(),
            pump: event_loop.handle(),
            dwell: None,
            settings: SettingsChild::new(),
            cursor: CursorState::default(),
            controller: Controller::new(controller_config(&chibipop::config::Config::default())),
            trace: false,
            last_poll: None,
            last_cursor: None,
            cursor_rung: Some(cursor::Rung::HyprctlPoll),
            last_move: Instant::now(),
            tray: TrayHandle::trayless(ChannelStatuses::startup(
                tray::status::capture_state(&capture),
                &cursor::Selection::Rung(cursor::Rung::HyprctlPoll),
                tray::status::popup_state(true),
            )),
            worker: None,
            worker_setup: worker::Setup {
                globals: Vec::new(),
                backend: Some(Backend::WlrScreencopy),
                db: dir.join("chibipop.sqlite"),
            },
            worker_ping,
            anki_tx: anki_channel(&event_loop.handle()).expect("the anki answer channel"),
            shot_tx: shot_channel(&event_loop.handle()).expect("the screenshot pixel channel"),
            shot: None,
            // No pipeline, so no engine to serve a job: a test that
            // wants one installs it (`fake_worker`).
            ocr_jobs: worker::OcrJobs::disconnected(),
            ocr_tx: ocr_text_channel(&event_loop.handle()).expect("the OCR text channel"),
            ocr_job: None,
            // No compositor here, so no data-control connection either -
            // which is also the GNOME state these tests can therefore
            // assert without one.
            clipboard: None,
            dicts: Vec::new(),
            hold: None,
            last_warning: None,
            portal_serving: false,
            capture_selection: capture,
            // Ticket 52's probe is a startup fact; the harness asserts
            // the folding, not the compositor's option.
            pointer_defect: None,
            portal_retry: None,
            popup: None,
            selector: None,
            pick: None,
            scan_outline: None,
            static_outline: None,
            demo: Demo::default(),
            scripting: false,
        }
    }

    /// The retry hook is portal-only and one-shot-guarded: `reload` on
    /// a screencopy session must never reach the portal, and must never
    /// touch the capture row (ADR-0002 - the promptless rung is exactly
    /// the one that has nothing to ask for).
    #[test]
    fn reload_does_not_prompt_a_screencopy_session() {
        let dir =
            std::env::temp_dir().join(format!("chibipop_daemon_noretry_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log_file = dir.join("chibipop.log");

        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let before = app.tray.statuses().row(ChannelId::Capture);
        app.handle_request("reload", Some(Verb::Reload));

        let written = std::fs::read_to_string(&log_file).unwrap();
        assert!(!written.contains("retrying the portal consent"), "log was: {written}");
        assert_eq!(before, app.tray.statuses().row(ChannelId::Capture));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The static-region action, all the way through: the rect lands in
    /// the config *file* (ADR-0005 - that file is the sole source of
    /// truth, so a restarted daemon has to find the box there) and the
    /// same rect reaches the pipeline, proved the one way that cannot
    /// lie: the next hover reads the user's box instead of a
    /// cursor-centred tile.
    ///
    /// The pick is stubbed at its seam. `took_static_region` takes the
    /// `Option<PhysRect>` a pick answers with, and that is the whole
    /// input to everything interesting here; `pick_region` above it is a
    /// nested Wayland pump, and there is no compositor in a unit test.
    #[test]
    fn the_static_region_verb_saves_the_rect_and_the_pipeline_reads_it() {
        let dir = scratch("static_region_set");
        let log_file = dir.join("chibipop.log");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let (worker, _log) = fake_worker(None, None);
        app.worker = Some(worker);
        // The user already picked Static in the settings window; the box
        // is what is still missing.
        app.config.anki.sentence_mode = chibipop::config::SentenceMode::Static;

        // Around `AT` (600,300) but nothing like the tile a live lookup
        // would otherwise read, so the anchor below tells the two apart.
        let region = PhysRect { x: 500, y: 250, w: 200, h: 100 };
        app.took_static_region(Some(region));

        let saved = chibipop::config::load_or_create(&app.paths.config_file).unwrap();
        assert_eq!(
            Some([500, 250, 200, 100]),
            saved.anki.static_region,
            "the rect has to survive a restart, so it goes in the file"
        );
        let written = std::fs::read_to_string(&log_file).unwrap();
        assert!(written.contains("static region: set to 200x100 at 500,250"), "log was: {written}");

        // The reload the save pushed rode the same channel the hover
        // now takes, and the worker settles settings before hovers - so
        // this read is through the region or the region never arrived.
        app.on_cursor_position(AT);
        match answer(&app).outcome {
            chibipop::controller::LookupOutcome::Ready { anchor, matched, scan, .. } => {
                assert_eq!(region, anchor, "the pipeline read the drawn box, not a tile");
                // `popup.highlight_match` is on by default, so line mode
                // would have put a `ScanKind::Match` rect in `scan`. The
                // static path passes `outline_match: false` and draws
                // nothing, which is the other half of the fingerprint.
                assert!(scan.is_empty(), "the static path draws no boxes");
                let hit = matched.expect("the matched word still has a rect");
                assert!(
                    hit.x < region.x + region.w && hit.y < region.y + region.h,
                    "the match came from inside the drawn box: {hit:?}"
                );
            }
            other => panic!("the lookup must resolve through the region: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A cancelled pick changes nothing at all: no file written, no
    /// region in memory, no reload pushed. `Esc`, a right-click, a drag
    /// under the threshold and a compositor with no layer shell are all
    /// the same `None`, and this enters through the socket verb so the
    /// whole path - `handle_request`, `apply_verb`, `pick_region` with
    /// nothing to drag on - is what produces it.
    #[test]
    fn a_cancelled_static_region_pick_writes_nothing() {
        let dir = scratch("static_region_cancel");
        let log_file = dir.join("chibipop.log");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        assert!(!app.paths.config_file.exists(), "the fixture starts with no config file");

        app.handle_request("static-region", Verb::parse("static-region"));

        let written = std::fs::read_to_string(&log_file).unwrap();
        assert!(
            written.contains("control: static-region - picking the static sentence region"),
            "log was: {written}"
        );
        assert!(
            written.contains("static region: pick cancelled - nothing changed"),
            "log was: {written}"
        );
        assert!(!written.contains("static region: set to"), "log was: {written}");
        assert_eq!(None, app.config.anki.static_region, "nothing in memory either");
        assert!(!app.paths.config_file.exists(), "and nothing was written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The one predicate every show and hide asks (the Windows bin's
    /// `LiveSettings::static_overlay_region`). All three conditions have
    /// to hold at once, and the mode is the one a user flips: switching
    /// away from Static is what takes the border down.
    #[test]
    fn the_outline_wants_a_drawn_region_in_static_mode_with_the_box_ticked() {
        use chibipop::config::SentenceMode;
        let mut cfg = chibipop::config::Config::default();
        let rect = PhysRect { x: 10, y: 20, w: 300, h: 40 };
        cfg.anki.static_region = Some([10, 20, 300, 40]);

        // Mode x checkbox, all four, with a region drawn throughout.
        cfg.anki.sentence_mode = SentenceMode::Static;
        cfg.anki.show_static_overlay = true;
        assert_eq!(Some(rect), static_overlay_region(&cfg), "static, ticked, drawn");

        cfg.anki.show_static_overlay = false;
        assert_eq!(None, static_overlay_region(&cfg), "static, unticked");

        cfg.anki.sentence_mode = SentenceMode::Line;
        assert_eq!(None, static_overlay_region(&cfg), "not static, unticked");

        cfg.anki.show_static_overlay = true;
        assert_eq!(None, static_overlay_region(&cfg), "ticked, but the mode left Static");

        // The third axis on its own: both switches on, nothing drawn.
        cfg.anki.sentence_mode = SentenceMode::Static;
        cfg.anki.static_region = None;
        assert_eq!(None, static_overlay_region(&cfg), "no box has been drawn yet");
    }

    /// A session with no layer shell: the region still serves lookups,
    /// the border cannot be drawn, and the daemon says which is which
    /// instead of pretending. Also the proof that a reload is one of the
    /// predicate's three call sites - the line exists only because the
    /// reload re-asked - and that a mode moved away from Static stops
    /// wanting one.
    #[test]
    fn a_static_region_with_no_layer_shell_says_so_and_still_serves_lookups() {
        let dir = scratch("static_region_noshell");
        let log_file = dir.join("chibipop.log");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let file = app.paths.config_file.clone();

        let mut cfg = chibipop::config::load_or_create(&file).unwrap();
        cfg.anki.sentence_mode = chibipop::config::SentenceMode::Static;
        cfg.anki.static_region = Some([10, 20, 300, 40]);
        cfg.save(&file).unwrap();
        app.handle_request("reload", Some(Verb::Reload));

        let written = std::fs::read_to_string(&log_file).unwrap();
        assert_eq!(
            1,
            written.matches("static region: no outline on this compositor").count(),
            "log was: {written}"
        );
        assert_eq!(
            Some(PhysRect { x: 10, y: 20, w: 300, h: 40 }),
            worker::settings(&app.config, &app.dicts).static_region,
            "an undrawable border must not cost the lookups"
        );

        cfg.anki.sentence_mode = chibipop::config::SentenceMode::Line;
        cfg.save(&file).unwrap();
        app.handle_request("reload", Some(Verb::Reload));

        let written = std::fs::read_to_string(&log_file).unwrap();
        assert_eq!(
            1,
            written.matches("static region: no outline on this compositor").count(),
            "a mode away from Static wants no border, so there is nothing new to say; \
             log was: {written}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The other half of the same guard, and the one that protects a
    /// user who already said yes: a portal session that IS serving must
    /// not be torn down and re-prompted just because something sent
    /// `reload` (a settings Apply does, on every save). One consent per
    /// grant is ADR-0002's whole bargain.
    #[test]
    fn reload_does_not_reprompt_a_serving_portal_session() {
        let dir = scratch("noreprompt");
        let log_file = dir.join("chibipop.log");

        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        app.capture_selection = capture_backend::Selection::Backend(Backend::Portal);
        app.portal_serving = true;
        // A retry is armed, so nothing but `portal_serving` can be what
        // holds the dialog back.
        app.portal_retry =
            Some(PortalRetry { state_dir: dir.clone(), globals: Vec::new(), cursor: None });
        app.note_channel(ChannelId::Capture, ChannelState::up("portal ScreenCast + PipeWire"));

        app.handle_request("reload", Some(Verb::Reload));

        let written = std::fs::read_to_string(&log_file).unwrap();
        assert!(!written.contains("retrying the portal consent"), "log was: {written}");
        assert_eq!(
            "Capture: portal ScreenCast + PipeWire",
            app.tray.statuses().row(ChannelId::Capture)
        );
        assert!(app.portal_retry.is_some(), "the retry hook must stay armed for a later denial");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A scratch dir per test, named after it.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("chibipop_daemon_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ---- ocr-clipboard ----

    /// Stock GNOME, reached through the socket exactly as a compositor
    /// bind reaches it: the refusal happens *before* any pick, names both
    /// globals so a compositor upgrade self-heals the install, and leaves
    /// nothing in flight. `test_app` has no compositor and therefore no
    /// data-control connection, which is the same state.
    #[test]
    fn ocr_to_clipboard_with_no_data_control_protocol_refuses_before_it_picks() {
        let dir = scratch("ocrclip_none");
        let log_file = dir.join("chibipop.log");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        assert!(app.clipboard.is_none(), "no compositor, so no selection to own");

        app.handle_request("ocr-clipboard", Verb::parse("ocr-clipboard"));

        let written = std::fs::read_to_string(&log_file).unwrap();
        assert!(
            written.contains("control: ocr-clipboard - picking a region to OCR onto the clipboard"),
            "the socket must answer for the verb it took: {written}"
        );
        assert!(written.contains(clipboard::EXT_MANAGER), "log was: {written}");
        assert!(written.contains(clipboard::WLR_MANAGER), "log was: {written}");
        assert!(
            !written.contains("select: pick took"),
            "a session that cannot copy must not ask for a region: {written}"
        );
        assert_eq!(None, app.ocr_job, "and nothing is left waiting");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A cancelled pick - Esc, a right click, a sub-threshold drag, or a
    /// compositor with no layer shell to drag on - copies nothing and
    /// queues nothing.
    #[test]
    fn a_cancelled_ocr_pick_copies_nothing_and_queues_nothing() {
        let dir = scratch("ocrclip_cancel");
        let log_file = dir.join("chibipop.log");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);

        app.took_ocr_region(None);

        let written = std::fs::read_to_string(&log_file).unwrap();
        assert!(
            written.contains("ocr-clipboard: pick cancelled - the clipboard is untouched"),
            "log was: {written}"
        );
        assert_eq!(None, app.ocr_job);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A region the recogniser found nothing in must not *clear* the
    /// clipboard: whatever the user already had is theirs, and an empty
    /// selection would be this feature destroying data on a bad drag.
    /// The in-flight slot is released either way, so the next press works.
    #[test]
    fn a_region_with_no_readable_text_leaves_the_selection_alone() {
        let dir = scratch("ocrclip_empty");
        let log_file = dir.join("chibipop.log");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        app.ocr_job = Some(PhysRect { x: 10, y: 20, w: 300, h: 40 });

        app.handle_ocr_text(Ok(vec![OcrLine { words: Vec::new() }]));

        let written = std::fs::read_to_string(&log_file).unwrap();
        assert!(
            written.contains("ocr-clipboard: nothing readable in the 300x40 region"),
            "log was: {written}"
        );
        assert_eq!(None, app.ocr_job, "the next press must not be refused");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A grab or a read that failed is reported rather than swallowed,
    /// and it releases the in-flight region too.
    #[test]
    fn a_failed_read_is_reported_and_releases_the_in_flight_region() {
        let dir = scratch("ocrclip_failed");
        let log_file = dir.join("chibipop.log");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        app.ocr_job = Some(PhysRect { x: 0, y: 0, w: 64, h: 48 });

        app.handle_ocr_text(Err("grabbing the region failed - no backend".to_string()));

        let written = std::fs::read_to_string(&log_file).unwrap();
        assert!(
            written.contains("ocr-clipboard: grabbing the region failed - no backend"),
            "log was: {written}"
        );
        assert_eq!(None, app.ocr_job);

        // And text arriving with nothing waiting for it says so instead
        // of copying a stale answer onto the clipboard.
        app.handle_ocr_text(Ok(vec![OcrLine {
            words: vec![OcrWord {
                text: WORD.to_string(),
                rect: PhysRect { x: 0, y: 0, w: 1, h: 1 },
            }],
        }]));
        let written = std::fs::read_to_string(&log_file).unwrap();
        assert!(
            written.contains("ocr-clipboard: text arrived with no region waiting for it"),
            "log was: {written}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The verbs' effect on the hold. There is no pipeline here, which
    /// is exactly the point: the hold is the daemon's own state and the
    /// verb table must be right whether or not a lookup can run.
    #[test]
    fn a_press_holds_and_a_release_ends_it() {
        let dir = scratch("hold");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);
        // An event rung, so no test ever shells out to hyprctl.
        app.cursor_rung = Some(cursor::Rung::ImageCopyCapture);
        app.last_cursor = Some(PhysPoint { x: 400, y: 300 });

        app.handle_request("trigger-down", Some(Verb::TriggerDown));
        let hold = app.hold.expect("a press holds");
        assert!(!hold.latched, "a key press is not a latch");
        assert!(hold.output.contains(PhysPoint { x: 400, y: 300 }), "{:?}", hold.output);

        app.handle_request("trigger-up", Some(Verb::TriggerUp));
        assert_eq!(None, app.hold, "a release ends the hold");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `toggle` outlives the key: a release while latched changes
    /// nothing, and only a second toggle ends it (ADR-0010).
    #[test]
    fn a_toggle_holds_the_freeze_until_it_is_toggled_off() {
        let dir = scratch("toggle");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);
        app.cursor_rung = Some(cursor::Rung::ImageCopyCapture);
        app.last_cursor = Some(PhysPoint { x: 400, y: 300 });

        app.handle_request("toggle", Some(Verb::Toggle));
        assert!(app.hold.is_some_and(|h| h.latched), "toggle-on latches");
        app.handle_request("trigger-up", Some(Verb::TriggerUp));
        assert!(app.hold.is_some(), "a stray release must not end a toggle");
        app.handle_request("toggle", Some(Verb::Toggle));
        assert_eq!(None, app.hold, "toggle-off ends it");

        let written = std::fs::read_to_string(dir.join("chibipop.log")).unwrap();
        assert!(written.contains("a toggle holds the freeze"), "log was: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Trigger mode reads where the cursor is, so a press before the
    /// cursor channel has said anything is a line, not a lookup.
    #[test]
    fn a_press_before_the_first_cursor_sample_looks_nothing_up() {
        let dir = scratch("nocursor");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);
        app.cursor_rung = Some(cursor::Rung::ImageCopyCapture);

        app.handle_request("trigger-down", Some(Verb::TriggerDown));

        assert_eq!(None, app.hold, "nothing to freeze on");
        let written = std::fs::read_to_string(dir.join("chibipop.log")).unwrap();
        assert!(written.contains("no cursor sample yet"), "log was: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Crossing outputs mid-hold re-grabs, and the hold follows the
    /// cursor onto the output it entered (ADR-0010). This box has one
    /// monitor, so the geometry is injected; `trigger::regrab` carries
    /// the decision and this pins that the daemon acts on it.
    #[test]
    fn a_hold_follows_the_cursor_onto_another_output() {
        let dir = scratch("crossing");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);
        let left = PhysRect { x: 0, y: 0, w: 1920, h: 1080 };
        app.hold = Some(Hold { output: left, latched: false });

        // No output geometry has arrived, so `bounds_containing`
        // answers with a plausible box around the point - which is a
        // different box than the one held, i.e. a crossing.
        app.on_cursor_position(PhysPoint { x: 5000, y: 500 });

        let now = app.hold.expect("the hold survives the crossing");
        assert_ne!(left, now.output, "the hold must move to the entered output");
        assert!(now.output.contains(PhysPoint { x: 5000, y: 500 }));
        let written = std::fs::read_to_string(dir.join("chibipop.log")).unwrap();
        assert!(written.contains("crossed onto another output"), "log was: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One failing lookup must not become one log line per cursor
    /// sample: a cursor crossing text no dictionary can serve would
    /// otherwise flood the file at the sample rate.
    #[test]
    fn a_repeated_lookup_failure_is_logged_once() {
        let dir = scratch("warnonce");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);
        for _ in 0..3 {
            app.execute(Command::WarnLookupFailed("no dictionary".to_string()));
        }
        app.execute(Command::WarnLookupFailed("something else".to_string()));

        let written = std::fs::read_to_string(dir.join("chibipop.log")).unwrap();
        assert_eq!(1, written.matches("lookup failed: no dictionary").count(), "log: {written}");
        assert_eq!(1, written.matches("lookup failed: something else").count(), "log: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- the scan overlay (`Command::ShowScanOverlay`) --

    /// One hover's worth of scan rects, in the order core sends them:
    /// the pass-1 capture box first, the word being defined last.
    fn one_hovers_rects() -> Vec<ScanRect> {
        vec![
            ScanRect { rect: PhysRect { x: 100, y: 200, w: 240, h: 60 }, kind: ScanKind::Pass1 },
            ScanRect { rect: PhysRect { x: 140, y: 210, w: 40, h: 40 }, kind: ScanKind::Match },
        ]
    }

    /// Two shipped Linux settings are drawn by this Command and by
    /// nothing else - `debug.show_scan_region` and, by default *on*,
    /// `popup.highlight_match`. Until it was wired it fell into
    /// `execute`'s catch-all, so both checkboxes drew nothing at all;
    /// the catch-all is the regression to keep out.
    #[test]
    fn a_scan_overlay_command_never_falls_into_the_no_op_arm_again() {
        let dir = scratch("scanoverlay");
        let log_file = dir.join("chibipop.log");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);

        app.execute(Command::ShowScanOverlay { rects: one_hovers_rects() });

        let written = std::fs::read_to_string(&log_file).unwrap();
        assert!(
            !written.contains("ShowScanOverlay"),
            "the command must be handled, not described as a no-op: {written}"
        );
        // No compositor in a unit test, so the honest answer is the
        // degradation, with the count - never silence, and never a
        // pretence that something was drawn.
        assert!(
            written.contains("overlay: 2 scan rect(s) and no outline on this compositor"),
            "log was: {written}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The empty vector is the hide - core sends one on every fresh
    /// placement whose two settings are both off (`controller.rs`), so
    /// it is the common case and must cost nothing, not a log line per
    /// hover.
    #[test]
    fn an_empty_scan_overlay_command_clears_without_a_word() {
        let dir = scratch("scanclear");
        let log_file = dir.join("chibipop.log");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);

        app.execute(Command::ShowScanOverlay { rects: Vec::new() });

        let written = std::fs::read_to_string(&log_file).unwrap();
        assert!(!written.contains("overlay:"), "clearing is silent: {written}");
        assert!(!written.contains("ShowScanOverlay"), "and still not a no-op: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What reaches the outline, for the one hover above: each box
    /// framed just outside itself so the next grab reads no border
    /// (ADR-0008), and the match in its own theme colour so it does not
    /// look like the capture box it was found in.
    #[test]
    fn a_hovers_scan_rects_reach_the_outline_outset_and_coloured_by_kind() {
        let theme = chibipop::ui::theme::Theme::dark();
        let marks = overlay::scan_marks(&one_hovers_rects(), &theme);

        assert_eq!(
            vec![
                PhysRect { x: 98, y: 198, w: 244, h: 64 },
                PhysRect { x: 138, y: 208, w: 44, h: 44 },
            ],
            marks.iter().map(|m| m.rect).collect::<Vec<_>>()
        );
        assert_eq!(
            vec![
                overlay::scan_colour(ScanKind::Pass1, &theme),
                overlay::scan_colour(ScanKind::Match, &theme),
            ],
            marks.iter().map(|m| m.colour).collect::<Vec<_>>()
        );
    }

    // -- the trigger channel's portal rung (ticket 36) --

    fn binding(id: shortcuts::ShortcutId, trigger: Option<&str>) -> shortcuts::Binding {
        shortcuts::Binding { id, trigger: trigger.map(str::to_string) }
    }

    /// The whole point of the rung: a portal press does exactly what
    /// `ctl trigger-down` does, and its release exactly what
    /// `trigger-up` does — one trigger semantics, two sources.
    #[test]
    fn a_portal_press_takes_the_same_frozen_grab_as_the_socket_verb() {
        let dir = scratch("portalhold");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);
        app.cursor_rung = Some(cursor::Rung::ImageCopyCapture);
        app.last_cursor = Some(PhysPoint { x: 400, y: 300 });

        app.handle_shortcut(shortcuts::Event::Fired {
            id: shortcuts::ShortcutId::Trigger,
            activated: true,
        });
        let hold = app.hold.expect("a portal press holds");
        assert!(!hold.latched, "a key press is not a latch");
        assert!(hold.output.contains(PhysPoint { x: 400, y: 300 }), "{:?}", hold.output);

        app.handle_shortcut(shortcuts::Event::Fired {
            id: shortcuts::ShortcutId::Trigger,
            activated: false,
        });
        assert_eq!(None, app.hold, "a portal release ends the hold");

        let written = std::fs::read_to_string(dir.join("chibipop.log")).unwrap();
        assert!(written.contains("trigger: portal activated trigger"), "log was: {written}");
        assert!(written.contains("trigger: portal deactivated trigger"), "log was: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The add shortcut is not a trigger: it must reach the Controller
    /// without freezing anything, and its release must do nothing at all.
    #[test]
    fn the_add_shortcut_never_takes_a_grab() {
        let dir = scratch("portaladd");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);
        app.cursor_rung = Some(cursor::Rung::ImageCopyCapture);
        app.last_cursor = Some(PhysPoint { x: 400, y: 300 });

        app.handle_shortcut(shortcuts::Event::Fired {
            id: shortcuts::ShortcutId::AnkiAdd,
            activated: true,
        });
        assert_eq!(None, app.hold, "anki-add is not the trigger");
        app.handle_shortcut(shortcuts::Event::Fired {
            id: shortcuts::ShortcutId::AnkiAdd,
            activated: false,
        });
        assert_eq!(None, app.hold);

        let written = std::fs::read_to_string(dir.join("chibipop.log")).unwrap();
        assert!(written.contains("trigger: portal activated anki-add"), "log was: {written}");
        assert!(!written.contains("frozen grab"), "log was: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What the portal bound is what the tray row and the settings
    /// window say — the observability half of ADR-0003's "channel
    /// selection is visible".
    #[test]
    fn a_bind_names_the_owner_in_the_row_and_publishes_it() {
        let dir = scratch("portalbound");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);

        app.handle_shortcut(shortcuts::Event::Bound(vec![
            binding(shortcuts::ShortcutId::Trigger, Some("Alt+F")),
            binding(shortcuts::ShortcutId::AnkiAdd, None),
        ]));

        let row = app.tray.statuses().row(ChannelId::Trigger);
        assert!(row.contains("GlobalShortcuts portal"), "{row}");
        assert!(row.contains("trigger Alt+F"), "{row}");
        assert!(row.contains("anki-add (key not reported)"), "{row}");
        // A working trigger never raises the tray's attention icon: the
        // socket is up and so is the portal.
        assert_eq!(ksni::Status::Active, app.tray.statuses().sni_status());

        let published = shortcuts::state::read(&dir).expect("the daemon publishes the channel");
        assert!(published.portal);
        assert_eq!(Some("Alt+F".to_string()), published.description(ShortcutId::Trigger));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The user re-bound the key in their desktop's own UI: the row and
    /// the published state follow, without a restart.
    #[test]
    fn shortcuts_changed_updates_the_row_and_the_published_key() {
        let dir = scratch("portalchanged");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);

        app.handle_shortcut(shortcuts::Event::Bound(vec![binding(
            shortcuts::ShortcutId::Trigger,
            Some("Alt+F"),
        )]));
        app.handle_shortcut(shortcuts::Event::Changed(vec![binding(
            shortcuts::ShortcutId::Trigger,
            Some("Meta+Shift+R"),
        )]));

        let row = app.tray.statuses().row(ChannelId::Trigger);
        assert!(row.contains("Meta+Shift+R"), "{row}");
        assert!(!row.contains("Alt+F"), "the old key must be gone: {row}");
        assert_eq!(
            Some("Meta+Shift+R".to_string()),
            shortcuts::state::read(&dir).expect("published").description(ShortcutId::Trigger)
        );
        let written = std::fs::read_to_string(dir.join("chibipop.log")).unwrap();
        assert!(written.contains("trigger: portal re-bound"), "log was: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A portal that cannot serve is a status with a reason in it, and
    /// the socket is still the trigger: no attention icon, and the
    /// settings window stops claiming a portal binding.
    #[test]
    fn an_unavailable_portal_leaves_the_socket_serving() {
        let dir = scratch("portalgone");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);

        app.handle_shortcut(shortcuts::Event::Bound(vec![binding(
            shortcuts::ShortcutId::Trigger,
            Some("Alt+F"),
        )]));
        app.handle_shortcut(shortcuts::Event::Unavailable {
            reason: "CreateSession: the portal requires an app id".to_string(),
            advice: Some("launch chibipop from its desktop entry".to_string()),
        });

        let row = app.tray.statuses().row(ChannelId::Trigger);
        assert!(row.contains("control socket"), "{row}");
        assert!(row.contains("app id"), "the row must carry the reason: {row}");
        assert!(!row.contains("desktop entry"), "the advice belongs in the log: {row}");
        assert_eq!(ksni::Status::Active, app.tray.statuses().sni_status());
        let written = std::fs::read_to_string(dir.join("chibipop.log")).unwrap();
        assert!(
            written.contains("trigger: launch chibipop from its desktop entry"),
            "log was: {written}"
        );

        let published = shortcuts::state::read(&dir).expect("published");
        assert!(!published.portal);
        assert_eq!(None, published.description(ShortcutId::Trigger));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The portal thread owns no log, so its diagnostics travel as
    /// events and are written here.
    #[test]
    fn a_shortcut_note_reaches_the_daemon_log() {
        let dir = scratch("portalnote");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);
        app.handle_shortcut(shortcuts::Event::Note("trigger: v2 session /foo".to_string()));
        let written = std::fs::read_to_string(dir.join("chibipop.log")).unwrap();
        assert!(written.contains("trigger: v2 session /foo"), "log was: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- live hover and the dwell re-check (ADR-0010) --

    use chibipop::lookup::deconj::Deconjugator;
    use chibipop::lookup::engine::LookupEngine;
    use chibipop::lookup::model::{FakeDictionary, Sense};
    use chibipop::text::layout::{OcrLine, OcrWord};
    use chibipop::text::{Frame, OcrEngine, RegionCapture};
    use chibipop::worker::WorkerParts;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Generous: never reached on a healthy run.
    const TIMEOUT: Duration = Duration::from_secs(10);

    /// Where the fakes' hovers land, and the word they read.
    const AT: PhysPoint = PhysPoint { x: 600, y: 300 };
    const WORD: &str = "\u{98DF}";

    /// One fake output: big enough to hold a read region, small enough
    /// that a press-time full grab is a cheap allocation.
    const FAKE_OUTPUT: i32 = 1000;

    /// What the fake seams did, in order. One Vec rather than a channel
    /// per seam: both run on the worker thread, so appending under one
    /// lock is the order the pipeline actually took, and a received
    /// result is the barrier that makes it complete.
    type Seams = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    fn seams() -> Seams {
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()))
    }

    fn note(log: &Seams, line: &str) {
        log.lock().expect("the seam log").push(line.to_string());
    }

    fn done(log: &Seams) -> Vec<String> {
        log.lock().expect("the seam log").clone()
    }

    /// Canned pixels; every grab is logged and can be held open, so an
    /// in-flight read is something a test can stand on.
    struct FakeCapture {
        log: Seams,
        gate: Option<mpsc::Receiver<()>>,
        entered: Option<mpsc::Sender<()>>,
    }

    impl RegionCapture for FakeCapture {
        fn grab(&mut self, region: PhysRect) -> anyhow::Result<Frame> {
            note(&self.log, "grab");
            if let Some(tx) = &self.entered {
                let _ = tx.send(());
            }
            if let Some(gate) = &self.gate {
                gate.recv_timeout(TIMEOUT).expect("the test must release the gated grab");
            }
            Ok(Frame {
                buf: vec![0u8; (region.w * region.h * 4) as usize],
                w: region.w,
                h: region.h,
                source: "fake",
                fallback: None,
                unchanged: false,
            })
        }

        fn bounds_containing(&self, p: PhysPoint) -> PhysRect {
            PhysRect {
                x: p.x - FAKE_OUTPUT / 2,
                y: p.y - FAKE_OUTPUT / 2,
                w: FAKE_OUTPUT,
                h: FAKE_OUTPUT,
            }
        }

        fn begin_read(&mut self) {
            note(&self.log, "begin_read");
        }

        fn end_read(&mut self) {
            note(&self.log, "end_read");
        }
    }

    /// One word over the whole grab, plus whether the pixels handed to
    /// it had been masked: the capture's own are black and a mask fills
    /// white (ADR-0008), so this is the mask itself, observed. Alpha is
    /// not evidence - the upscale sets it opaque either way.
    struct FakeOcr {
        log: Seams,
    }

    impl OcrEngine for FakeOcr {
        fn recognise(&self, bgra: &[u8], w: i32, h: i32) -> anyhow::Result<Vec<OcrLine>> {
            let masked = bgra
                .as_chunks::<4>()
                .0
                .iter()
                .any(|px| px[0] == 0xFF && px[1] == 0xFF && px[2] == 0xFF);
            note(&self.log, &format!("ocr masked={masked}"));
            Ok(vec![OcrLine {
                words: vec![OcrWord {
                    text: WORD.to_string(),
                    rect: PhysRect { x: 0, y: 0, w, h },
                }],
            }])
        }

        fn set_language(&mut self, _tag: &str) {}

        fn name(&self) -> &str {
            "fake-ocr"
        }

        fn provides_geometry(&self) -> bool {
            true
        }
    }

    /// The real core pipeline over those fakes: no screen, no OCR
    /// models, no database, and every pass countable.
    fn fake_worker(
        gate: Option<mpsc::Receiver<()>>,
        entered: Option<mpsc::Sender<()>>,
    ) -> (Worker, Seams) {
        fake_worker_serving(gate, entered, None)
    }

    /// The same pipeline with the shipped `serve` hook installed over the
    /// fake engine, so the one-off OCR seam can be driven with no
    /// compositor and no ONNX models.
    fn fake_worker_serving(
        gate: Option<mpsc::Receiver<()>>,
        entered: Option<mpsc::Sender<()>>,
        jobs: Option<mpsc::Receiver<worker::OcrRequest>>,
    ) -> (Worker, Seams) {
        let log = seams();
        let capture_log = log.clone();
        let ocr_log = log.clone();
        let settings = worker::settings(&chibipop::config::Config::default(), &[]);
        let (worker, _dicts) = Worker::spawn(
            settings,
            move || {
                let mut dict = FakeDictionary::new();
                dict.add_dict(1, "FakeDict");
                dict.add_term(WORD, None, None, "", None, 10, 1);
                dict.add_entry(
                    10,
                    1,
                    vec![Sense {
                        glosses: vec!["to eat".to_string()],
                        glosses_html: Vec::new(),
                        pos: Vec::new(),
                        misc: Vec::new(),
                    }],
                );
                Ok(WorkerParts {
                    capture: Box::new(FakeCapture { log: capture_log, gate, entered }),
                    ocr: Box::new(FakeOcr { log: ocr_log }),
                    dict: Box::new(dict),
                    reopen_dict: None,
                    engine: LookupEngine::new(Deconjugator::new(Vec::new())),
                    // The shipped hook, not a stand-in written here.
                    serve: jobs.map(worker::serve_jobs),
                })
            },
            || {},
        )
        .expect("the fake pipeline must start");
        (worker, log)
    }

    /// One answer off a calloop channel, or a test failure.
    ///
    /// `calloop::channel::Channel` has no `recv_timeout`, and a blocking
    /// `recv` on a hook that never ran would hang the suite instead of
    /// failing it.
    fn ocr_answer(
        answers: &calloop::channel::Channel<Result<Vec<OcrLine>, String>>,
    ) -> Result<Vec<OcrLine>, String> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match answers.try_recv() {
                Ok(read) => return read,
                Err(mpsc::TryRecvError::Empty) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) => panic!("the serve hook must answer within {TIMEOUT:?}: {e:?}"),
            }
        }
    }

    /// The one-off OCR seam end to end with no compositor: a queued job
    /// wakes a Worker that is blocked on its trigger channel, runs on the
    /// thread that owns the engine, and its lines come back over the
    /// pump's own channel. Core pins the same shape in
    /// `tests/worker.rs::a_nudged_job_wakes_a_blocked_worker_and_is_read_through_the_facade`;
    /// this is that against this crate's fakes and the shipped hook.
    ///
    /// The pixels are white, which `FakeOcr` reports as `masked=true`:
    /// that is how the assertion knows the engine was shown *this job's*
    /// bytes and not a capture grab's black ones - and the seam log
    /// containing no `grab` at all is the other half of it.
    #[test]
    fn a_queued_ocr_job_wakes_the_worker_and_answers_through_the_serve_hook() {
        let (jobs_tx, jobs_rx) = mpsc::channel::<worker::OcrRequest>();
        let (worker, log) = fake_worker_serving(None, None, Some(jobs_rx));
        let jobs = worker::OcrJobs::new(jobs_tx, worker.serve_nudge());
        let (answer, answers) = calloop::channel::channel::<Result<Vec<OcrLine>, String>>();

        jobs.send(worker::OcrRequest { bgra: vec![0xFF; 8 * 4 * 4], w: 8, h: 4, answer })
            .expect("a live pipeline takes the job");
        let lines = ocr_answer(&answers).expect("the fake engine answers");

        assert_eq!(
            done(&log),
            ["ocr masked=true"],
            "the job went through the OCR facade and touched no capture backend"
        );
        assert_eq!(
            PhysRect { x: 0, y: 0, w: 8, h: 4 },
            lines[0].words[0].rect,
            "the job's own dimensions reached the engine"
        );
        assert_eq!(WORD, chibipop::text::layout::join_lines(&lines), "and its text came back");
        // The nudge is the only thing that could have woken it: no
        // trigger was ever sent, so there is no lookup to receive.
        assert!(worker.results().try_recv().is_err(), "a serve wake answers no lookup");
    }

    /// A queue with no pipeline behind it refuses instead of parking the
    /// job for ever: `ocr-clipboard` on a daemon whose Worker never came
    /// up must say so rather than take a region and go quiet.
    #[test]
    fn an_ocr_job_queued_with_no_pipeline_is_refused_rather_than_parked() {
        let jobs = worker::OcrJobs::disconnected();
        let (answer, _answers) = calloop::channel::channel::<Result<Vec<OcrLine>, String>>();

        let refused = jobs
            .send(worker::OcrRequest { bgra: vec![0u8; 4], w: 1, h: 1, answer })
            .expect_err("a disconnected queue must refuse");

        assert!(
            format!("{refused:#}").contains("not running"),
            "the refusal must name the reason: {refused:#}"
        );
    }

    /// The pipeline's answer, or a test failure. Receiving it is what
    /// makes the seam log complete.
    fn answer(app: &App) -> chibipop::worker::WorkerResult {
        app.worker
            .as_ref()
            .expect("the pipeline")
            .results()
            .recv_timeout(TIMEOUT)
            .expect("the pipeline must answer")
    }

    /// The non-negotiable core: a cursor sample becomes a lookup on the
    /// sample. Nothing timed sits in between - no settle delay, no
    /// velocity gate, no dispatch tick (ADR-0010).
    #[test]
    fn a_cursor_sample_dispatches_a_live_lookup_at_once() {
        let dir = scratch("live");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);
        let (worker, log) = fake_worker(None, None);
        app.worker = Some(worker);

        app.on_cursor_position(AT);
        answer(&app);

        assert_eq!(
            done(&log),
            ["begin_read", "grab", "ocr masked=false", "end_read"],
            "one sample, one bracketed read"
        );
        assert_eq!(CaptureMode::Live, app.capture_mode(), "no hold: the grab is live");
        assert!(app.dwell.is_none(), "a dispatch arms no timer of its own");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An event rung delivers a position when its session opens and
    /// then only on movement (ADR-0003), so a daemon that came up with
    /// the cursor already resting on a word has exactly one sample -
    /// and the pipeline is the last thing to exist. Spending that
    /// sample on nothing would leave live mode silent until the mouse
    /// moved, and would spend the Controller's movement gate too: the
    /// same position asked twice is not a move.
    #[test]
    fn a_sample_that_arrives_before_the_pipeline_is_spent_once_it_is_up() {
        let dir = scratch("earlysample");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);
        assert!(app.worker.is_none(), "no pipeline yet, as at startup");

        app.on_cursor_position(AT);
        // A rung may re-deliver the resting position (a session that
        // reopens, an output that re-enters); none of those samples may
        // reach the Controller while there is nothing to ask.
        app.on_cursor_position(AT);
        let written = std::fs::read_to_string(dir.join("chibipop.log")).unwrap();
        assert!(!written.contains("lookup:"), "nothing to ask yet: {written}");

        // What `spawn_worker` does the moment a pipeline exists.
        let (worker, log) = fake_worker(None, None);
        app.worker = Some(worker);
        app.look_where_the_cursor_is();
        answer(&app);

        assert_eq!(
            done(&log),
            ["begin_read", "grab", "ocr masked=false", "end_read"],
            "the resting cursor gets its lookup - the gate was never spent"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The dictionary names the "Not searched" split is matched against
    /// are the pipeline's own first read, so `spawn_worker` cannot
    /// resolve the scope before the pipeline exists - it re-resolves
    /// after. Without that, a fresh daemon searched every dictionary
    /// until something sent `reload`, which is the ticket-08 defect one
    /// step further in.
    #[test]
    fn a_fresh_pipeline_is_told_the_split_once_the_names_are_known() {
        let dir = scratch("rescope");
        let log_file = dir.join("chibipop.log");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        app.config.ocr.language = "ja".to_string();
        app.config
            .dictionaries
            .per_language
            .insert("ja".to_string(), vec!["大辞林".to_string()]);
        // What `spawn_worker` handed over: no identities, no restriction.
        let sent = worker::settings(&app.config, &[]).present_cfg;
        assert!(!sent.restrict_to_order, "an empty library cannot be restricted");

        let (worker, _seams) = fake_worker(None, None);
        app.worker = Some(worker);
        app.dicts = vec![
            DictInfo { dict_id: 1, name: "大辞林　第四版".to_string() },
            DictInfo { dict_id: 2, name: "Jitendex.org [2026-07-09]".to_string() },
        ];
        app.rescope_lookups(&sent);

        let written = std::fs::read_to_string(&log_file).unwrap();
        assert!(written.contains("ja searches 1 of 2"), "log was: {written}");
        assert!(app.worker.is_some(), "the reload must have reached the pipeline");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// And costs nothing when there is nothing to say: the shipped
    /// config has no split, and every respawn already holds the names.
    #[test]
    fn a_pipeline_whose_scope_did_not_change_is_left_alone() {
        let dir = scratch("norescope");
        let log_file = dir.join("chibipop.log");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let (worker, _seams) = fake_worker(None, None);
        app.worker = Some(worker);
        app.dicts = vec![DictInfo { dict_id: 1, name: "Jitendex.org".to_string() }];
        let sent = worker::settings(&app.config, &[]).present_cfg;

        app.rescope_lookups(&sent);

        let written = std::fs::read_to_string(&log_file).unwrap();
        assert!(!written.contains("searches"), "log was: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A live grab must not read our own popup; a hold's frozen grab
    /// reads straight through it (ADR-0008/0010). The fake recogniser
    /// reports the fill it was handed, so both halves are observed
    /// rather than argued.
    #[test]
    fn a_live_lookup_masks_the_popup_and_a_hold_reads_through_it() {
        let dir = scratch("livemask");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);
        let (worker, log) = fake_worker(None, None);
        app.worker = Some(worker);
        // Over the hovered point, as a placed popup can be.
        let popup = PhysRect { x: AT.x - 40, y: AT.y - 40, w: 200, h: 120 };

        app.execute(Command::RequestLookup { id: RequestId(1), point: AT, popup: Some(popup) });
        answer(&app);
        assert_eq!(
            done(&log),
            ["begin_read", "grab", "ocr masked=true", "end_read"],
            "a live lookup masks the popup out of its own OCR input"
        );

        app.cursor_rung = Some(cursor::Rung::ImageCopyCapture);
        app.last_cursor = Some(AT);
        app.handle_request("trigger-down", Some(Verb::TriggerDown));
        assert_eq!(CaptureMode::Frozen, app.capture_mode());
        answer(&app);
        // The press's own full grab, bracketed like any other read -
        // then the hold's lookup, which touches no backend at all and
        // masks nothing, because those pixels predate the popup.
        assert_eq!(
            done(&log)[4..],
            ["begin_read", "grab", "end_read", "ocr masked=false"],
            "the hold reads through the popup: {:?}",
            done(&log)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Backpressure is the pacer: samples arriving behind an in-flight
    /// read coalesce to the newest, and the daemon queues nothing of its
    /// own (ADR-0010 - one in flight, latest-wins).
    #[test]
    fn samples_behind_an_in_flight_lookup_coalesce_to_the_newest() {
        let dir = scratch("coalesce");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let (worker, log) = fake_worker(Some(gate_rx), Some(entered_tx));
        app.worker = Some(worker);

        app.on_cursor_position(AT);
        entered_rx.recv_timeout(TIMEOUT).expect("the first read must start");
        // Both land while that read is held open.
        app.on_cursor_position(PhysPoint { x: AT.x + 100, y: AT.y });
        app.on_cursor_position(PhysPoint { x: AT.x + 200, y: AT.y });
        gate_tx.send(()).unwrap();

        let results = app.worker.as_ref().expect("the pipeline").results();
        assert_eq!(RequestId(1), results.recv_timeout(TIMEOUT).unwrap().id);
        entered_rx.recv_timeout(TIMEOUT).expect("the coalesced read must start");
        gate_tx.send(()).unwrap();
        assert_eq!(
            RequestId(3),
            results.recv_timeout(TIMEOUT).unwrap().id,
            "the newest sample wins"
        );
        assert!(results.try_recv().is_err(), "the superseded sample never answers");
        assert_eq!(
            2,
            done(&log).iter().filter(|line| *line == "grab").count(),
            "three samples, two grabs: the middle one never captured"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The daemon's own half of the arming rule. A hold reads pixels
    /// that cannot change, so trigger mode is never dwell-watched.
    #[test]
    fn a_frozen_hold_is_never_dwell_watched() {
        let output = PhysRect { x: 0, y: 0, w: 1920, h: 1080 };
        assert!(dwell_wanted(None, true), "a shown popup in live mode is watched");
        assert!(!dwell_wanted(Some(Hold { output, latched: false }), true));
        assert!(!dwell_wanted(Some(Hold { output, latched: true }), true));
        assert!(!dwell_wanted(None, false), "nothing shown is nothing to watch");
    }

    /// A watch with nothing on screen asks the pipeline for nothing and
    /// retires on its own deadline - which is what leaves an idle daemon
    /// holding no timed source at all (ADR-0010's zero idle wakeups).
    ///
    /// Armed by hand: a popup with a rect needs a compositor, and the
    /// Controller's half of the decision is core's own test.
    #[test]
    fn a_dwell_watch_with_nothing_shown_asks_nothing_and_retires() {
        let dir = scratch("dwellidle");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);
        app.trace = true;
        let (worker, log) = fake_worker(None, None);
        app.worker = Some(worker);

        app.arm_dwell();
        assert!(app.dwell.is_some(), "armed");
        let escape = event_loop.get_signal();
        let mut passes = 0;
        event_loop
            .run(Some(budget::DWELL), &mut app, |_| {
                passes += 1;
                if passes >= 3 {
                    escape.stop();
                }
            })
            .unwrap();

        assert!(app.dwell.is_none(), "one deadline with nothing shown retires the watch");
        assert!(done(&log).is_empty(), "and asks the pipeline for nothing");
        let written = std::fs::read_to_string(dir.join("chibipop.log")).unwrap();
        assert_eq!(1, written.matches("dwell: deadline").count(), "log was: {written}");
        assert!(written.contains("watch retired"), "log was: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- AnkiConnect (ticket 42) --

    use std::io::{Read, Write};

    /// A fake AnkiConnect.
    ///
    /// The seam under test is a socket, not a trait: `chibipop::anki`
    /// speaks plain HTTP through `ureq`, and mirroring the Windows bin
    /// exactly means the daemon's calls really do leave the process.
    /// So this is the far end of the wire, and it remembers every
    /// request body - which is how a test can assert on the deck, the
    /// model and the fields that were actually sent.
    struct FakeAnki {
        url: String,
        seen: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    }

    impl FakeAnki {
        /// Answers `replies` requests, then closes.
        fn start(replies: usize) -> FakeAnki {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
            let url = format!("http://{}", listener.local_addr().expect("the bound address"));
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let recorded = seen.clone();
            std::thread::spawn(move || {
                for _ in 0..replies {
                    let Ok((mut stream, _)) = listener.accept() else { return };
                    let request: serde_json::Value =
                        serde_json::from_str(&read_body(&mut stream)).unwrap_or(serde_json::Value::Null);
                    let reply = canned_reply(&request);
                    recorded.lock().expect("the request log").push(request);
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                        reply.len()
                    );
                }
            });
            FakeAnki { url, seen }
        }

        fn seen(&self) -> Vec<serde_json::Value> {
            self.seen.lock().expect("the request log").clone()
        }
    }

    /// One HTTP request's body, by its `Content-Length`.
    fn read_body(stream: &mut std::net::TcpStream) -> String {
        let mut raw = Vec::new();
        let mut byte = [0u8; 1];
        // Headers first: read one byte at a time so the body is not
        // swallowed into a buffer this function cannot give back.
        while !raw.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(1) => raw.push(byte[0]),
                _ => return String::new(),
            }
        }
        let headers = String::from_utf8_lossy(&raw).to_lowercase();
        let len = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; len];
        if stream.read_exact(&mut body).is_err() {
            return String::new();
        }
        String::from_utf8_lossy(&body).to_string()
    }

    /// AnkiConnect v6's answer to the two actions the popup makes.
    ///
    /// `canAddNotes` refuses the first note and accepts the rest, so a
    /// test has exactly one known duplicate to assert on.
    fn canned_reply(request: &serde_json::Value) -> String {
        match request.get("action").and_then(|a| a.as_str()) {
            Some("canAddNotes") => {
                let notes = request
                    .get("params")
                    .and_then(|p| p.get("notes"))
                    .and_then(|n| n.as_array())
                    .map_or(0, Vec::len);
                let flags: Vec<&str> =
                    (0..notes).map(|i| if i == 0 { "false" } else { "true" }).collect();
                format!("{{\"result\":[{}],\"error\":null}}", flags.join(","))
            }
            Some("addNote") => "{\"result\":1729,\"error\":null}".to_string(),
            _ => "{\"result\":null,\"error\":null}".to_string(),
        }
    }

    /// Point the daemon at the fake and turn the feature on.
    fn anki_at(app: &mut App, url: &str) {
        app.config.anki.enabled = true;
        app.config.anki.url = url.to_string();
        app.config.anki.deck = "Mining".to_string();
        app.config.anki.model = "Lapis".to_string();
        app.controller = Controller::new(controller_config(&app.config));
    }

    /// Pump until `wanted` is in the log or `budget` passes are spent,
    /// then hand the log back.
    ///
    /// An AnkiConnect answer crosses a thread and a calloop channel, so
    /// there is nothing to assert on until the pump has dispatched it -
    /// and a test that expects *no* answer has to spend a budget to be
    /// worth anything.
    fn pump_until(
        event_loop: &mut EventLoop<'static, App>,
        app: &mut App,
        log_file: &std::path::Path,
        wanted: &str,
        budget: u32,
    ) -> String {
        let escape = event_loop.get_signal();
        let mut passes = 0;
        let mut written = String::new();
        event_loop
            .run(Some(Duration::from_millis(50)), app, |_| {
                passes += 1;
                written = std::fs::read_to_string(log_file).unwrap_or_default();
                if written.contains(wanted) || passes >= budget {
                    escape.stop();
                }
            })
            .unwrap();
        written
    }

    /// The dupe check the Controller orders when a popup lands: it must
    /// reach the real AnkiConnect action, carrying the configured deck
    /// and model, and its answer must come back to the pump.
    #[test]
    fn a_dupe_check_goes_out_over_http_and_its_answer_comes_back() {
        let dir = scratch("ankidupes");
        let log_file = dir.join("chibipop.log");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let anki = FakeAnki::start(1);
        anki_at(&mut app, &anki.url);

        app.execute(Command::CheckDupes {
            generation: 7,
            exprs: vec![WORD.to_string(), "\u{732B}".to_string()],
        });
        let written = pump_until(&mut event_loop, &mut app, &log_file, "anki: dupe check", 60);

        let seen = anki.seen();
        assert_eq!(1, seen.len(), "one check, one request: {seen:?}");
        assert_eq!(Some("canAddNotes"), seen[0]["action"].as_str());
        let notes = seen[0]["params"]["notes"].as_array().expect("the notes");
        assert_eq!(2, notes.len(), "one note per expression: {notes:?}");
        assert_eq!(Some("Mining"), notes[0]["deckName"].as_str());
        assert_eq!(Some("Lapis"), notes[0]["modelName"].as_str());
        assert!(
            written.contains("anki: dupe check answered - 1 of the popup's expressions"),
            "the fake refuses the first note, so exactly one is a duplicate: {written}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The add itself: the Anki button and the `anki-add` shortcut both
    /// end here, and this is the call that creates the card. The fields
    /// must arrive mapped by `anki.field_map`, exactly as on Windows.
    #[test]
    fn an_add_creates_the_note_through_the_configured_field_map() {
        let dir = scratch("ankiadd");
        let log_file = dir.join("chibipop.log");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let anki = FakeAnki::start(1);
        anki_at(&mut app, &anki.url);

        let mut fields = HashMap::new();
        fields.insert("expression".to_string(), WORD.to_string());
        fields.insert("reading".to_string(), "\u{305F}\u{3079}".to_string());
        app.execute(Command::AddNote { expr: WORD.to_string(), fields });
        let written = pump_until(&mut event_loop, &mut app, &log_file, "anki: card added", 60);

        let seen = anki.seen();
        assert_eq!(1, seen.len(), "one add, one request: {seen:?}");
        assert_eq!(Some("addNote"), seen[0]["action"].as_str());
        let note = &seen[0]["params"]["note"];
        assert_eq!(Some("Mining"), note["deckName"].as_str());
        assert_eq!(Some("Lapis"), note["modelName"].as_str());
        assert_eq!(
            Some(WORD),
            note["fields"]["Expression"].as_str(),
            "the default map routes expression -> Expression: {note}"
        );
        assert!(written.contains("anki: card added as note 1729"), "log was: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Anki not running is the common case, and it must cost one line
    /// and nothing else: no panic, no pump stalled on a dead socket.
    #[test]
    fn an_ankiconnect_that_is_not_listening_is_one_line() {
        let dir = scratch("ankidown");
        let log_file = dir.join("chibipop.log");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        // A port that was bound and let go: nothing is listening on it.
        let dead = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
            probe.local_addr().expect("the bound address")
        };
        anki_at(&mut app, &format!("http://{dead}"));

        app.execute(Command::AddNote { expr: WORD.to_string(), fields: HashMap::new() });
        let written = pump_until(&mut event_loop, &mut app, &log_file, "anki: adding the card", 60);

        assert!(written.contains("anki: adding the card failed"), "log was: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Both entry points are the same guarded Event: with nothing on
    /// screen there is nothing to add, and neither the shortcut nor a
    /// click on the slot may reach the network. This is the Windows
    /// enable rule - the button is not there, and the hotkey is not
    /// armed - arrived at through the Controller instead of a hook.
    #[test]
    fn an_add_with_nothing_shown_asks_ankiconnect_for_nothing() {
        let dir = scratch("ankiunarmed");
        let log_file = dir.join("chibipop.log");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let anki = FakeAnki::start(1);
        anki_at(&mut app, &anki.url);

        app.handle_shortcut(shortcuts::Event::Fired {
            id: shortcuts::ShortcutId::AnkiAdd,
            activated: true,
        });
        app.pointer_interactions(vec![popup::Interaction::Anki {
            local: PhysPoint { x: 10, y: 10 },
        }]);
        // Long enough for a request to have been made, had one been.
        let written = pump_until(&mut event_loop, &mut app, &log_file, "anki: ", 8);

        assert!(anki.seen().is_empty(), "no popup, no card: {:?}", anki.seen());
        assert!(!written.contains("anki: "), "log was: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A popup the Controller believes is on screen, and the generation
    /// of the dupe check its presentation ordered.
    ///
    /// Driven straight into the Controller because the real placement
    /// round-trip needs a compositor to answer `PopupPlaced`, and what
    /// is under test below is the path from a shortcut press to the
    /// AnkiConnect call - not the layer surface.
    ///
    /// The generation is answered because it is the only way a test can
    /// answer that dupe check: `Controller::dupes_checked` refuses a
    /// stale one, and the popup's `connected` flag - which decides
    /// whether a mining screenshot has a card to file - is exactly what
    /// the answer sets. `None` where Anki is off and no check was
    /// ordered; callers that do not care ignore the whole thing.
    fn place_a_popup(app: &mut App) -> Option<u64> {
        use chibipop::present::{Card, Presentation};
        use chibipop::text::layout::Orientation;

        let anchor = PhysRect { x: 100, y: 100, w: 40, h: 40 };
        let out = app.controller.handle(Event::CursorMoved { pos: AT });
        let id = out
            .iter()
            .find_map(|cmd| match cmd {
                Command::RequestLookup { id, .. } => Some(*id),
                _ => None,
            })
            .expect("a live sample dispatches a lookup");
        app.controller.handle(Event::LookupResult {
            id,
            outcome: chibipop::controller::LookupOutcome::Ready {
                presentation: Box::new(Presentation {
                    top: Some(Card {
                        written: Some(WORD.to_string()),
                        reading: None,
                        pos: Vec::new(),
                        freq: None,
                        blocks: Vec::new(),
                        match_len: 1,
                    }),
                    collapsed: Vec::new(),
                    all_cards: Vec::new(),
                    sentence: None,
                }),
                anchor,
                orientation: Orientation::Horizontal,
                matched: None,
                scan: Vec::new(),
            },
        });
        // The dupe check is ordered when the rect lands, not when the
        // presentation does: `begin_place` finishes at `PopupPlaced`.
        let ordered = app.controller.handle(Event::PopupPlaced {
            rect: PhysRect { x: 100, y: 150, w: 300, h: 200 },
            content_h: 200,
            view_h: 200,
        });
        assert!(app.controller.popup().is_some(), "the Controller must think it is shown");
        ordered.iter().find_map(|cmd| match cmd {
            Command::CheckDupes { generation, .. } => Some(*generation),
            _ => None,
        })
    }

    /// The `anki-add` portal shortcut creates a card for the current
    /// lookup. The press cannot be synthesized here - the portal rung
    /// needs an app id and a real key - so it enters where the portal
    /// thread's events enter, and the card it produces is asserted on
    /// the wire.
    #[test]
    fn the_anki_add_shortcut_creates_a_card_for_the_shown_lookup() {
        let dir = scratch("ankishortcut");
        let log_file = dir.join("chibipop.log");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let anki = FakeAnki::start(1);
        anki_at(&mut app, &anki.url);
        place_a_popup(&mut app);

        app.handle_shortcut(shortcuts::Event::Fired {
            id: shortcuts::ShortcutId::AnkiAdd,
            activated: true,
        });
        let written = pump_until(&mut event_loop, &mut app, &log_file, "anki: card added", 60);

        let seen = anki.seen();
        assert_eq!(1, seen.len(), "one press, one card: {seen:?}");
        assert_eq!(Some("addNote"), seen[0]["action"].as_str());
        assert_eq!(
            Some(WORD),
            seen[0]["params"]["note"]["fields"]["Expression"].as_str(),
            "the card is the lookup that is on screen: {seen:?}"
        );
        assert!(written.contains("anki: card added as note 1729"), "log was: {written}");

        // The release is not a second add (`Action::Nothing`), and the
        // Controller refuses a repeat of one it already added.
        app.handle_shortcut(shortcuts::Event::Fired {
            id: shortcuts::ShortcutId::AnkiAdd,
            activated: false,
        });
        app.handle_shortcut(shortcuts::Event::Fired {
            id: shortcuts::ShortcutId::AnkiAdd,
            activated: true,
        });
        pump_until(&mut event_loop, &mut app, &log_file, "never logged", 4);
        assert_eq!(1, anki.seen().len(), "one card, however often it is asked for");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same card, asked for over the control socket instead of the
    /// portal - ADR-0003 rung 2, the only rung a sway user has. It
    /// enters through `handle_request` with the verb parsed off the
    /// wire word, so what is driven here is `chibipop ctl anki-add`
    /// end to end minus the socket bytes (those are `control`'s own
    /// round-trip test).
    #[test]
    fn the_anki_add_verb_creates_the_same_card_the_portal_shortcut_does() {
        let dir = scratch("ankiverb");
        let log_file = dir.join("chibipop.log");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let anki = FakeAnki::start(1);
        anki_at(&mut app, &anki.url);
        place_a_popup(&mut app);

        app.handle_request("anki-add", Verb::parse("anki-add"));
        let written = pump_until(&mut event_loop, &mut app, &log_file, "anki: card added", 60);

        let seen = anki.seen();
        assert_eq!(1, seen.len(), "one verb, one card: {seen:?}");
        assert_eq!(Some("addNote"), seen[0]["action"].as_str());
        assert_eq!(
            Some(WORD),
            seen[0]["params"]["note"]["fields"]["Expression"].as_str(),
            "the card is the lookup that is on screen: {seen:?}"
        );
        assert!(written.contains("anki: card added as note 1729"), "log was: {written}");
        assert!(
            written.contains("control: anki-add - card requested"),
            "the socket logs what it was asked for: {written}"
        );

        // And the two rungs are one path, not two: a portal press after
        // the verb's card adds nothing, because the Controller already
        // knows this lookup was added.
        app.handle_shortcut(shortcuts::Event::Fired {
            id: shortcuts::ShortcutId::AnkiAdd,
            activated: true,
        });
        pump_until(&mut event_loop, &mut app, &log_file, "never logged", 4);
        assert_eq!(1, anki.seen().len(), "one card, whichever rung asks");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Anki off is the affordance gone: no dupe check when the popup
    /// lands, and a press that reaches nothing - the same rule the
    /// Windows button and its hotkey follow.
    #[test]
    fn anki_disabled_never_reaches_ankiconnect() {
        let dir = scratch("ankioff");
        let log_file = dir.join("chibipop.log");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let anki = FakeAnki::start(1);
        anki_at(&mut app, &anki.url);
        app.config.anki.enabled = false;
        app.controller = Controller::new(controller_config(&app.config));
        place_a_popup(&mut app);

        app.handle_shortcut(shortcuts::Event::Fired {
            id: shortcuts::ShortcutId::AnkiAdd,
            activated: true,
        });
        let written = pump_until(&mut event_loop, &mut app, &log_file, "anki: ", 8);

        assert!(anki.seen().is_empty(), "anki off, nothing on the wire: {:?}", anki.seen());
        assert!(!written.contains("anki: "), "log was: {written}");
        assert!(
            !app.controller.anki().expect("shown").enabled,
            "and the affordance's own state says so, which is what hides the slot"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- the mining screenshot and the picture that rides an add ----
    //
    // **How the region pick is stubbed.** None of these drives a
    // compositor. Two seams stand in for one, and both are the exact
    // entry points the real flow uses:
    //
    // - `App::took_shot_region(picked, shot)` is the pick's answer,
    //   the same split `static-region` uses. `None` is a cancel.
    // - `App::handle_shot(grabbed)` is what the grabbing thread's
    //   `calloop::channel` calls, so a fabricated `Frame` is
    //   indistinguishable here from real pixels.
    //
    // Only the drag itself and `capture::oneshot` are left out, and both
    // are covered against a real compositor by ticket 03
    // (`tests/surfaces_live.rs`, `capture-dump`).

    /// Two by two of solid blue, which is the smallest thing
    /// `encode_bgra_to_png` will take: BGRA8, top-down, `w * h * 4`.
    fn test_frame() -> Frame {
        Frame {
            buf: [255, 0, 0, 255].repeat(4),
            w: 2,
            h: 2,
            source: "test",
            fallback: None,
            unchanged: false,
        }
    }

    /// Turn the screenshot gate on and route the picture somewhere.
    ///
    /// `save_dir` is relative, so it resolves under `test_app`'s XDG
    /// `data_dir` - which is the scratch dir - through the same
    /// `Paths::screenshots_dir` the daemon uses.
    fn screenshots_on(app: &mut App) {
        app.config.actions.screenshot.include_on_add = true;
        app.config.actions.screenshot.save_dir = "shots".to_string();
        app.config.anki.field_map.push(chibipop::config::FieldMapping {
            anki_field: "Screenshot".to_string(),
            source: "screenshot".to_string(),
        });
    }

    /// The parked plan, or a failure that says what was there instead.
    fn parked(app: &mut App) -> Pending {
        match app.shot.take() {
            Some(Shot::Parked(shot)) => shot,
            Some(Shot::Grabbing(_)) => panic!("a grab is in flight, not a parked plan"),
            None => panic!("no plan was parked - the add went out without a picture"),
        }
    }

    /// The whole feature, end to end minus the compositor: the add the
    /// `anki-add` verb authorises is *not* dispatched as a plain add, and
    /// the card that does go out carries a picture whose filename, target
    /// field and folder all came from `chibipop::shot`.
    #[test]
    fn an_add_that_carries_a_screenshot_files_the_picture_core_planned() {
        let dir = scratch("shotadd");
        let log_file = dir.join("chibipop.log");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let anki = FakeAnki::start(1);
        anki_at(&mut app, &anki.url);
        screenshots_on(&mut app);
        place_a_popup(&mut app);

        app.handle_request("anki-add", Verb::parse("anki-add"));

        // The plan is parked and the plain add was suppressed: core owns
        // the filename and the picture field, and this is where they show.
        let shot = parked(&mut app);
        assert_eq!(dir.join("shots"), shot.plan.path.parent().expect("a folder"));
        let stem =
            shot.plan.path.file_stem().expect("a stem").to_string_lossy().into_owned();
        assert!(stem.starts_with(WORD), "core names the file after the word: {stem}");
        assert_eq!(Some("Screenshot".to_string()), shot.plan.picture_field);
        assert!(anki.seen().is_empty(), "the plain add must not have gone out too");

        // The grabbing thread's answer, with pixels standing in for a
        // real region (see the note above).
        let png = shot.plan.path.clone();
        app.shot = Some(Shot::Grabbing(shot));
        app.handle_shot(Ok(test_frame()));
        let written =
            pump_until(&mut event_loop, &mut app, &log_file, "anki: card added", 60);

        let seen = anki.seen();
        assert_eq!(1, seen.len(), "one add, one request: {seen:?}");
        assert_eq!(Some("addNote"), seen[0]["action"].as_str());
        let note = &seen[0]["params"]["note"];
        assert_eq!(
            Some(WORD),
            note["fields"]["Expression"].as_str(),
            "the card is still the lookup on screen: {note}"
        );
        let picture = &note["picture"][0];
        assert_eq!(
            Some(format!("chibipop-screenshot-{stem}.png").as_str()),
            picture["filename"].as_str(),
            "the attachment is core's namespaced name: {note}"
        );
        assert_eq!(
            Some("Screenshot"),
            picture["fields"][0].as_str(),
            "and it lands in the field the map routes `screenshot` to: {note}"
        );
        assert!(
            !picture["data"].as_str().unwrap_or_default().is_empty(),
            "with the PNG itself as base64: {note}"
        );
        assert!(png.is_file(), "and the file is on disk at {}", png.display());
        assert!(
            written.contains("anki: card added with a screenshot as note 1729"),
            "log was: {written}"
        );
        assert!(app.shot.is_none(), "and nothing is left in flight");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The gate is core's and it is off by default: an add with
    /// `include_on_add = false` is byte for byte the add that shipped
    /// before this feature - no plan, no pick, no picture on the wire.
    #[test]
    fn an_add_with_the_screenshot_gate_off_is_the_plain_add_it_always_was() {
        let dir = scratch("shotoff");
        let log_file = dir.join("chibipop.log");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let anki = FakeAnki::start(1);
        anki_at(&mut app, &anki.url);
        screenshots_on(&mut app);
        app.config.actions.screenshot.include_on_add = false;
        place_a_popup(&mut app);

        app.handle_request("anki-add", Verb::parse("anki-add"));
        assert!(app.shot.is_none(), "the gate is off, so nothing is parked");
        let written =
            pump_until(&mut event_loop, &mut app, &log_file, "anki: card added", 60);

        let seen = anki.seen();
        assert_eq!(1, seen.len(), "one add, one request: {seen:?}");
        assert!(
            seen[0]["params"]["note"].get("picture").is_none(),
            "and no picture rides along: {:?}",
            seen[0]
        );
        assert!(written.contains("anki: card added as note 1729"), "log was: {written}");
        assert!(!dir.join("shots").exists(), "nothing was written to the folder either");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The deferral itself, plus the documented cancel: the plan is
    /// parked *inside* the socket's callback and drained by the pump
    /// afterwards, and a pick that answers nothing files the card
    /// exactly once - without a picture. There is no layer shell here,
    /// so the pick really does answer nothing.
    #[test]
    fn a_cancelled_screenshot_pick_files_the_card_once_without_a_picture() {
        let dir = scratch("shotcancel");
        let log_file = dir.join("chibipop.log");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let anki = FakeAnki::start(2);
        anki_at(&mut app, &anki.url);
        screenshots_on(&mut app);
        place_a_popup(&mut app);

        app.handle_request("anki-add", Verb::parse("anki-add"));
        assert!(
            matches!(app.shot, Some(Shot::Parked(_))),
            "the pick must not run inside the command batch"
        );
        assert!(
            !std::fs::read_to_string(&log_file).unwrap_or_default().contains("select: pick"),
            "and no pick has been attempted yet"
        );

        let written =
            pump_until(&mut event_loop, &mut app, &log_file, "anki: card added", 60);

        assert!(
            written.contains("select: pick took"),
            "the pump's idle pass owns the pick: {written}"
        );
        assert!(
            written.contains("screenshot: no region was picked - the card goes in without a picture"),
            "log was: {written}"
        );
        let seen = anki.seen();
        assert_eq!(1, seen.len(), "exactly one add, and only one: {seen:?}");
        assert!(seen[0]["params"]["note"].get("picture").is_none(), "{:?}", seen[0]);
        assert!(app.shot.is_none(), "and the slot is free for the next one");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A grab that never came back is Windows' rule exactly: a card
    /// without a picture, not a card that never happens.
    #[test]
    fn a_screenshot_grab_that_failed_still_files_the_card_without_a_picture() {
        let dir = scratch("shotnograb");
        let log_file = dir.join("chibipop.log");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let anki = FakeAnki::start(1);
        anki_at(&mut app, &anki.url);
        screenshots_on(&mut app);
        place_a_popup(&mut app);

        app.handle_request("anki-add", Verb::parse("anki-add"));
        let shot = parked(&mut app);
        app.shot = Some(Shot::Grabbing(shot));
        app.handle_shot(Err("this compositor advertises no capture protocol".to_string()));
        let written =
            pump_until(&mut event_loop, &mut app, &log_file, "anki: card added", 60);

        assert!(written.contains("screenshot: the grab failed"), "log was: {written}");
        let seen = anki.seen();
        assert_eq!(1, seen.len(), "the card still goes in: {seen:?}");
        assert!(seen[0]["params"]["note"].get("picture").is_none(), "{:?}", seen[0]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A save that cannot land must still answer the popup: `start_add`
    /// marked it adding before the picture was authorised, so a silent
    /// failure here would leave "Adding…" on screen for good.
    ///
    /// The folder is blocked by a *file* of the same name, which is the
    /// one way to make `create_dir_all` fail without root.
    #[test]
    fn a_screenshot_that_cannot_be_saved_still_clears_the_popup() {
        let dir = scratch("shotnosave");
        let log_file = dir.join("chibipop.log");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let anki = FakeAnki::start(1);
        anki_at(&mut app, &anki.url);
        screenshots_on(&mut app);
        std::fs::write(dir.join("blocked"), b"not a directory").unwrap();
        app.config.actions.screenshot.save_dir =
            dir.join("blocked").display().to_string();
        place_a_popup(&mut app);

        app.handle_request("anki-add", Verb::parse("anki-add"));
        let shot = parked(&mut app);
        app.shot = Some(Shot::Grabbing(shot));
        app.handle_shot(Ok(test_frame()));
        let written = pump_until(&mut event_loop, &mut app, &log_file, "screenshot: the", 60);

        assert!(
            written.contains("screenshot: the picture never landed"),
            "log was: {written}"
        );
        assert!(anki.seen().is_empty(), "the save comes first, so nothing was filed");
        let state = app.controller.anki().expect("still shown");
        assert!(!state.adding, "the popup must not be left adding");
        assert!(state.failed, "and it says the add failed, which is what the user sees");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The `screenshot` verb: the mining context for the lookup on
    /// screen, filed on a card of its own. Ungated - `include_on_add` is
    /// off here and irrelevant, which is why core has a second entry
    /// point (`shot::plan`).
    #[test]
    fn the_screenshot_verb_files_the_mining_context_for_the_lookup_on_screen() {
        let dir = scratch("shotverb");
        let log_file = dir.join("chibipop.log");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let anki = FakeAnki::start(1);
        anki_at(&mut app, &anki.url);
        screenshots_on(&mut app);
        app.config.actions.screenshot.include_on_add = false;
        let generation = place_a_popup(&mut app).expect("anki is on, so a dupe check was ordered");
        // What a served dupe check leaves behind: the popup's own view
        // of AnkiConnect, which is what decides whether a card is filed.
        app.feed(Event::DupesChecked { generation, dupes: Some(HashSet::new()) });
        assert!(app.controller.anki().expect("shown").connected);

        app.handle_request("screenshot", Verb::parse("screenshot"));

        let shot = parked(&mut app);
        assert_eq!(ShotKind::Mining { files_a_card: true }, shot.kind);
        let stem =
            shot.plan.path.file_stem().expect("a stem").to_string_lossy().into_owned();
        app.shot = Some(Shot::Grabbing(shot));
        app.handle_shot(Ok(test_frame()));
        let written =
            pump_until(&mut event_loop, &mut app, &log_file, "anki: card added", 60);

        let seen = anki.seen();
        assert_eq!(1, seen.len(), "one press, one card: {seen:?}");
        assert_eq!(
            Some(format!("chibipop-screenshot-{stem}.png").as_str()),
            seen[0]["params"]["note"]["picture"][0]["filename"].as_str(),
            "the mining picture rides the card: {seen:?}"
        );
        assert!(
            written.contains("control: screenshot - picking the mining screenshot's region"),
            "the socket logs what it was asked for: {written}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// AnkiConnect not serving this popup is not a reason to lose the
    /// picture: it goes to disk with no card to ride on, and nothing
    /// claims the word was added.
    #[test]
    fn a_mining_screenshot_with_no_card_to_file_still_saves_the_picture() {
        let dir = scratch("shotnocard");
        let log_file = dir.join("chibipop.log");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let anki = FakeAnki::start(1);
        anki_at(&mut app, &anki.url);
        screenshots_on(&mut app);
        let generation = place_a_popup(&mut app).expect("anki is on, so a dupe check was ordered");
        // A fresh popup is optimistically `connected` while Anki is
        // enabled (`AnkiPopupState::fresh`), so what makes it *not*
        // serving is a dupe check that came back with nothing - which is
        // exactly what AnkiConnect being down looks like from here.
        app.feed(Event::DupesChecked { generation, dupes: None });
        assert!(!app.controller.anki().expect("shown").connected);

        app.handle_request("screenshot", Verb::parse("screenshot"));

        let shot = parked(&mut app);
        assert_eq!(ShotKind::Mining { files_a_card: false }, shot.kind);
        let png = shot.plan.path.clone();
        app.shot = Some(Shot::Grabbing(shot));
        app.handle_shot(Ok(test_frame()));
        let written = pump_until(&mut event_loop, &mut app, &log_file, "screenshot: saved", 60);

        assert!(png.is_file(), "the PNG is at {}", png.display());
        assert!(
            written.contains("screenshot: saved to") && written.contains("no card to file it on"),
            "log was: {written}"
        );
        assert!(anki.seen().is_empty(), "nothing was asked of AnkiConnect: {:?}", anki.seen());
        assert!(
            !app.controller.anki().expect("shown").added.contains(WORD),
            "and no add lifecycle was faked for a card that does not exist"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A verb pressed with nothing on screen does something honest.
    ///
    /// Windows' `is_available` gate is the same (a visible popup with a
    /// card), but it fails silently; on a compositor bind there is no
    /// dialog and no return code a user ever sees, so the line in the log
    /// is the whole diagnosis.
    #[test]
    fn the_screenshot_verb_with_nothing_on_screen_says_why() {
        let dir = scratch("shotnopopup");
        let log_file = dir.join("chibipop.log");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let anki = FakeAnki::start(1);
        anki_at(&mut app, &anki.url);
        screenshots_on(&mut app);

        app.handle_request("screenshot", Verb::parse("screenshot"));
        let written = pump_until(&mut event_loop, &mut app, &log_file, "anki: ", 8);

        assert!(app.shot.is_none(), "nothing to plan, so nothing parked");
        assert!(
            written.contains("screenshot: nothing to file"),
            "and it says so rather than failing silently: {written}"
        );
        assert!(anki.seen().is_empty(), "no popup, no card: {:?}", anki.seen());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One pick at a time, which the `Option<Shot>` slot is: two verbs
    /// can arrive in one socket callback, and the second must not drag a
    /// box over the dim the first put up. The add still files its card.
    #[test]
    fn a_second_shot_is_refused_while_one_is_already_owed() {
        let dir = scratch("shotbusy");
        let log_file = dir.join("chibipop.log");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let anki = FakeAnki::start(2);
        anki_at(&mut app, &anki.url);
        screenshots_on(&mut app);
        let generation = place_a_popup(&mut app).expect("anki is on, so a dupe check was ordered");
        app.feed(Event::DupesChecked { generation, dupes: Some(HashSet::new()) });

        // The add parks first; the mining verb arrives before the pump
        // has had its idle pass.
        app.handle_request("anki-add", Verb::parse("anki-add"));
        app.handle_request("screenshot", Verb::parse("screenshot"));

        assert!(matches!(app.shot, Some(Shot::Parked(_))), "the first one keeps the slot");
        let written = pump_until(&mut event_loop, &mut app, &log_file, "anki: card added", 60);
        assert!(
            written.contains("screenshot: a region pick is already owed"),
            "log was: {written}"
        );
        assert_eq!(1, anki.seen().len(), "one card, from the add that owned the slot");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
