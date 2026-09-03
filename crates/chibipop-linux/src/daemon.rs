//! The daemon owns the calloop pump, instance lock, control socket, and log.
//! ARCHITECTURE.md#workspace-and-seams needs synchronous code with the
//! calloop Linux pump.
//! The daemon owns popup layer surfaces and the first capture-channel stage.
//! This stage selects a capture backend from the capture ladder.
//! If it selects the portal rung, consent must finish before the daemon reports
//! channel state. The daemon then connects the OCR channel to this pump.

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
use crate::signals;
use crate::trigger::{self, Hold};
use crate::wayland;
use crate::worker;
use anyhow::{bail, Context, Result};
use calloop::generic::Generic;
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
use wayland_client::backend::protocol::ProtocolError;
use wayland_client::backend::WaylandError;
use wayland_client::delegate_dispatch;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_registry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::globals::registry_queue_init;
use wayland_client::{Connection, Dispatch, DispatchError, EventQueue, QueueHandle};
use wayland_protocols::ext::image_capture_source::v1::client::ext_image_capture_source_v1::ExtImageCaptureSourceV1;
use wayland_protocols::ext::image_capture_source::v1::client::ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_cursor_session_v1::ExtImageCopyCaptureCursorSessionV1;
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::ZxdgOutputV1;

/// Controller tick length. Linux has no dispatch timer.
/// ARCHITECTURE.md#hover-cadence defines event-paced hover with Worker throttle.
/// The Controller uses this value to derive alert arithmetic from ticks.
const DISPATCH_TICK_MS: u32 = 20;

/// The environment hook for the surface probe and its pick deadline.
/// See [`App::probe_surfaces`].
const SURFACE_PROBE_ENV: &str = "CHIBIPOP_SURFACE_PROBE";
const SURFACE_PROBE_DEADLINE: Duration = Duration::from_millis(400);

/// The demo anchor box before the first cursor sample.
/// The canned popup needs a position before that sample.
const DEMO_ANCHOR: PhysRect = PhysRect { x: 200, y: 200, w: 120, h: 32 };

/// Shared state for the pump.
///
/// This type is `pub(crate)` because popup Wayland dispatch targets it
/// (`popup::surface`).
/// SCTK 0.21 uses a blanket impl that cannot access this state.
/// Keep the impls that pass events beside their code. They use the accessors below.
pub(crate) struct App {
    log: Log,
    stub: StubState,
    /// Directories that this daemon reads or writes.
    /// The daemon resolves them once at startup.
    /// They contain published trigger state, the config file for reload, and
    /// the folder for mined pictures (`Paths::screenshots_dir`).
    paths: Paths,
    /// Loaded Config. A reload rebuilds Worker data from the same source that
    /// the Controller reads.
    config: chibipop::config::Config,
    signal: LoopSignal,
    /// ProtocolError that ended this session, if any.
    /// This error cannot recover. The Wayland source stops the pump like a signal
    /// and stores the error here.
    /// [`run`] reads it after orderly shutdown and returns a non-zero exit.
    fatal: Option<ProtocolError>,
    /// The Wayland side of the cursor channel.
    cursor: CursorState,
    /// Cursor Events and the trigger verbs drive the Controller. The
    /// code below runs the Controller Commands.
    controller: Controller,
    /// CHIBIPOP_CURSOR_TRACE=1 logs every sample and poll interval.
    /// Last hyprctl sample in logical units. Move detection reads this value.
    trace: bool,
    last_poll: Option<(i32, i32)>,
    /// Newest cursor sample in global physical units. A press reads this point.
    last_cursor: Option<PhysPoint>,
    /// Cursor rung for this session. A press reads the rung to decide whether it
    /// can request a new sample. If it cannot, the press uses the newest Event.
    cursor_rung: Option<cursor::Rung>,
    /// Time when the hyprctl rung last saw cursor motion.
    last_move: Instant,
    /// At most one Settings child (ARCHITECTURE.md#settings-and-config).
    /// The tray Settings item starts it. The settings-scoped flock guards
    /// cross-process use. This field guards the daemon.
    settings: SettingsChild,
    /// Channel health and the SNI tray that mirrors it
    /// (ARCHITECTURE.md#platform-integration).
    /// This field also stores the daemon view. It works when no tray exists.
    tray: TrayHandle,
    /// Popup layer surfaces. This field is `None` when no compositor accepts a
    /// bind.
    /// A unit test and a session without layer shell use this state.
    /// The daemon still runs in both cases.
    popup: Option<Popup>,
    /// Region-selector layer surfaces. This field is `None` without layer shell.
    /// The cause matches the popup shell cause. Absence is a state, not an error.
    selector: Option<Selector>,
    /// One pick in flight while the nested pump of [`Selector::pick`] holds the
    /// thread.
    /// This field stays here because SCTK handlers target `App`, not `Selector`.
    /// The pump dispatches `&mut App` into those handlers.
    pick: Option<Pick>,
    /// Click-through, frame-only layer surfaces for scan rects.
    /// They outline the area that this hover captured and the word that it
    /// defined (`Command::ShowScanOverlay`).
    /// This field is `None` for the selector reason.
    ///
    /// The static-region border uses a second `Outline` because the two objects
    /// have separate lifetimes.
    /// A hover repaint must not erase a static region.
    /// Windows uses two windows for the same reason (`ui/overlay.rs`,
    /// `ui/static_overlay.rs`).
    scan_outline: Option<Outline>,
    /// Border for the static sentence region. It is the other half of the pair
    /// and matches `ui/static_overlay.rs`.
    /// [`static_overlay_region`] decides when to show or hide it.
    /// One predicate gives every call site the same border rule.
    static_outline: Option<Outline>,
    /// `CHIBIPOP_POPUP_DEMO=1` makes trigger verbs show and hide the canned
    /// popup without a lookup.
    /// A developer can inspect the surface without a Dictionary.
    demo: Demo,
    /// A scripted pointer pass runs now (`CHIBIPOP_POINTER_SCRIPT`).
    /// Repaints from its steps must not start a second pass.
    scripting: bool,
    /// Core pipeline. It runs capture, OCR, and Dictionary on its own thread
    /// (ARCHITECTURE.md#workspace-and-seams).
    /// This field is `None` if the build fails.
    /// The state covers an absent capture protocol, OCR models, or portal consent.
    /// The daemon still runs and reports the cause.
    worker: Option<Worker>,
    /// Japanese word-boundary analysis for the shown Card.
    ///
    /// The service loads its model lazily and uses the Worker wake source,
    /// so analysis never blocks the calloop pump.
    analysis: chibipop::analysis::Service,
    /// Data that a spawn needs. A granted portal retry gives a new session to a
    /// new pipeline from this data.
    worker_setup: worker::Setup,
    /// Wake that the Worker thread pings after it queues a result.
    worker_ping: calloop::ping::Ping,
    /// Channel for an AnkiConnect answer.
    /// Each call sends an HTTP request and waits on its own thread.
    /// The pump receives the answer as an Event, like a Worker result
    /// (ARCHITECTURE.md#workspace-and-seams).
    anki_tx: calloop::channel::Sender<AnkiOutcome>,
    /// Channel for pixels from a mined region.
    /// Its shape and rationale match `anki_tx`.
    /// A capture backend open and a frame grab both block.
    /// A thread does both tasks, then sends an Event.
    shot_tx: calloop::channel::Sender<Result<Frame, String>>,
    /// The one screenshot in flight, if one exists (see [`Shot`]).
    shot: Option<Shot>,
    /// One-off OCR queue for the Worker's thread-affine engine and its wake.
    /// Each respawn rebuilds this field because each wake belongs to one Worker.
    /// When no pipeline exists, this field is
    /// [`worker::OcrJobs::disconnected`].
    /// A copy then fails at once instead of a long delay.
    ocr_jobs: worker::OcrJobs,
    /// Channel for lines from a one-off OCR job.
    /// Each request keeps a clone, and the answer arrives as an Event on this
    /// pump.
    /// The Worker thread never waits for this pump
    /// (ARCHITECTURE.md#workspace-and-seams).
    ocr_tx: calloop::channel::Sender<Result<Vec<OcrLine>, String>>,
    /// Region that a queued OCR job reads while the job runs.
    /// The answer diagnostic names this region.
    /// This field also limits each key press to one queued job.
    ocr_job: Option<PhysRect>,
    /// Writable selection. This field is `None` when a compositor advertises no
    /// data-control protocol.
    /// Stock GNOME has this state. The bind discovers an absent layer shell.
    /// This absence affects only this action.
    clipboard: Option<clipboard::Clipboard>,
    /// Dictionary identities last reported by the pipeline.
    dicts: Vec<DictInfo>,
    /// Trigger-mode hold while the user holds one
    /// (ARCHITECTURE.md#hover-cadence).
    hold: Option<Hold>,
    /// Last lookup failure that the daemon logged.
    /// This record limits each repeated line to one log entry as the cursor
    /// moves.
    last_warning: Option<String>,
    /// True when a portal session serves pixels.
    /// The backend lives on the Worker thread, so retry reads this flag and
    /// does not access the backend.
    portal_serving: bool,
    /// Rung that the ladder chose. `reload` reads it to decide if retry has
    /// purpose.
    capture_selection: capture_backend::Selection,
    /// Extra text for the Capture row beside the backend name.
    /// The daemon sets this text when the compositor paints the pointer into
    /// OCR frames.
    /// The daemon probes this option once at startup because only a compositor
    /// reload can change it.
    /// Later Capture transitions keep the text, so a portal retry cannot remove
    /// the defect from the row.
    pointer_defect: Option<String>,
    /// Every value that the portal retry needs to run again from here.
    portal_retry: Option<PortalRetry>,
    /// Handle that receives new sources.
    /// The dwell watch is the only source that this daemon adds or removes at
    /// runtime.
    /// The decision state must therefore reach the pump handle.
    pump: LoopHandle<'static, App>,
    /// Dwell re-check timer while it is armed
    /// (ARCHITECTURE.md#hover-cadence).
    dwell: Option<RegistrationToken>,
}

/// One popup job. It blocks the thread that runs it.
enum AnkiCall {
    Dupes { generation: u64, exprs: Vec<String> },
    Add { expr: String, fields: HashMap<String, String> },
    /// Pixels of a mined region. The job encodes pixels, writes the PNG, and
    /// files the card that points to it.
    /// It runs here instead of on the grab thread because file work calls
    /// AnkiConnect.
    /// It reads the same `[anki]` snapshot as other calls.
    /// It also encodes here because deflate for a 4K region is not pump work.
    Shot { plan: chibipop::shot::ShotPlan, bgra: Vec<u8>, w: i32, h: i32, files_a_card: bool },
}

/// One answer that the pump receives.
///
/// Failures travel as text. The pump thread owns the log, so no code prints a
/// failure at its source.
enum AnkiOutcome {
    /// `Err` means that AnkiConnect refused the request or does not run.
    Dupes { generation: u64, dupes: Result<HashSet<String>, String> },
    Added { expr: String, note: Result<i64, String> },
    /// Complete answer for a mined picture.
    /// `Ok(Some(note))` means that the picture was saved and filed.
    /// `Ok(None)` means that it was saved without a card.
    /// `Err` means that a step failed.
    /// `dir` is the folder that received the file, not the file itself.
    /// The filename carries the word. The word is screen content, so
    /// diagnostics must not hold it (ARCHITECTURE.md#platform-integration).
    Shot { expr: String, dir: PathBuf, filed: Result<Option<i64>, String> },
}

/// Complete life of one screenshot on this side of the seam.
///
/// These two states represent the two waits for a shot. They are exclusive.
/// [`App::drain_shot`] takes the parked plan, picks a region on the pump thread,
/// and gives the plan to the grab state.
/// Therefore `Option<Shot>` also enforces one pick at a time
/// (see [`App::park_shot`]).
enum Shot {
    /// The user approved this shot. The shot waits for a region that the user
    /// must drag.
    ///
    /// The `AddNote` arm of [`App::execute`] or `Verb::Screenshot` parks this
    /// state.
    /// The pump's top level drains it.
    /// The Windows bin uses the same rule at the bottom of its message loop
    /// (`crates/chibipop-windows/src/app.rs`).
    /// A region pick uses a nested pump. A nested pump inside a command batch
    /// re-enters Controller dispatch before the batch ends.
    Parked(Pending),
    /// The region is picked. A separate thread grabs its pixels.
    /// The popup stays off screen until the pixels arrive because an earlier
    /// popup could appear in them.
    Grabbing(Pending),
}

/// Plan for one screenshot and the feature that requested it.
struct Pending {
    /// Core owns every file and note rule (`chibipop::shot`).
    /// This bin picks a region and grabs pixels.
    plan: chibipop::shot::ShotPlan,
    kind: ShotKind,
}

/// Screenshot feature that owns a plan. Two features exist.
///
/// This enum chooses the no-picture result and whether the daemon asks
/// AnkiConnect for a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShotKind {
    /// `actions.screenshot.include_on_add`. The Controller put the popup in the
    /// add state.
    /// The daemon must file the card in both cases. A pick that returns nothing
    /// still files the card without a picture.
    Add,
    /// The mining screenshot (`actions.screenshot` and
    /// `MiningContextScreenshot` on Windows). No code waits for it.
    /// `files_a_card` records the popup's AnkiConnect state when the verb
    /// arrives.
    /// A false value still writes the PNG without a card.
    Mining { files_a_card: bool },
}

impl ShotKind {
    /// True when this shot sends pixels to AnkiConnect.
    /// An add always sends them. `plan_add` answers only for a popup with a
    /// card. The old plain add also sent them.
    fn files_a_card(self) -> bool {
        match self {
            ShotKind::Add => true,
            ShotKind::Mining { files_a_card } => files_a_card,
        }
    }
}

impl AnkiCall {
    /// Network part of the call. It runs outside the pump.
    fn run(self, anki: &chibipop::config::AnkiConfig) -> AnkiOutcome {
        match self {
            AnkiCall::Dupes { generation, exprs } => {
                let refs: Vec<&str> = exprs.iter().map(String::as_str).collect();
                let dupes = chibipop::anki::find_duplicates(
                    &anki.url,
                    &anki.deck,
                    &anki.model,
                    &refs,
                    &anki.field_map,
                );
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
            // Core owns the filename, the `source = "screenshot"` field lookup,
            // the base64 payload, and the AnkiConnect call.
            // This arm calls three core functions and makes no decisions.
            AnkiCall::Shot { plan, bgra, w, h, files_a_card } => {
                let filed = (|| -> Result<Option<i64>> {
                    let png = chibipop::image::encode_bgra_to_png(&bgra, w, h)?;
                    if files_a_card {
                        return chibipop::shot::save_and_add(&png, &plan, anki).map(Some);
                    }
                    // Anki cannot take a card. The daemon still writes the
                    // picture without a card.
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

/// Values that a second consent request needs. Retry uses the startup path.
struct PortalRetry {
    state_dir: PathBuf,
    globals: Vec<wayland::Advertised>,
    /// `Some` only when the cursor ladder chose rung 2.
    /// A retry must not restore a rung that the ladder did not choose.
    cursor: Option<portal::CursorSink>,
}

/// Eager portal consent from start to finish.
///
/// This function returns the backend when the portal accepts consent.
/// It returns the Capture channel row in both outcomes.
/// A refusal stores a retry state. A refusal never exits or panics.
fn open_portal(retry: &PortalRetry, log: &mut Log) -> (Option<PortalCapture>, ChannelState) {
    // Monitors must accept an anchor before the daemon publishes the tray.
    // The pump does not exist yet, so this code uses a temporary probe.
    let outputs = image_copy::probe_geometry(&retry.globals);
    // The layout origin stays until the first grab moves the point.
    // The connected stream tracks the region that the code reads
    // (`PortalCapture::grab`).
    // The startup guess selects only the initial monitor.
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

    /// Apply one Verb from either channel.
    /// The control socket is rung 2 and the GlobalShortcuts portal is rung 1.
    /// Both channels call this method. A Portal press and
    /// `chibipop ctl trigger-down` produce the same result.
    /// `Verb::AnkiAdd` is the only keyboard path to AnkiConnect.
    /// The socket `anki-add` and Portal `anki-add` shortcut therefore match.
    fn apply_verb(&mut self, verb: Verb) {
        match verb {
            Verb::Reload => self.reload_config(),
            // The canned popup replaces a lookup, so a machine without a
            // Dictionary can inspect the surface.
            Verb::TriggerDown | Verb::Toggle if self.demo.armed => self.demo_show(),
            Verb::TriggerUp if self.demo.armed => self.hide_popup(),
            Verb::TriggerDown => self.trigger(trigger::down(self.hold)),
            Verb::TriggerUp => self.trigger(trigger::up(self.hold)),
            Verb::Toggle => self.trigger(trigger::toggle(self.hold)),
            // The in-panel Anki slot raises the same Event, so every card path
            // uses one AnkiConnect flow.
            Verb::AnkiAdd => self.feed(Event::AddRequested),
            // Native channel only, like `static-region` below.
            Verb::Screenshot => self.mining_screenshot(),
            // Native channel only.
            Verb::OcrClipboard => self.ocr_to_clipboard(),
            // Native channel only. No portal id exists for this action, so the
            // socket provides the complete global channel.
            Verb::StaticRegion => self.pick_static_region(),
        }
    }

    /// One Event from the GlobalShortcuts session thread.
    ///
    /// The Portal adds a source for socket presses. It does not replace the
    /// socket.
    /// Every press therefore uses [`App::apply_verb`].
    /// This method records the binding owner, reported key, and settings state.
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
            // This rung does not serve requests. The socket does.
            // This state has a reason and never ends the daemon.
            shortcuts::Event::Unavailable { reason, advice } => {
                self.log.diag(&format!("trigger: portal rung unavailable - {reason}"));
                // The row gets one short clause
                // (ARCHITECTURE.md#platform-integration: one line).
                // The log stores advice, where it has room.
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

    /// The portal answered `BindShortcuts`, or the user changed a key in the
    /// desktop UI (`ShortcutsChanged`).
    /// Both cases log a line, update the trigger row, and update the file that
    /// the settings window reads.
    fn trigger_bound(&mut self, what: &str, bindings: Vec<shortcuts::Binding>) {
        let detail = shortcuts::portal_detail(&bindings);
        self.log.diag(&format!("trigger: portal {what} - {detail}"));
        self.note_channel(ChannelId::Trigger, ChannelState::up(detail));
        self.publish_trigger(&shortcuts::state::Published::portal(bindings));
    }

    /// Tell the settings window which channel owns the binding
    /// (ARCHITECTURE.md#settings-and-config).
    /// A write error logs a diagnostic. The trigger still works.
    fn publish_trigger(&mut self, published: &shortcuts::state::Published) {
        if let Err(e) = shortcuts::state::publish(&self.paths.state_dir, published) {
            self.log.diag(&format!("trigger: could not publish the channel state - {e}"));
        }
    }

    /// One trigger Verb effect (ARCHITECTURE.md#hover-cadence).
    ///
    /// A press freezes the output under the cursor and looks up text there.
    /// A release drops the frame and retracts the popup.
    /// Code sends the grab to the Worker before the lookup.
    /// The Worker serves its queue in order, so the grab predates the popup by
    /// rule, not by assumption.
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
                // The press supplies the first cursor sample. The first lookup
                // needs no cursor motion.
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

    /// Take the press-time grab of `output` on the Worker's thread.
    fn freeze_at(&mut self, at: PhysPoint, output: PhysRect) {
        let Some(worker) = self.worker.as_ref() else {
            self.log.diag(
                "trigger: no pipeline - a lookup needs capture, OCR models and a dictionary",
            );
            return;
        };
        // Freeze has no answer, so no result uses its id.
        // A failed grab appears in later lookup results.
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

    /// Drop the frozen frame for a hold. Grabs use live pixels again.
    fn thaw(&mut self) {
        let Some(worker) = self.worker.as_ref() else { return };
        let _ = worker.trigger().send(Trigger { kind: TriggerKind::Thaw, id: RequestId(0) });
        self.log.diag("trigger: hold released, frozen grab dropped");
    }

    /// Get the cursor position now for a press.
    ///
    /// The Hyprctl-poll rung receives a direct request.
    /// A press can arrive at the slowest adaptive interval
    /// (ARCHITECTURE.md#hover-cadence).
    /// A position read costs little. Event rungs already sent their newest
    /// sample.
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

    /// Output box that contains `at`.
    /// The daemon uses capture backend arithmetic, so both records use the same
    /// box.
    fn output_containing(&self, at: PhysPoint) -> PhysRect {
        crate::capture::geometry::bounds_containing(&self.cursor.geometries(), at)
    }

    /// Popup used by Wayland dispatch. `run` always builds it.
    /// A compositor without layer shell still gets a Popup without a shell that
    /// can paint.
    /// A bind failure for `wl_compositor` or `wl_shm` stops startup before
    /// handlers run.
    /// Tests use `Option` to build App without compositor state.
    pub(crate) fn popup_mut(&mut self) -> &mut Popup {
        self.popup.as_mut().expect("a popup dispatch arrived with no popup bound")
    }

    /// Return true when a popup can put a panel on screen.
    /// Stock GNOME returns false. Draw paths use this result instead of
    /// assumptions.
    pub(crate) fn popup_can_draw(&self) -> bool {
        self.popup.as_ref().is_some_and(Popup::available)
    }

    /// Return the selection state that the popup may render.
    ///
    /// The layout must receive no selection when Anki is disabled, even if
    /// an old Controller surface still has selection data.
    fn popup_selection(&self) -> Option<chibipop::select::Selections> {
        if self.config.anki.enabled {
            self.controller.selection().cloned()
        } else {
            None
        }
    }

    /// Re-run text hit testing after a drag repaint.
    fn refresh_drag_text(&mut self) {
        let interaction = self.popup.as_mut().and_then(Popup::drag_move);
        if let Some(interaction) = interaction {
            self.pointer_interactions(vec![interaction]);
        }
    }

    /// Move popup diagnostics into the log. The popup owns no log, so this
    /// thread does.
    pub(crate) fn flush_popup_notes(&mut self) {
        let Some(popup) = self.popup.as_mut() else { return };
        for line in popup.drain_notes() {
            self.log.diag(&line);
        }
    }
    // ---- surfaces beside the popup ----
    //
    // Three layer-surface kinds use this state. Shared SCTK handlers route each
    // event by surface identity.
    // They do not send events to the popup and hope for a match.
    // The popup still receives every event for one of its panels and no other
    // event.

    pub(crate) fn selector_mut(&mut self) -> Option<&mut Selector> {
        self.selector.as_mut()
    }

    /// Connection that a pick uses for its own queue, plus the daemon queue
    /// handle for its wake.
    pub(crate) fn selector_handles(&self) -> Result<(Connection, QueueHandle<App>)> {
        let selector = self
            .selector
            .as_ref()
            .context("this compositor advertises no layer shell, so there is nothing to drag on")?;
        Ok(selector.handles())
    }

    /// One diagnostic from the pick code.
    pub(crate) fn selector_note(&mut self, line: String) {
        match self.selector.as_mut() {
            Some(selector) => selector.note(line),
            // No selector exists, but the line remains true.
            None => self.log.diag(&line),
        }
    }

    /// Move selector, pick, and outline diagnostics into the log.
    /// This matches `flush_popup_notes`. None of these objects owns the log.
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

    /// Outputs that the selector and outline need.
    /// Read them from the popup so all three surfaces share one `OutputState`
    /// and one global physical pixel space.
    pub(crate) fn screens(&self) -> Vec<popup::Screen> {
        self.popup.as_ref().map(Popup::screens).unwrap_or_default()
    }

    /// Seat for the pointer and keyboard that a pick creates.
    pub(crate) fn seat(&mut self) -> Option<WlSeat> {
        self.popup.as_mut()?.seats().seats().next()
    }

    // ---- lifecycle of one pick, driven by `select::Selector::pick` ----

    pub(crate) fn pick_start(&mut self, pick: Pick) {
        self.pick = Some(pick);
    }

    pub(crate) fn pick_arm(&mut self, signal: calloop::LoopSignal) {
        if let Some(pick) = self.pick.as_mut() {
            pick.arm(signal);
        }
    }

    /// One pump iteration handles one drag motion per compositor frame.
    /// One commit per frame matches popup cadence from frame callbacks.
    /// A decided pick leaves the loop here (see [`Pick::tick`]).
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

    /// Remove the selector and return the number of surfaces removed.
    pub(crate) fn pick_finish(&mut self) -> usize {
        self.flush_surface_notes();
        self.pick.take().map_or(0, Pick::destroy)
    }

    /// Drag a region on the dimmed screen. This function blocks until the user
    /// decides.
    /// `None` means cancel, a drag below threshold, or no selector.
    /// Each outcome is a state, not an error.
    ///
    /// The popup stays hidden while the pick runs.
    /// It must not enter the pixels that a caller grabs after the pick.
    /// The selector remains modal.
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

    // ---- static sentence region ----

    /// `static-region`: draw the box that
    /// [`chibipop::config::SentenceMode::Static`] uses for the Anki sentence.
    ///
    /// This action works in every sentence mode by design and matches Windows
    /// (`crates/chibipop-windows/src/app.rs`'s slot-1 hotkey).
    /// The box lets a user switch to Static.
    /// A check for Static first would block that switch.
    /// The box appears at once when the predicate allows it.
    ///
    /// The method hides the old border first.
    /// It shares `Layer::Overlay` with the selector.
    /// Without this step, the old box could cover the new drag.
    fn pick_static_region(&mut self) {
        if let Some(outline) = self.static_outline.as_mut() {
            outline.hide();
        }
        // `None` uses the product deadline (`select::PICK_TIMEOUT`).
        // A second constant would conflict with the product pick duration.
        let picked = self.pick_region(None);
        self.took_static_region(picked);
    }

    /// Interpret a finished pick.
    /// `None` means cancel, a drag below threshold, expiry, or no layer shell.
    /// It leaves memory and file state unchanged.
    /// It also leaves the border where the predicate places it.
    ///
    /// This split keeps nested pump work in [`App::pick_static_region`] and
    /// keeps pure state here.
    /// Tests drive this state seam.
    fn took_static_region(&mut self, picked: Option<PhysRect>) {
        let Some(rect) = picked else {
            self.log.diag("static region: pick cancelled - nothing changed");
            self.sync_static_outline();
            return;
        };
        self.config.anki.static_region = Some([rect.x, rect.y, rect.w, rect.h]);
        // The config file is the sole source of truth
        // (ARCHITECTURE.md#settings-and-config).
        // Save the region before the daemon derives any other state.
        // TOML is small, and this call already blocks on the user drag.
        // A write error logs a diagnostic. The current region stays until the
        // next reload.
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
        // The Controller handles this with `RequestReload`.
        // It carries fresh `WorkerSettings` and the new region into the
        // pipeline.
        // This matches `reload_config` without another file read.
        let cfg = controller_config(&self.config);
        self.feed(Event::ConfigReloaded(Box::new(cfg)));
        self.sync_static_outline();
    }

    /// Place the static-region border where [`static_overlay_region`] needs
    /// it, or hide it.
    ///
    /// One function serves startup, config reload, and region updates.
    /// It evaluates the three conditions once, so call sites cannot diverge.
    /// This matches the Windows bin rule in
    /// `LiveSettings::static_overlay_region`.
    fn sync_static_outline(&mut self) {
        let wanted = static_overlay_region(&self.config);
        let screens = self.screens();
        let Some(outline) = self.static_outline.as_mut() else {
            // No layer shell means no border.
            // Report this reduced state once when the config asks for a border.
            if wanted.is_some() {
                self.log.diag(
                    "static region: no outline on this compositor (no zwlr_layer_shell_v1), \
                     so the border cannot be drawn; the region itself still serves lookups",
                );
            }
            return;
        };
        let was = outline.marks().first().map(|m| m.rect);
        // Send the desired state every time, not only after a change.
        // A closed pane can remove a surface while the desired state stays the
        // same.
        // Reassert the state to restore it. Gate only the log because Line
        // mode has nothing to report.
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

    // ---- Mining screenshot and picture for an add ----

    /// Resolve `actions.screenshot.save_dir`.
    /// Absolute values stay absolute. Relative values use the executable
    /// directory in Portable mode and the XDG data directory otherwise.
    fn screenshots_dir(&self) -> PathBuf {
        self.paths.screenshots_dir(&self.config.actions.screenshot.save_dir)
    }

    /// Return the picture for the add that the Controller approved, or `None`
    /// if no picture applies.
    ///
    /// Core owns this decision (`chibipop::shot::plan_add`).
    /// It checks `include_on_add`, the blank-expression and already-added
    /// guards, the filename, and the picture field.
    /// This function supplies the popup, config, and clock.
    fn plan_shot_for_add(&self) -> Option<chibipop::shot::ShotPlan> {
        let view = self.controller.popup()?;
        chibipop::shot::plan_add(
            &view,
            &self.config,
            &self.screenshots_dir(),
            chibipop::shot::epoch_secs(),
        )
    }

    /// `screenshot`: grab a region and save it as the Mining context for the
    /// on-screen lookup (`MiningContextScreenshot` on Windows).
    ///
    /// Windows uses `popup_visible` and a top card as its gate
    /// (`action/screenshot.rs::is_available`).
    /// If either is absent, Windows does nothing.
    /// This method uses the same gate because the picture belongs to the
    /// on-screen word.
    /// It logs the reason because a compositor bind has no dialog or visible
    /// return code.
    fn mining_screenshot(&mut self) {
        let planned = self.controller.popup().map(|view| {
            (
                view.presentation.top.is_some(),
                // This plan has no add gate. The Mining screenshot saves a
                // picture regardless of the popup add state.
                // It therefore skips `plan_add` guards and `include_on_add`.
                chibipop::shot::plan(
                    &view,
                    &self.config,
                    &self.screenshots_dir(),
                    chibipop::shot::epoch_secs(),
                ),
                // Popup AnkiConnect state. False still writes the PNG without
                // a card.
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

    /// Park a plan and ask the pump to drain it after this batch.
    ///
    /// The idle callback runs after calloop finishes dispatch.
    /// This matches the Windows message-loop seam.
    /// A pick uses a nested pump. A nested pump inside a command batch would
    /// re-enter Controller dispatch before the batch ends.
    fn park_shot(&mut self, shot: Pending) {
        if self.shot.is_some() {
            // Two picks cannot share the screen. The second would use the dim
            // from the first.
            // The control socket drains all queued connections in one callback,
            // so two `chibipop ctl` presses can arrive together.
            self.log
                .diag("screenshot: a region pick is already owed, so this one is refused");
            self.shot_without_picture(shot, "another pick is in flight");
            return;
        }
        self.shot = Some(Shot::Parked(shot));
        self.pump.insert_idle(|app: &mut App| app.drain_shot());
    }

    /// OS part of a parked shot: hide, drag, and grab.
    ///
    /// [`App::pick_region`] hides the popup.
    /// The popup stays down until pixels arrive, so it does not enter them.
    fn drain_shot(&mut self) {
        let Some(Shot::Parked(shot)) = self.shot.take() else { return };
        // `None` uses the product deadline (`select::PICK_TIMEOUT`), the same
        // as `static-region`.
        // A second constant would create a second pick duration.
        let picked = self.pick_region(None);
        self.took_shot_region(picked, shot);
    }

    /// Interpret a completed pick.
    /// Split from [`App::drain_shot`] for the same reason as
    /// `took_static_region` and `pick_static_region`.
    /// The pick uses a nested pump and needs a compositor.
    /// This part holds pure state for daemon tests.
    fn took_shot_region(&mut self, picked: Option<PhysRect>, shot: Pending) {
        match picked {
            Some(region) => self.spawn_shot(region, shot),
            None => {
                self.restore_popup();
                self.shot_without_picture(shot, "no region was picked");
            }
        }
    }

    /// Grab an arbitrary rect on its own thread.
    ///
    /// Keep the plan here.
    /// A failed `Builder::spawn` drops its closure and could leave the popup
    /// at "Adding…" forever.
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

    /// Data for a one-shot grab to open its own backend.
    /// It includes the startup capability probe, the selected capture rung,
    /// and the state dir for the portal restore token.
    /// A second rung-2 session can stay silent without a new prompt.
    fn capture_setup(&self) -> capture::Setup {
        capture::Setup {
            globals: self.worker_setup.globals.clone(),
            backend: self.worker_setup.backend,
            state_dir: self.paths.state_dir.clone(),
        }
    }

    /// The grab thread returned. Restore the popup here.
    /// Send pixels to the file-and-card call on another thread
    /// (ARCHITECTURE.md#workspace-and-seams).
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
            // Windows uses the same rule: a failed grab yields a card without
            // a picture.
            Err(e) => {
                self.log.diag(&format!("screenshot: the grab failed - {e}"));
                self.shot_without_picture(shot, "the grab failed");
            }
        }
    }

    /// Shot with no picture: cancel, expiry, or failed grab.
    ///
    /// An add still files its card. `start_add` marked the popup in the add
    /// state before it approved the picture.
    /// This dispatch alone clears "Adding…". A Mining screenshot has no caller
    /// and ends here.
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

    /// Restore the popup after a pick or grab hides it.
    ///
    /// The pump can receive a newer popup while the grab runs.
    /// If a popup exists, keep it. If the Controller hides it, do not restore it.
    /// The Controller owns Anki state because the slot is part of the panel.
    /// A new raster therefore uses the current state.
    fn restore_popup(&mut self) {
        if self.popup.as_ref().and_then(Popup::shown).is_some() || !self.controller.is_shown() {
            return;
        }
        let Some(req) = self.popup.as_ref().and_then(Popup::request).cloned() else {
            return;
        };
        self.show_popup(&ShowRequest {
            anki: self.controller.anki().cloned(),
            selection: self.popup_selection(),
            ..req
        });
    }

    // ---- OCR to clipboard ----

    /// `ocr-clipboard`: pick a region, read it, and copy the selection.
    ///
    /// Check both refusal cases before the pick.
    /// A compositor without data-control has no clipboard that chibipop can
    /// write.
    /// A region that still waits for the recognizer owns the answer channel.
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
        // `None` uses the product deadline (`select::PICK_TIMEOUT`).
        // A second constant would conflict with the product pick duration.
        let picked = self.pick_region(None);
        self.took_ocr_region(picked);
    }

    /// Interpret a completed pick.
    ///
    /// Split from [`App::ocr_to_clipboard`] for the same reason as
    /// [`App::took_static_region`] and `pick_static_region`.
    /// The pick uses a nested pump and needs a compositor.
    /// This part holds state for daemon tests.
    fn took_ocr_region(&mut self, picked: Option<PhysRect>) {
        let Some(region) = picked else {
            self.log.diag("ocr-clipboard: pick cancelled - the clipboard is untouched");
            self.restore_popup();
            return;
        };
        self.ocr_job = Some(region);
        self.spawn_ocr_read(region);
    }

    /// Grab the region and send its pixels to the Worker engine.
    ///
    /// Two stages run off the pump, one thread each.
    /// The grab opens its own backend. The thread-affine recognizer stays on
    /// the Worker (ARCHITECTURE.md#ocr-engine).
    /// The grab thread sends the frame to [`worker::OcrJobs`].
    /// The pump does not copy the frame.
    /// Only text returns here as an Event
    /// (ARCHITECTURE.md#workspace-and-seams).
    fn spawn_ocr_read(&mut self, region: PhysRect) {
        let setup = self.capture_setup();
        let jobs = self.ocr_jobs.clone();
        let answer = self.ocr_tx.clone();
        let spawned = std::thread::Builder::new()
            .name("chibipop-ocr-clip".to_string())
            .spawn(move || {
                // Use native resolution. This adapter never upscales.
                // meikiocr runs worse on 2x crops in every benchmark slice
                // (ARCHITECTURE.md#ocr-engine).
                // The Windows twin uses 2x because its engine needs it.
                match capture::oneshot(&setup, region) {
                    Ok(frame) => {
                        let request = worker::OcrRequest {
                            bgra: frame.buf,
                            w: frame.w,
                            h: frame.h,
                            answer: answer.clone(),
                        };
                        // A queue without a pipeline returns an error here.
                        // No job can remain in the queue forever.
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

    /// The recognizer returned. Join lines and copy the selection.
    ///
    /// Core owns this rule for line joins (`chibipop::text::layout::join_lines`).
    /// The Windows action uses it too, so both platforms copy the same text.
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
        // Log counts, not text. The text is screen content, and diagnostics do
        // not use lookup opt-in (ARCHITECTURE.md#platform-integration).
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

    /// `CHIBIPOP_SURFACE_PROBE=1`: show the outline and region selector once at
    /// startup, then report their results.
    ///
    /// This has the same role as `CHIBIPOP_POPUP_DEMO` for the popup and
    /// `capture-dump` for the capture ladder.
    /// These surfaces need a compositor, so this probe maps, paints, and removes
    /// them against a real compositor.
    /// It creates no seat input. The pick reaches its deadline and exercises the
    /// timeout guard.
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

        // Place two boxes inside the output for repeatable runs.
        // Use one wide box and one square box with separate positions.
        // Send them through the shipped consumer, not directly to `Outline::show`.
        // This matches hover behavior with two kinds and theme colors, each
        // outside the border (`Command::ShowScanOverlay`).
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
                // Outline surfaces use the daemon queue.
                // A round trip returns configures and puts the strips on screen.
                // Without it, the probe would only show sent requests.
                if let Err(e) = queue.roundtrip(self) {
                    self.log.diag(&format!("probe: outline round trip failed: {e}"));
                }
                self.flush_surface_notes();
                self.log.diag(&format!("probe: outline shown on {count} surface(s)"));
            }
            None => self.log.diag("probe: no outline on this compositor"),
        }

        // Leave one pick to expire without seat input.
        // This tests map, configure, paint, nested pump, deadline, and teardown
        // without input from the user's pointer.
        let picked = self.pick_region(Some(SURFACE_PROBE_DEADLINE));
        self.log.diag(&format!("probe: pick answered {}", picked.is_some()));

        // An empty vector hides on both platforms (`controller.rs`).
        // The probe uses the same path as popup retraction.
        self.execute(Command::ShowScanOverlay { rects: Vec::new() });
        if let Some(left) = self.scan_outline.as_ref().map(|o| o.marks().len()) {
            if let Err(e) = queue.roundtrip(self) {
                self.log.diag(&format!("probe: outline round trip failed: {e}"));
            }
            self.flush_surface_notes();
            self.log.diag(&format!("probe: outline hidden, {left} rect(s) left"));
        }
    }

    // ---- routes ----

    /// A layer surface was configured. Surface identity selects the owner.
    /// The pick paints the dim, an outline paints strips, and the popup paints
    /// the panel.
    pub(crate) fn layer_configured(&mut self, layer: &LayerSurface, size: (u32, u32)) {
        if self.pick.as_ref().is_some_and(|p| p.owns_layer(layer)) {
            if let Some(pick) = self.pick.as_mut() {
                pick.configured(layer, size);
            }
            // The dim must appear before the drag starts.
            // The pump repaint takes one iteration.
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
            // A resize paints here instead of in `show`, so a scripted pointer
            // pass finds its frame here.
            self.refresh_drag_text();
            self.run_pointer_script();
        }
    }

    /// The compositor closed a layer surface.
    /// The popup recreates it, the pick cancels, and the outline loses one
    /// surface until the next show.
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

    /// A `wl_surface.frame` callback. Only the popup requests one.
    /// Popup commits use the refresh rate, so ignore other owners.
    pub(crate) fn surface_frame(&mut self, surface: &WlSurface) {
        if !self.popup.as_ref().is_some_and(|p| p.owns(surface)) {
            return;
        }
        self.popup_mut().frame_done(surface);
        self.flush_popup_notes();
        self.refresh_drag_text();
        self.run_pointer_script();
    }

    /// One `wl_pointer` frame. The pick owns the pointer while active because
    /// its surfaces cover the output.
    /// The popup receives frames that do not belong to the pick.
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

    /// A `preferred_scale` Event arrived. The daemon does not latch scale.
    /// Hyprland can send 1.0 and then correct it.
    /// A change on the shown surface triggers re-raster and re-place.
    /// The Controller receives the new rect.
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
            Some(Ok(Some(placed))) => {
                self.placed(placed);
                self.refresh_drag_text();
            }
            Some(Err(e)) => self.log.diag(&format!("popup: re-render failed: {e:#}")),
            _ => {}
        }
    }

    /// Popup-local pointer input becomes Controller Events.
    ///
    /// Wayland has no global wheel or pointer channel. These Events come only
    /// from the popup input region or its implicit pointer grab.
    pub(crate) fn pointer_interactions(&mut self, interactions: Vec<popup::Interaction>) {
        for interaction in interactions {
            match interaction {
                popup::Interaction::Scroll { notches } => {
                    self.log.diag(&format!("pointer: wheel {notches:+} notch(es) over the panel"));
                    self.feed(Event::Scrolled { notches });
                }
                popup::Interaction::Down { local, button, hit, text } => {
                    self.log.diag(&format!(
                        "pointer: {button:?} down at {},{} -> {}",
                        local.x,
                        local.y,
                        match &hit {
                            Some(hit) => format!("{hit:?}"),
                            None => "no target".to_string(),
                        }
                    ));
                    self.feed(Event::PointerDown { local, button, hit, text });
                }
                popup::Interaction::Move { local, text } => {
                    self.feed(Event::PointerMoved { local, text });
                }
                popup::Interaction::Up { local, button } => {
                    self.feed(Event::PointerUp { local, button });
                }
                // Core reserves the slot and the painter fills it.
                // The Controller decides whether the press becomes an add.
                popup::Interaction::Anki { local } => {
                    self.log.diag(&format!(
                        "pointer: primary down at panel {},{} -> the Anki slot",
                        local.x, local.y
                    ));
                    self.feed(Event::AddRequested);
                }
            }
        }
    }

    /// Scripted pointer passes (`CHIBIPOP_POINTER_SCRIPT`) start here, not
    /// inside the popup.
    ///
    /// Each step must reach the Controller and return as a repaint before the
    /// next step resolves.
    /// Otherwise a scroll followed by a click would use the replaced frame.
    /// This method runs after a show, configure, or frame callback because any
    /// path can raster.
    /// Re-entrant calls return at once. The loop then takes the pass they arm.
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

    /// One scripted step through the same entry points as a real
    /// `wl_pointer` frame.
    fn pointer_step(&mut self, panel: usize, step: popup::Step) {
        if self.popup.is_none() {
            return;
        }
        let mut interactions = Vec::new();
        match step {
            popup::Step::Enter(x, y) => {
                self.log.diag(&format!("pointer: script enter at {x},{y} logical"));
                self.popup_mut().pointer_enter(panel, (x, y), None);
            }
            popup::Step::Motion(x, y) => {
                self.popup_mut().pointer_motion((x, y));
                let at = self.popup_mut().hit_at((x, y));
                self.log.diag(&format!("pointer: script motion at {x},{y} logical -> {at}"));
            }
            popup::Step::Click(x, y) => {
                self.log.diag(&format!("pointer: script click at {x},{y} logical"));
                if let Some(interaction) = self.popup_mut().pointer_button((x, y)) {
                    interactions.push(interaction);
                }
                if let Some(interaction) =
                    self.popup_mut().pointer_release((x, y), chibipop::controller::Button::Primary)
                {
                    interactions.push(interaction);
                }
            }
            popup::Step::Wheel(value120) => {
                self.log.diag(&format!("pointer: script wheel value120 {value120}"));
                if let Some(interaction) = self.popup_mut().pointer_wheel_120(value120) {
                    interactions.push(interaction);
                }
            }
            popup::Step::Leave => {
                self.popup_mut().pointer_leave(panel);
            }
            popup::Step::Dump => {
                self.popup_mut().dump_hits();
            }
        }
        self.flush_popup_notes();
        self.pointer_interactions(interactions);
    }

    /// Send one Event through the Controller and run each Command it returns.
    /// Then sync the dwell watch with the current screen
    /// (ARCHITECTURE.md#hover-cadence).
    fn feed(&mut self, event: Event) {
        for cmd in self.controller.handle(event) {
            self.execute(cmd);
        }
        self.sync_dwell();
    }

    /// Return whether the dwell re-check has something to watch now.
    fn dwell_wanted(&self) -> bool {
        dwell_wanted(self.hold, self.controller.dwell_armed())
    }

    /// Arm the dwell watch when a target exists.
    ///
    /// [`App::dwell_tick`] removes the timer.
    /// A source cannot remove itself while dispatch runs.
    /// When the watch expires, the idle daemon has no timed source. Event
    /// cursor rungs then cause no idle wakeups.
    fn sync_dwell(&mut self) {
        if self.dwell.is_some() || !self.dwell_wanted() {
            return;
        }
        self.arm_dwell();
    }

    /// Create one dwell watch from now.
    /// `sync_dwell` owns the decision. Tests arm one by hand because a popup
    /// with a rect needs a compositor.
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

    /// At each dwell deadline, ask the shown popup's question, then decide
    /// whether the watch remains (ARCHITECTURE.md#hover-cadence).
    ///
    /// The re-grab races damage at this deadline below the seams.
    /// An unchanged screen needs no copy or OCR. The Controller returns no new
    /// presentation.
    /// Only a change reaches the popup.
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
            // OCR must not read our popup while a live grab runs.
            // A frozen hold predates the popup (ARCHITECTURE.md#capture-and-masking).
            // Wayland lacks surface exclusion, so this mask is the complete
            // mechanism.
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
            // New content needs one scripted pass from this frame.
            // Do not arm a pass for `RepaintPopup`. A scroll repaint would
            // otherwise call itself.
            Command::ShowPopup { presentation, anchor, scroll, show_back } => {
                if let Some(popup) = self.popup.as_mut() {
                    popup.arm_script();
                }
                self.show_popup(&ShowRequest {
                    presentation: *presentation,
                    anchor,
                    scroll,
                    show_back,
                    // The slot is part of the panel, not a separate Windows
                    // window. Every raster carries its state.
                    anki: self.controller.anki().cloned(),
                    selection: self.popup_selection(),
                });
            }
            Command::RepaintPopup { scroll, show_back } => {
                let anki = self.controller.anki().cloned();
                let selection = self.popup_selection();
                let req = self.popup.as_ref().and_then(Popup::request).map(|req| ShowRequest {
                    scroll,
                    show_back,
                    anki,
                    selection,
                    ..req.clone()
                });
                if let Some(req) = req {
                    self.show_popup(&req);
                }
            }
            Command::RequestAnalysis { generation, texts } => {
                self.analysis.request(generation, texts);
            }
            Command::SetDragging(dragging) => {
                if let Some(popup) = self.popup.as_mut() {
                    popup.set_dragging(dragging);
                }
            }
            // A glossary citation uses desktop `xdg-open` to choose a browser.
            // `layout::link_action` allow-lists `http` and `https` because the
            // URL comes from a dictionary file.
            // Run it detached and unmasked like the settings child.
            // The daemon blocks SIGINT/SIGTERM in its threads. A mask would
            // outlive `exec`.
            Command::OpenUrl(url) => self.open_url(&url),
            Command::HidePopup => self.hide_popup(),
            // Two settings use this path:
            // `debug.show_scan_region` shows capture boxes.
            // `popup.highlight_match` shows the matched word.
            // The `matched` field otherwise only feeds hold-region arithmetic.
            // An empty vector hides the overlay.
            Command::ShowScanOverlay { rects } => self.show_scan_overlay(&rects),
            // A fresh popup replaces the old one.
            // The sub-notch delta belongs to the old entry and must not move
            // the new popup.
            Command::DiscardScroll => {
                if let Some(popup) = self.popup.as_mut() {
                    popup.discard_scroll();
                }
            }
            // Screen content is written only after user opt-in.
            Command::LogLookup { headword, match_len } => {
                self.log.lookup(&format!("{headword}  match={match_len}"));
            }
            // When the cursor crosses unsupported text, one line could repeat
            // for each sample.
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
            // Screenshot-on-add seam. A plan carries a picture, so do not
            // dispatch the plain add.
            // The picture call files the card. OS work stays in
            // [`App::park_shot`].
            Command::AddNote { expr, fields } => match self.plan_shot_for_add() {
                Some(plan) => self.park_shot(Pending { plan, kind: ShotKind::Add }),
                None => self.spawn_anki(AnkiCall::Add { expr, fields }),
            },
            // Rows that arm (`Set*Armed`, `SetCursorShape`) come from the Windows
            // dispatch tick.
            // This daemon has no such tick or seat hook, so no row is armed per
            // tick.
            other => self.log.diag(&format!("controller: {other:?} (no-op)")),
        }
    }

    /// Send one Trigger to the pipeline, or log that none exists.
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

    /// Relate lookup pixels to the popup in time.
    /// A hold reads the press-time frame. Other modes read the current screen.
    fn capture_mode(&self) -> CaptureMode {
        match self.hold {
            Some(_) => CaptureMode::Frozen,
            None => CaptureMode::Live,
        }
    }

    /// Drain the Worker and Japanese analysis results. Only the freshest queued
    /// result from each service matters because newer requests supersede older
    /// requests before they reach the event loop.
    fn drain_results(&mut self) {
        let mut freshest = None;
        if let Some(worker) = self.worker.as_ref() {
            while let Ok(result) = worker.results().try_recv() {
                freshest = Some(result);
            }
        }
        let mut freshest_analysis = None;
        while let Ok(result) = self.analysis.results().try_recv() {
            freshest_analysis = Some(result);
        }
        if let Some(result) = freshest {
            self.feed(Event::LookupResult { id: result.id, outcome: result.outcome });
        }
        if let Some((generation, words)) = freshest_analysis {
            self.feed(Event::AnalysisReady { generation, words });
        }
    }

    /// One AnkiConnect call outside the pump.
    ///
    /// Each request blocks in `ureq` while it waits for a server.
    /// The server can be absent, so this call must stay off the pump.
    /// A two-second connect timeout would freeze the popup for two seconds.
    /// The answer returns as an Event, like the Worker's path
    /// (ARCHITECTURE.md#workspace-and-seams).
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

    /// One AnkiConnect answer on the pump thread.
    ///
    /// Lines carry counts and note IDs, not the expression.
    /// Screen content stays private because the user did not opt in to
    /// diagnostics (ARCHITECTURE.md#platform-integration).
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
                // `None` means a picture without a card.
                // No add lifecycle closes, and no code can claim that it filed
                // the word.
                // Other outcomes answer the popup because `start_add` marked
                // it in the add state before it allowed the picture.
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

    /// The Anki slot state changed.
    ///
    /// Windows uses a separate button window for place, hide, and repaint.
    /// Here the slot belongs to the panel.
    /// Each paint already carries the current state.
    /// This method catches a state change after the last raster.
    fn sync_anki_slot(&mut self) {
        // If no popup is shown, no state needs sync.
        // `HidePopup` retracts it. Never show the request still on the surface
        // from here.
        let Some(want) = self.controller.anki().cloned() else { return };
        let req = self.popup.as_ref().and_then(Popup::request);
        let Some(req) = req.filter(|req| req.anki.as_ref() != Some(&want)).cloned() else {
            return;
        };
        self.show_popup(&ShowRequest {
            anki: Some(want),
            selection: self.popup_selection(),
            ..req
        });
    }

    /// Measure, place, raster, and commit. Then tell the Controller where the
    /// surface landed.
    /// The bin owns the measurer, so this round trip gives the Controller a
    /// rect it cannot compute.
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
                // A same-size show rasters at once, so the scripted pass
                // already has its frame.
                self.refresh_drag_text();
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

    /// Hide with a transparent buffer, not an unmap.
    /// Hyprland then does not fire a layer animation, so hide stays instant.
    ///
    /// Hide the scan outline too. Its boxes belong to the popup that shows the
    /// answer.
    /// The boxes would outline a word that no longer has a definition.
    /// Windows uses the same overlay path (`app.rs`'s `Command::HidePopup`).
    /// This also covers the Linux-only caller, [`App::pick_region`], which must
    /// not leave an old frame under the region selector.
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

    /// Show what this hover captured and box the word it defined.
    ///
    /// Linux half of `Command::ShowScanOverlay`: core selects the rects.
    /// `debug.show_scan_region` supplies capture boxes.
    /// `popup.highlight_match` supplies the `Match` rect.
    /// If both settings are off, the vector is empty.
    /// This method selects the appearance.
    /// The popup theme supplies colors, so the outline follows the panel theme
    /// without a second palette.
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

    /// The canned popup (`CHIBIPOP_POPUP_DEMO=1`) follows the lookup path:
    /// measure, place, commit, and `PopupPlaced`.
    fn demo_show(&mut self) {
        let anchor = self.demo.anchor.or_else(|| self.cursor_anchor()).unwrap_or(DEMO_ANCHOR);
        self.log.diag(&format!(
            "popup: demo show at anchor {},{} {}x{}",
            anchor.x, anchor.y, anchor.w, anchor.h
        ));
        // The demo bypasses the Controller. It arms the scripted pointer pass
        // itself.
        if let Some(popup) = self.popup.as_mut() {
            popup.arm_script();
        }
        self.show_popup(&ShowRequest {
            presentation: popup::canned(),
            anchor,
            scroll: 0,
            show_back: false,
            // The demo requests the slot so the panel paints the Anki slot for
            // inspection.
            // Production requests it only after an AnkiConnect answer.
            anki: Some(chibipop::present::AnkiPopupState {
                enabled: true,
                connected: true,
                ..chibipop::present::AnkiPopupState::disabled()
            }),
            selection: self.popup_selection(),
        });
    }

    /// Return the last cursor sample as an anchor box, so the demo popup lands
    /// where a lookup would land.
    fn cursor_anchor(&mut self) -> Option<PhysRect> {
        let (lx, ly) = self.last_poll?;
        let pos = self.cursor.logical_to_global(f64::from(lx), f64::from(ly))?;
        Some(PhysRect { x: pos.x, y: pos.y, w: DEMO_ANCHOR.w, h: DEMO_ANCHOR.h })
    }

    /// One tray message, handled on the daemon thread.
    /// The log, settings guard, and loop signal live there
    /// (ARCHITECTURE.md#platform-integration).
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

    /// The compositor reports a verdict for this connection.
    /// Log once, then stop as the tray Quit and signal source do.
    ///
    /// A protocol error cannot recover.
    /// The server already destroyed the named object, so later requests on this
    /// connection are invalid.
    /// Do not retry or degrade. End the pump and set the exit status.
    /// Guard this path because the error stays visible at every wakeup.
    /// The log must not repeat. The old loop wrote ~8 MB of stderr in 300 s.
    fn end_on_protocol_error(&mut self, err: &ProtocolError) {
        if self.fatal.is_some() {
            return;
        }
        // The compositor supplies the message. Only the pure Rust backend gives
        // it here.
        // libwayland keeps its own log line and leaves this field empty.
        // Name it only when the message exists.
        self.log.diag(&format!(
            "wayland: protocol error on {}#{} - code {}{}{} - the connection is dead, shutting down",
            err.object_interface,
            err.object_id,
            err.code,
            if err.message.is_empty() { "" } else { ": " },
            err.message,
        ));
        self.fatal = Some(err.clone());
        self.signal.stop();
    }

    /// Record a channel change in the registry, tray rows, and SNI status.
    /// Write one log line only after a change.
    ///
    /// A known software cursor stays on the Capture row.
    /// The pixel backend can serve pixels while the pointer remains there.
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

    /// Start the settings window unless a child already exists.
    /// The tray Settings item calls this.
    /// The daemon guard covers this process, and the settings-scoped flock
    /// covers other processes.
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

    /// Give a glossary citation to the desktop browser.
    ///
    /// Do not wait for `xdg-open`.
    /// Drop its stdio so browser output does not reach daemon output.
    /// Reap the child with `wait` at the next citation, not with SIGCHLD setup.
    /// The settings guard makes the same choice for its one short-lived child.
    fn open_url(&mut self, url: &str) {
        let mut command = std::process::Command::new("xdg-open");
        command
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        crate::signals::unmasked(&mut command);
        match command.spawn() {
            Ok(mut child) => {
                self.log.diag(&format!("link: opened {url} as pid {}", child.id()));
                // A browser that is already active exits at once.
                // Another browser can outlive this call and stays with the
                // session.
                let _ = child.try_wait();
            }
            Err(e) => self.log.diag(&format!("link: xdg-open failed: {e}")),
        }
    }

    /// `reload` reads the file again and applies daemon settings.
    /// It updates the lookup-log gate and the popup settings.
    /// `popup.layer` needs no surface recreation, so it acts as a runtime
    /// toggle.
    /// A denied portal does not exit. Hover shows one error state with in-app
    /// retry, so reload tries portal consent again.
    /// The config file is the only source of truth
    /// (ARCHITECTURE.md#settings-and-config). Nothing structured crosses the
    /// socket.
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
                // The Controller answers with `RequestReload`.
                // This sends new settings to the Worker and reopens the
                // dictionary after a rebuild rename.
                self.config = config;
                let cfg = controller_config(&self.config);
                self.feed(Event::ConfigReloaded(Box::new(cfg)));
                // The mode, checkbox, and region are editable in the settings
                // window.
                // Reload is the second of the predicate's three call sites.
                // A switch from Static removes the border.
                self.sync_static_outline();
            }
            Err(e) => self.log.diag(&format!("config: reload failed: {e:#}")),
        }
    }

    /// Build or rebuild the core pipeline on its own thread.
    ///
    /// Drop the old handle first to close the old thread.
    /// Its trigger channel then closes, and `recv` returns.
    /// If the pipeline build fails, log it and keep the daemon alive.
    /// The cursor, tray, settings, and popup remain available.
    fn spawn_worker(&mut self, portal: Option<PortalCapture>) {
        self.worker = None;
        // Until spawn succeeds, the queue has no pipeline.
        // An `ocr-clipboard` press while the pipeline is absent fails with a reason,
        // rather than wait on a dead thread.
        // The old queue sender goes away with it.
        // A job that the dead Worker never read cannot reach the new one.
        self.ocr_jobs = worker::OcrJobs::disconnected();
        let settings = worker::settings(&self.config, &self.dicts);
        // Resolve settings against the identities already held.
        // The first spawn has none. See `rescope_lookups`.
        let sent_scope = settings.present_cfg.clone();
        let started = Instant::now();
        // Create a fresh queue for each spawn.
        // The wake nudge belongs to one Worker's trigger channel.
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

    /// The terms list after the dictionary identities become known.
    ///
    /// `Config::present_config` resolves terms against installed dictionary
    /// names.
    /// The config names enabled dictionaries and includes each installed
    /// dictionary that appears in neither array.
    /// The pipeline's first read supplies those names.
    /// Settings sent to `spawn_worker` use an empty library, so they select no
    /// dictionary.
    /// Send the real answer now.
    /// A fresh daemon must honor the config value from its first lookup, not its
    /// first reload.
    /// A respawn normally leaves the answer unchanged, so it needs no log.
    fn rescope_lookups(&mut self, sent: &chibipop::present::PresentConfig) {
        let settings = worker::settings(&self.config, &self.dicts);
        if settings.present_cfg == *sent {
            return;
        }
        self.log.diag(&format!(
            "worker: {} searches {} of {} dictionary/ies",
            self.config.ocr.language,
            settings.present_cfg.terms.len(),
            self.dicts.len(),
        ));
        self.send_trigger(TriggerKind::Reload(Box::new(settings)), RequestId(0));
    }

    /// Ask for one lookup at the cursor's current position, if known.
    ///
    /// Event rungs provide a position when their session opens and on movement
    /// (ARCHITECTURE.md#input-ladders).
    /// If the daemon starts with the cursor on a word, it has one sample but no
    /// pipeline.
    /// This method asks when the pipeline becomes available.
    /// It makes live mode true at once, like the trigger press's first cursor
    /// sample (ARCHITECTURE.md#hover-cadence).
    fn look_where_the_cursor_is(&mut self) {
        let Some(pos) = self.last_cursor else { return };
        self.log.diag(&format!("lookup: asking where the cursor already is ({}, {})", pos.x, pos.y));
        self.feed(Event::CursorMoved { pos });
    }

    /// Retry portal consent when in-app retry requests it.
    ///
    /// Do this only when the ladder picks the Portal and the backend does not
    /// serve pixels.
    /// Do not tear down or prompt a granted session after a config
    /// edit.
    /// Both the Settings window and a shell command use `reload`.
    /// Keep `reload` as the minimal verb set.
    fn retry_portal_capture(&mut self) {
        if self.portal_serving || self.capture_selection.backend() != Some(Backend::Portal) {
            return;
        }
        let Some(retry) = self.portal_retry.take() else { return };
        self.log.diag("capture: retrying the portal consent (reload)");
        let (capture, state) = open_portal(&retry, &mut self.log);
        self.portal_retry = Some(retry);
        self.note_channel(ChannelId::Capture, state);
        // The Worker reads through the session.
        // A granted retry therefore goes to a fresh pipeline.
        if capture.is_some() {
            self.portal_serving = true;
            self.spawn_worker(capture);
        }
    }

    /// One `hyprctl cursorpos` poll tick: sample, send a change through the
    /// seam, and arm the next interval (ARCHITECTURE.md#hover-cadence).
    ///
    /// If the sample fails, the daemon can observe channel failure
    /// live.
    /// The compositor or `hyprctl` can stop.
    /// Report either result to the status registry.
    /// The tray Cursor row and NeedsAttention then track it at runtime.
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

/// The only seam: channel -> `Event::CursorMoved` -> Controller.
/// Both rungs arrive here with global physical pixels.
impl CursorHandler for App {
    fn cursor(&mut self) -> &mut CursorState {
        &mut self.cursor
    }

    fn on_cursor_position(&mut self, pos: PhysPoint) {
        if self.trace {
            self.log.diag(&format!("cursor: ({}, {})", pos.x, pos.y));
        }
        self.last_cursor = Some(pos);
        // When a hold crosses outputs, it needs a fresh full grab on the entered
        // output (ARCHITECTURE.md#hover-cadence).
        // Take it before the lookup that sees the output change.
        // Do it here, not behind the Controller.
        if let Some(hold) = self.hold {
            if let Some(output) = trigger::regrab(hold, &self.cursor.geometries(), pos) {
                self.log.diag("trigger: the cursor crossed onto another output");
                self.freeze_at(pos, output);
                self.hold = Some(Hold { output, ..hold });
            }
        }
        // A sample without a pipeline would use the Controller movement gate
        // for a lookup that nobody can answer.
        // It would report one error for each sample.
        // Keep the newest position. `look_where_the_cursor_is` uses it when a
        // pipeline exists.
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
        roles: config.popup.render_settings().roles,
        edge_autoscroll: config.popup.edge_autoscroll,
        primary_additive: config.anki.selection_buttons
            == chibipop::config::SelectionButtons::PrimaryAdditive,
        separator: config.anki.selection_separator.into(),
    }
}

/// Return the static-region border when it belongs on screen.
///
/// Conditions are Static sentence mode, an enabled outline, and a region that
/// the user drew.
///
/// Windows uses `LiveSettings::static_overlay_region` for the same reason.
/// Startup, config reload, and a fresh region all call this method.
/// Keep the condition in one place.
/// A mode other than Static returns `None`, which removes the border.
fn static_overlay_region(config: &chibipop::config::Config) -> Option<PhysRect> {
    if config.anki.sentence_mode != chibipop::config::SentenceMode::Static
        || !config.anki.show_static_overlay
    {
        return None;
    }
    worker::static_region(&config.anki)
}

/// Return whether the dwell re-check has a target
/// (ARCHITECTURE.md#hover-cadence).
///
/// The method uses two conditions.
/// `armed` belongs to the Controller: live mode, a popup rect, and no
/// drill-down.
/// `hold` belongs to the daemon because only it knows the frozen grab.
/// Hold pixels predate the popup and cannot change, so trigger mode needs no
/// re-check.
fn dwell_wanted(hold: Option<Hold>, armed: bool) -> bool {
    hold.is_none() && armed
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// Registry events use a long-lived queue.
/// The startup report prints the full table, so log later changes only.
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

/// The daemon queue runs on the pump.
/// The daemon dispatches it instead of the callback from
/// `WaylandSource::insert`.
///
/// `insert` gives the queue `EventQueue::dispatch_pending` and stops there.
/// This call cannot report a protocol error because `wayland-client` hides it.
/// Its `dispatching_impl` reads the backend with
/// `dispatch_inner_queue().unwrap_or_default()` and drops the error on purpose.
/// A compositor that kills the connection leaves a readable socket, `Ok(0)` on
/// each wakeup, and a full CPU.
/// Ask the connection for its sticky error here.
/// This fixes the issue with less code than manual queue dispatch.
/// The source keeps its read guard, flush, and `before_sleep`.
/// These remain necessary when a connection has more than one queue.
fn insert_wayland_source(
    pump: &LoopHandle<'static, App>,
    conn: &Connection,
    queue: EventQueue<App>,
) -> Result<()> {
    let watch = conn.clone();
    pump.insert_source(WaylandSource::new(conn.clone(), queue), move |_, queue, app: &mut App| {
        let dispatched = queue.dispatch_pending(app);
        let fatal = match &dispatched {
            // No work remains for handlers.
            // This is every drain end and every wakeup after a protocol error.
            // Check `last_error` once per wakeup.
            // The dispatch above has just taken the mutex repeatedly.
            Ok(0) => watch.protocol_error(),
            Ok(_) => None,
            Err(e) => fatal_protocol_error(e).cloned(),
        };
        match fatal {
            // The daemon owns the failure.
            // Return `Ok(0)` so the source does not log another error or turn it
            // into calloop's opaque `Protocol error`.
            // The pump already stopped, so this is the final dispatch.
            Some(err) => {
                app.end_on_protocol_error(&err);
                Ok(0)
            }
            None => dispatched,
        }
    })
    .map_err(|e| anyhow::anyhow!("registering the Wayland source: {e}"))?;
    Ok(())
}

/// End only on protocol errors.
///
/// Other dispatch failures stay the source's business.
/// This keeps a `WouldBlock` flush or a slow compositor nonfatal.
/// It also prevents duplicate output when the source already treats a malformed
/// message as fatal.
fn fatal_protocol_error(err: &DispatchError) -> Option<&ProtocolError> {
    match err {
        DispatchError::Backend(WaylandError::Protocol(err)) => Some(err),
        DispatchError::Backend(WaylandError::Io(_)) | DispatchError::BadMessage { .. } => None,
    }
}

/// Implement `AsFd` so the socket registers with calloop.
struct Listening(ControlSocket);

impl AsFd for Listening {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.listener().as_fd()
    }
}

/// The AnkiConnect answer channel on the pump.
///
/// Use one helper for both call sites. Tests build an `App` too.
/// An answer that reaches no `Event` would make the add lifecycle
/// (add, added, failed) impossible to test.
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

/// The selected region's pixel channel on the pump.
///
/// This is the twin of `anki_channel`.
/// The grab runs on its own thread, and tests build an `App` too.
/// A grab that reaches no `App` method would make the screenshot flow
/// impossible to test.
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

/// The one-off OCR job's answer channel on the pump.
///
/// This is the twin of `shot_channel`, one stage later.
/// The recognizer runs on the Worker's thread
/// (ARCHITECTURE.md#ocr-engine). The engine is thread-affine.
/// Its lines arrive as an Event, not as a blocked pump.
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

/// The clipboard thread's diagnostic channel on the pump.
///
/// The offer lives on a connection and its own thread (`clipboard`).
/// The log lives here (ARCHITECTURE.md#platform-integration).
/// Its messages travel as text, like an AnkiConnect failure.
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

pub fn run(paths: Paths) -> Result<()> {
    // Block shutdown signals before any other process work.
    // Later work can spawn threads, and the mask inherits at spawn
    // (`signals::block_shutdown`).
    // Register the source near the other event sources.
    // The signal mask starts at this line.
    let signals = signals::block_shutdown()?;

    let display = wayland::display_name()?;
    let runtime_dir = paths.runtime_dir()?;

    // Acquire the lock first.
    // A second launch must stop before it touches the current log.
    // `Log::open` truncates the file.
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

    // Choose the capture backend first.
    // Cursor ladder rung 2 exists only when Portal serves pixels, and it uses
    // the same stream.
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
    // Neither backend can exclude the pointer that the compositor painted into
    // its framebuffer.
    // State this before OCR can read arrows as glyphs.
    // This applies to both backends. The Portal copies through the same
    // framebuffer.
    let pointer_in_frames = software_cursor::probe();
    if let Some(line) = pointer_in_frames.startup_line() {
        log.diag(&line);
    }
    let pointer_defect = pointer_in_frames.row_defect();
    let portal_metadata = capture_selection.backend() == Some(Backend::Portal)
        && portal::cursor_metadata_available();

    // The cursor channel uses one rung by advertised capability
    // (ARCHITECTURE.md#input-ladders).
    // Otherwise log a diagnostic with the absent capability.
    // Keep the daemon up either way.
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

    // Rung 2 samples arrive on the PipeWire thread and must reach the pump like
    // all Events.
    // Use a bounded calloop channel.
    // A metadata burst cannot grow the queue without limit, and the daemon
    // stays synchronous (ARCHITECTURE.md#workspace-and-seams).
    let (cursor_tx, cursor_rx) = calloop::channel::sync_channel::<PhysPoint>(64);
    let cursor_sink: Option<portal::CursorSink> =
        if selection == cursor::Selection::Rung(cursor::Rung::PortalMetadata) {
            let tx = cursor_tx.clone();
            Some(std::sync::Arc::new(move |p: PhysPoint| {
                // A full queue means the pump is behind on cursor Events.
                // Drop the sample. Do not block the stream thread.
                let _ = tx.send(p);
            }))
        } else {
            None
        };

    // Ask for portal consent early.
    // The dialog belongs to the launch context, not a hover.
    // Publish the channel row before the tray exists.
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

    // Trigger channel ladder (ARCHITECTURE.md#input-ladders).
    // The socket listens as rung 2, so decide only whether to ask the
    // GlobalShortcuts portal to carry the two shortcuts too.
    // Its session uses its own thread. Events arrive here, so the pump stays
    // synchronous (ARCHITECTURE.md#workspace-and-seams).
    let (trigger_override, trigger_warning) = shortcuts::ChannelOverride::from_env();
    if let Some(w) = &trigger_warning {
        log.diag(w);
    }
    let trigger_selection = shortcuts::select(shortcuts::portal::probe(), trigger_override);
    // The advice text gives a bind command for the user.
    // Name this binary because PATH can lack bare `chibipop`.
    log.diag(&trigger_selection.startup_line(&crate::paths::exec_name()));
    // Until the portal answers, publish the native state: the compositor owns
    // the key.
    // The settings window must show this.
    // Do not let a stale portal binding survive a previous run.
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

    // The SNI tray (ARCHITECTURE.md#platform-integration) uses its own
    // D-Bus thread. Its activations arrive here as `TrayRequest`s, so the
    // pump stays sync.
    // `spawn` reports diagnostics instead of errors, so a trayless session is
    // normal (stock GNOME, bare Hyprland) and costs nothing.
    // The registry is the daemon's view of channel health, with or without a
    // tray.
    let (tray_tx, tray_rx) = calloop::channel::channel::<TrayRequest>();
    let mut statuses = ChannelStatuses::startup(
        match &pointer_defect {
            Some(defect) => capture_state.degraded_by(defect),
            None => capture_state,
        },
        &selection,
        tray::status::popup_state(wayland::popup_shell_advertised(&globals)),
    );
    // The trigger row starts with the always-bound socket.
    // The ladder above identifies the owner.
    // Publish the row with that owner from the first update.
    statuses.set(ChannelId::Trigger, trigger_state);
    let (mut tray_handle, tray_diagnostics) = tray::spawn(statuses, tray_tx);
    for line in tray_diagnostics {
        log.diag(&line);
    }
    for row in tray_handle.statuses().rows() {
        log.diag(&format!("channel: {row}"));
    }

    let mut event_loop: EventLoop<App> = EventLoop::try_new().context("creating the event loop")?;

    // The long-lived Wayland queue. `registry_queue_init` lets SCTK bind the
    // popup globals.
    // The second hand-made registry serves the cursor channel and reports
    // dynamic global changes.
    let (globals_list, mut queue) =
        registry_queue_init::<App>(&conn).context("initialising the Wayland registry")?;
    let registry = conn.display().get_registry(&queue.handle(), ());

    // The popup. A compositor without layer shell keeps the daemon up.
    // Capture, cursor, trigger, tray, and settings still work.
    // The capability report names the absent global, and the Popup row
    // reports it.
    // Do not drop the popup's other Wayland objects. Their Events still arrive.
    // A handler without its state would panic. The popup always exists.
    // A bind error here is fatal, as the report states.
    // The database path also serves the painter.
    // It opens a read-only connection to the media store because the Worker
    // owns the dictionary on another thread.
    let db = paths.data_dir.join("chibipop.sqlite");
    let mut popup = Popup::bind(&globals_list, &queue.handle(), &config, &db)
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

    // Three surfaces sit beside the popup.
    // Each borrows the popup's process handles: one `wl_compositor`, one
    // `wl_shm`, one `wl_viewporter`, and one `OutputState`.
    // Each returns `None` when the same global is absent, so a
    // layer-shell-less session reports it once and keeps other channels.
    // Two outlines have separate lifetimes for scan rects and the static
    // border.
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

    // The Worker's wake. A result queued on its thread becomes one event-loop
    // wake here (ARCHITECTURE.md#workspace-and-seams).
    // The pump stays sync.
    let (worker_ping, worker_pings) =
        calloop::ping::make_ping().context("creating the worker wake")?;
    let analysis = chibipop::analysis::Service::spawn(
        chibipop::paths::data_file(chibipop::analysis::MODEL_FILE),
        {
            let ping = worker_ping.clone();
            move || ping.ping()
        },
    );

    // AnkiConnect answers come from the call threads.
    // Selected-region pixels come from the grab thread.
    // One-off OCR lines come from the Worker's thread.
    let anki_tx = anki_channel(&event_loop.handle())?;
    let shot_tx = shot_channel(&event_loop.handle())?;
    let ocr_tx = ocr_text_channel(&event_loop.handle())?;

    // The writable selection uses its own connection and thread.
    // A compositor without the data-control protocol (stock GNOME) reports one
    // state, like the absent layer shell above.
    // It affects only `ocr-clipboard`.
    // Both globals let a compositor add support and let the install self-heal
    // (ARCHITECTURE.md#capture-and-masking).
    // A bind failure is not fatal. Log it and keep the other channels.
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
        fatal: None,
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
        analysis,
        worker_setup: worker::Setup {
            globals: globals.clone(),
            backend: capture_selection.backend(),
            db: db.clone(),
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

    // Bind the selected rung and settle it before the pump starts.
    // Both rungs need output geometry.
    // Rung 1 also needs the seat pointer and per-output cursor sessions.
    // Dispatches create those objects.
    let qh = queue.handle();
    match &selection {
        cursor::Selection::Rung(cursor::Rung::ImageCopyCapture) => {
            app.cursor.bind_outputs(&registry, &globals, &qh);
            app.cursor.bind_capture(&registry, &globals, &qh);
        }
        // Rung 2 needs the same layout facts and no more Wayland state.
        // Samples come from the PipeWire stream that the Portal backend
        // opened.
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

    // Popup surfaces: one per output. Map them now and never unmap.
    // The output round trip already ran, so each surface uses known geometry.
    // The second round trip delivers configures and maps each surface hidden
    // before the pump starts.
    if app.popup_can_draw() {
        app.popup_mut().map_all();
        app.flush_popup_notes();
        queue.roundtrip(&mut app).context("mapping the popup's layer surfaces")?;
        app.flush_popup_notes();
        let mapped = app.popup_mut().surface_count();
        app.log.diag(&format!("popup: {mapped} layer surface(s) mapped hidden"));
    }

    // Static-region border if the config requests it.
    // This is the first predicate call site.
    // A daemon in Static mode starts with an outlined box, like the Windows
    // bin.
    // The round trip delivers outline configures and puts strips on screen.
    // Without it, the border appears only after the next calloop wake.
    app.sync_static_outline();
    queue.roundtrip(&mut app).context("mapping the static region's outline")?;
    app.flush_surface_notes();

    // Run the surface probe if requested.
    // The outline and selector use the geometry just used for popup surfaces.
    // This is the last point where the daemon queue can receive a manual round
    // trip. Calloop owns the queue after this point.
    if std::env::var(SURFACE_PROBE_ENV).is_ok_and(|v| v == "1") {
        app.probe_surfaces(&mut queue);
    }

    insert_wayland_source(&event_loop.handle(), &conn, queue)?;

    // Rung 3 is the only timed source. Event rungs cause zero idle wakeups
    // (ARCHITECTURE.md#hover-cadence).
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

    // Rung 2 samples already use global physical pixels.
    // They come from the portal stream thread.
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
        // Nothing sends on this path.
        // Drop the receiver so a stray sample yields a cheap error instead of
        // an unbounded queue.
        drop(cursor_rx);
    }

    // Portal session events include the bound set, each press and release, and
    // diagnostics.
    // Register the receiver for every rung so it outlives the sender.
    // The native rung starts no sender. An idle channel causes no wakeups.
    event_loop
        .handle()
        .insert_source(shortcut_rx, |event, _, app: &mut App| {
            if let calloop::channel::Event::Msg(event) = event {
                app.handle_shortcut(event);
            }
        })
        .map_err(|e| anyhow::anyhow!("registering the shortcut channel: {e}"))?;

    // Menu activations and tray diagnostics run on this thread.
    // `Event::Closed` needs no action. The tray thread exit means a trayless
    // session.
    event_loop
        .handle()
        .insert_source(tray_rx, |event, _, app: &mut App| {
            if let calloop::channel::Event::Msg(request) = event {
                app.handle_tray(request);
            }
        })
        .map_err(|e| anyhow::anyhow!("registering the tray channel: {e}"))?;

    // `signalfd` was blocked at the top of `run`. Now the pump reads it.
    // Every daemon thread inherited that mask, so process SIGINT/SIGTERM has
    // no other destination.
    event_loop
        .handle()
        .insert_source(signals, |event, _, app: &mut App| {
            app.log.diag(&format!("signal: {:?} - shutting down", event.signal()));
            app.signal.stop();
        })
        .map_err(|e| anyhow::anyhow!("registering the signal source: {e}"))?;

    // Drain the Worker's results after its wake.
    event_loop
        .handle()
        .insert_source(worker_pings, |_, _, app: &mut App| app.drain_results())
        .map_err(|e| anyhow::anyhow!("registering the worker wake: {e}"))?;

    // Start the pipeline last.
    // OCR model and dictionary open operations block.
    // All earlier channels must exist before a lookup request.
    // On the Portal rung, the consented session moves to the Worker thread
    // here.
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

    // When the loop drops, the control source drops and unlinks the socket
    // file.
    // The lock file remains (see lock.rs), and the kernel releases the flock
    // when `lock` drops.
    drop(event_loop);
    app.log.diag("shutdown: control socket unlinked, instance lock released");
    drop(lock);

    // A protocol error stops the pump like a signal.
    // The shutdown above remains orderly, but a compositor-killed session
    // differs from a user quit.
    // The exit status must tell a supervisor the difference.
    // The log already contains the object, code, and compositor message.
    // This status carries the verdict only.
    match &app.fatal {
        Some(err) => bail!("shut down on a Wayland protocol error on {}", err.object_interface),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcuts::ShortcutId;
    use wayland_client::backend::ObjectId;

    /// Test hot reload. `reload` reads the file again, so the lookup-log gate
    /// follows the config without a restart.
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

    /// The tray thread has no log. It sends diagnostics as requests, and this
    /// method writes them.
    /// A trayless run uses this path for its "no tray host" line.
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

    /// Quit stops the pump through the tray's calloop channel.
    /// `run` resets and watches the loop signal.
    /// The test checks that the pump makes no later pass.
    /// A timeout turns a regression into a failure, not a hang.
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

    /// Log each channel change once.
    /// A failed poll then cannot fill the log at the poll cadence.
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

    /// Use a screencopy session without a pipeline.
    /// Tests then use the promptless rung, avoid a portal dialog, and open no
    /// OCR models for log assertions.
    fn test_app(
        dir: &std::path::Path,
        log_file: &std::path::Path,
        // The pump's own lifetime, because `App` holds its handle.
        event_loop: &EventLoop<'static, App>,
    ) -> App {
        let capture = capture_backend::Selection::Backend(Backend::WlrScreencopy);
        let (worker_ping, _pings) = calloop::ping::make_ping().unwrap();
        let analysis = chibipop::analysis::Service::spawn(
            dir.join("missing-analysis-model"),
            || {},
        );
        App {
            log: Log::open(log_file, false),
            stub: StubState::default(),
            // Keep every test directory under the scratch directory in XDG
            // shape.
            // Relative `save_dir` resolves under `data_dir`, so each screenshot
            // test writes its PNG inside its scratch directory.
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
            fatal: None,
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
            analysis,
            worker_setup: worker::Setup {
                globals: Vec::new(),
                backend: Some(Backend::WlrScreencopy),
                db: dir.join("chibipop.sqlite"),
            },
            worker_ping,
            anki_tx: anki_channel(&event_loop.handle()).expect("the anki answer channel"),
            shot_tx: shot_channel(&event_loop.handle()).expect("the screenshot pixel channel"),
            shot: None,
            // No pipeline means no engine can serve a job.
            // A test that needs one installs it (`fake_worker`).
            ocr_jobs: worker::OcrJobs::disconnected(),
            ocr_tx: ocr_text_channel(&event_loop.handle()).expect("the OCR text channel"),
            ocr_job: None,
            // No compositor means no data-control connection.
            // This also matches the GNOME state that these tests can assert
            // without one.
            clipboard: None,
            dicts: Vec::new(),
            hold: None,
            last_warning: None,
            portal_serving: false,
            capture_selection: capture,
            // The software-cursor probe is a startup fact.
            // The harness tests the combination, not the compositor option.
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

    /// `reload` retries only portal capture and only when the one-shot guard
    /// allows it.
    /// A screencopy session must not reach the Portal or change the Capture row
    /// (ARCHITECTURE.md#capture-and-masking).
    /// The promptless rung has nothing to ask for.
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

    /// Test the static-region action end to end.
    /// The rect reaches the config file (ARCHITECTURE.md#settings-and-config).
    /// The file is the sole source, so a restart finds the box there.
    /// The same rect reaches the pipeline.
    /// The next hover reads the user's box, not a cursor-centered tile.
    ///
    /// The pick uses a stub at its seam.
    /// `took_static_region` returns `Option<PhysRect>`, which supplies all
    /// needed input.
    /// `pick_region` is a nested Wayland pump. A unit test has no compositor.
    #[test]
    fn the_static_region_verb_saves_the_rect_and_the_pipeline_reads_it() {
        let dir = scratch("static_region_set");
        let log_file = dir.join("chibipop.log");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let (worker, _log) = fake_worker(None, None);
        app.worker = Some(worker);
        // Save pushes a reload. Reload rebuilds `WorkerSettings` from
        // `self.dicts`.
        // The fake Worker represents a finished spawn. It must leave the
        // identity.
        app.dicts = fake_dicts();
        // The user chose Static in the settings. Only the box is absent.
        app.config.anki.sentence_mode = chibipop::config::SentenceMode::Static;

        // Place `AT` near (600,300), but outside the cursor-centered tile.
        // The anchor then distinguishes the two regions.
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

        // Save sends reload through the same channel that hover uses.
        // The Worker settles settings before hover, so this read proves that
        // the region arrived.
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

    /// A canceled pick changes nothing: no file, memory region, or reload.
    /// Esc, a right click, a short drag, and no layer shell all return `None`.
    /// The socket verb enters this path through `handle_request`, `apply_verb`,
    /// and `pick_region`.
    /// No drag then produces `None`.
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

    /// Every show and hide checks this predicate
    /// (the Windows bin's `LiveSettings::static_overlay_region`).
    /// Static mode, an enabled outline, and a drawn region must all hold.
    /// The user can change the mode. A switch away from Static removes the
    /// border.
    #[test]
    fn the_outline_wants_a_drawn_region_in_static_mode_with_the_box_ticked() {
        use chibipop::config::SentenceMode;
        let mut cfg = chibipop::config::Config::default();
        let rect = PhysRect { x: 10, y: 20, w: 300, h: 40 };
        cfg.anki.static_region = Some([10, 20, 300, 40]);

        // Test each mode and checkbox with a drawn region.
        cfg.anki.sentence_mode = SentenceMode::Static;
        cfg.anki.show_static_overlay = true;
        assert_eq!(Some(rect), static_overlay_region(&cfg), "static, ticked, drawn");

        cfg.anki.show_static_overlay = false;
        assert_eq!(None, static_overlay_region(&cfg), "static, unticked");

        cfg.anki.sentence_mode = SentenceMode::Line;
        assert_eq!(None, static_overlay_region(&cfg), "not static, unticked");

        cfg.anki.show_static_overlay = true;
        assert_eq!(None, static_overlay_region(&cfg), "ticked, but the mode left Static");

        // Test both switches with no region.
        cfg.anki.sentence_mode = SentenceMode::Static;
        cfg.anki.static_region = None;
        assert_eq!(None, static_overlay_region(&cfg), "no box has been drawn yet");
    }

    /// A session without a layer shell still serves lookups.
    /// It cannot draw the border, and the daemon reports that fact.
    /// Reload is one of the predicate's call sites because it asks again.
    /// A mode away from Static then removes the border request.
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

    /// Protect a Portal session with a grant.
    /// When it serves, reload must not tear it down or prompt again.
    /// Settings Apply sends reload after every save.
    /// One consent covers one grant.
    #[test]
    fn reload_does_not_reprompt_a_serving_portal_session() {
        let dir = scratch("noreprompt");
        let log_file = dir.join("chibipop.log");

        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        app.capture_selection = capture_backend::Selection::Backend(Backend::Portal);
        app.portal_serving = true;
        // A retry is armed. Only `portal_serving` can prevent the dialog.
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

    /// Create a scratch directory for each test.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("chibipop_daemon_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ---- ocr-clipboard ----

    /// Stock GNOME reaches this path through the socket, like a compositor
    /// bind.
    /// Refuse before the pick, report both globals, and leave no job in flight.
    /// `test_app` has no compositor or data-control connection, which is the
    /// same state.
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

    /// A canceled pick returns no text and queues no job.
    /// Esc, a right click, a short drag, and no layer shell all cancel.
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

    /// If the recognizer finds no text, do not clear the clipboard.
    /// Keep the user's current selection.
    /// An empty result must not destroy data after a bad drag.
    /// Release the OCR job in either case, so the next press works.
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

    /// Report a failed grab or read, and release the OCR job.
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
        // Text without a queued region must report an error, not copy a stale
        // answer to the clipboard.
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

    /// Test verb effects on the hold.
    /// No pipeline exists, so this checks daemon state independent of lookup.
    /// The verb table must stay correct when lookup cannot run.
    #[test]
    fn a_press_holds_and_a_release_ends_it() {
        let dir = scratch("hold");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);
        // Use an event rung. The test must not call `hyprctl`.
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

    /// `toggle` lasts beyond key release.
    /// Release while latched changes nothing. A second toggle ends the hold
    /// (ARCHITECTURE.md#hover-cadence).
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

    /// Trigger mode reads the current cursor position.
    /// A press before the first cursor sample logs a line, not a lookup.
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

    /// When a hold crosses outputs, it triggers a new grab.
    /// The hold follows the cursor onto the entered output
    /// (ARCHITECTURE.md#hover-cadence).
    /// This test injects geometry for one monitor.
    /// `trigger::regrab` gives the decision, and the test checks the daemon
    /// action.
    #[test]
    fn a_hold_follows_the_cursor_onto_another_output() {
        let dir = scratch("crossing");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);
        let left = PhysRect { x: 0, y: 0, w: 1920, h: 1080 };
        app.hold = Some(Hold { output: left, latched: false });

        // Output geometry has not arrived, so `bounds_containing` returns a
        // plausible box.
        // It differs from the held box, which indicates an output change.
        app.on_cursor_position(PhysPoint { x: 5000, y: 500 });

        let now = app.hold.expect("the hold survives the crossing");
        assert_ne!(left, now.output, "the hold must move to the entered output");
        assert!(now.output.contains(PhysPoint { x: 5000, y: 500 }));
        let written = std::fs::read_to_string(dir.join("chibipop.log")).unwrap();
        assert!(written.contains("crossed onto another output"), "log was: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Log a failed lookup once, not once per cursor sample.
    /// Unsupported text would otherwise fill the file at the sample rate.
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

    // ---- scan overlay (`Command::ShowScanOverlay`) ----

    /// One hover's scan rects in core order:
    /// the pass-1 capture box first, and the Match box last.
    fn one_hovers_rects() -> Vec<ScanRect> {
        vec![
            ScanRect { rect: PhysRect { x: 100, y: 200, w: 240, h: 60 }, kind: ScanKind::Pass1 },
            ScanRect { rect: PhysRect { x: 140, y: 210, w: 40, h: 40 }, kind: ScanKind::Match },
        ]
    }

    /// Two Linux settings use this Command:
    /// `debug.show_scan_region` and `popup.highlight_match`, which is on by
    /// default.
    /// Before this path existed, the `execute` catch-all ignored both settings.
    /// Keep the catch-all as the regression target.
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
        // The unit test has no compositor. Report the honest degradation with
        // its count.
        // Do not stay silent or claim that a surface was drawn.
        assert!(
            written.contains("overlay: 2 scan rect(s) and no outline on this compositor"),
            "log was: {written}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Core sends an empty vector when both settings are off (`controller.rs`).
    /// It hides the overlay and is common.
    /// It must cost no log line per hover.
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

    /// Each box reaches the outline with a two-pixel frame outside itself.
    /// The next grab then reads no border (ARCHITECTURE.md#capture-and-masking).
    /// Match uses its own theme color, so it differs from its capture box.
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

    // ---- trigger channel Portal rung ----

    fn binding(id: shortcuts::ShortcutId, trigger: Option<&str>) -> shortcuts::Binding {
        shortcuts::Binding { id, trigger: trigger.map(str::to_string) }
    }

    /// Portal press and release use the same trigger semantics as
    /// `ctl trigger-down` and `trigger-up`.
    /// Two sources feed one trigger.
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

    /// The add shortcut is not a trigger.
    /// Press reaches the Controller without a grab. Release does nothing.
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

    /// The tray row and settings window report the Portal binding.
    /// This test checks channel visibility.
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
        // A trigger that works never raises the tray's attention icon: the
        // socket and the portal both work.
        assert_eq!(ksni::Status::Active, app.tray.statuses().sni_status());

        let published = shortcuts::state::read(&dir).expect("the daemon publishes the channel");
        assert!(published.portal);
        assert_eq!(Some("Alt+F".to_string()), published.description(ShortcutId::Trigger));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A user can change the key in the desktop UI.
    /// The row and published state update without a restart.
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

    /// If Portal cannot serve, the status includes a reason.
    /// The socket remains the trigger.
    /// The tray shows no attention icon, and the settings window no longer
    /// reports a Portal binding.
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

    /// The Portal thread has no log.
    /// It sends diagnostics as Events, and this method writes them.
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

    // -- live hover and the dwell re-check --

    use chibipop::lookup::deconj::Deconjugator;
    use chibipop::lookup::engine::LookupEngine;
    use chibipop::lookup::model::FakeDictionary;
    use chibipop::text::layout::{OcrLine, OcrWord};
    use chibipop::text::{Frame, OcrEngine, RegionCapture};
    use chibipop::worker::WorkerParts;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Test deadline for fake operations. A healthy path does not reach it.
    const TIMEOUT: Duration = Duration::from_secs(10);

    /// Cursor point and word for fake hovers.
    const AT: PhysPoint = PhysPoint { x: 600, y: 300 };
    const WORD: &str = "\u{98DF}";

    /// Fake output can hold a read region.
    /// It stays small enough for a press-time full grab to use one cheap
    /// allocation.
    const FAKE_OUTPUT: i32 = 1000;

    /// Fake seam log in order.
    /// One `Vec` replaces per-seam channels.
    /// All seams run on the Worker thread, so one lock preserves pipeline
    /// order.
    /// A received result marks completion.
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

    /// Fake pixels log each grab and can pause it.
    /// A test can then inspect an active read.
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

    /// Fake OCR reports one word and whether its input had a mask.
    /// Capture pixels are black. A mask fills white
    /// (ARCHITECTURE.md#capture-and-masking).
    /// White pixels therefore prove the mask.
    /// Alpha does not prove it because the upscale makes it opaque.
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

    /// Real core pipeline over fakes.
    /// It opens no screen, OCR models, or database.
    /// Tests can count each pass.
    fn fake_worker(
        gate: Option<mpsc::Receiver<()>>,
        entered: Option<mpsc::Sender<()>>,
    ) -> (Worker, Seams) {
        fake_worker_serving(gate, entered, None)
    }

    /// The same pipeline with the shipped `serve` hook over the fake engine.
    /// The one-off OCR seam then runs without a compositor or ONNX models.
    fn fake_worker_serving(
        gate: Option<mpsc::Receiver<()>>,
        entered: Option<mpsc::Sender<()>>,
        jobs: Option<mpsc::Receiver<worker::OcrRequest>>,
    ) -> (Worker, Seams) {
        let log = seams();
        let capture_log = log.clone();
        let ocr_log = log.clone();
                // Use a named dictionary, not `&[]`.
                // An empty terms list searches nothing
                // (ARCHITECTURE.md#dictionary-and-lookup).
                // A pipeline without its identity would read every lookup and
                // present none.
        let settings = worker::settings(&chibipop::config::Config::default(), &fake_dicts());
        let (worker, _dicts) = Worker::spawn(
            settings,
            move || {
                let mut dict = FakeDictionary::new();
                dict.add_dict(1, "FakeDict");
                dict.add_term(WORD, None, None, "", None, 10, 1);
                dict.add_entry(10, 1, r#"["to eat"]"#);
                Ok(WorkerParts {
                    capture: Box::new(FakeCapture { log: capture_log, gate, entered }),
                    ocr: Box::new(FakeOcr { log: ocr_log }),
                    dict: Box::new(dict),
                    reopen_dict: None,
                    engine: LookupEngine::new(Deconjugator::new(Vec::new())),
                    // Use the shipped hook, not a local stand-in.
                    serve: jobs.map(worker::serve_jobs),
                })
            },
            || {},
        )
        .expect("the fake pipeline must start");
        (worker, log)
    }

    /// Return the identity that the `fake_worker` dictionary reports.
    /// Real `spawn_worker` gets it from the first pipeline read and stores it
    /// in `App::dicts`.
    /// The fake Worker represents a finished spawn, so it represents this
    /// identity too.
    fn fake_dicts() -> Vec<DictInfo> {
        vec![DictInfo { dict_id: 1, name: "FakeDict".to_string() }]
    }

    /// Receive one answer from the calloop channel, or report a test failure.
    ///
    /// `calloop::channel::Channel` has no `recv_timeout`.
    /// A `recv` call on a hook that never runs would hang the suite and hide the
    /// failure.
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

    /// Test the one-off OCR path end to end without a compositor.
    /// A queued job wakes the Worker from its trigger channel.
    /// The Worker runs the job on the thread that owns the engine.
    /// It then sends lines to the pump channel.
    /// Core tests the same shape in
    /// `tests/worker.rs::a_nudged_job_wakes_a_blocked_worker_and_is_read_through_the_facade`.
    /// This test uses this crate's fakes and the shipped hook.
    ///
    /// Pixels are white, so `FakeOcr` reports `masked=true`.
    /// The assertion proves that the engine received this job's bytes, not
    /// black capture bytes.
    /// No `grab` in the seam log proves the second half.
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
        // The nudge alone woke it. No trigger was sent, so no lookup exists.
        assert!(worker.results().try_recv().is_err(), "a serve wake answers no lookup");
    }

    /// A queue without a pipeline refuses a job.
    /// `ocr-clipboard` on a daemon whose Worker never starts must report a
    /// reason instead of a region pick and stop.
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

    /// Return the pipeline answer, or report a test failure.
    /// The answer completes the seam log.
    fn answer(app: &App) -> chibipop::worker::WorkerResult {
        app.worker
            .as_ref()
            .expect("the pipeline")
            .results()
            .recv_timeout(TIMEOUT)
            .expect("the pipeline must answer")
    }

    /// Core guarantee: a cursor sample becomes a lookup at once.
    /// No timed delay, velocity gate, or dispatch tick sits between
    /// (ARCHITECTURE.md#hover-cadence).
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

    /// An event rung gives a position when its session opens and on movement
    /// (ARCHITECTURE.md#input-ladders).
    /// The daemon can start with the cursor on a word before the pipeline
    /// exists.
    /// If the daemon consumes the sample too early, live mode stays silent
    /// until the mouse moves.
    /// The second sample does not pass the Controller movement gate because the
    /// position did not change.
    #[test]
    fn a_sample_that_arrives_before_the_pipeline_is_spent_once_it_is_up() {
        let dir = scratch("earlysample");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);
        assert!(app.worker.is_none(), "no pipeline yet, as at startup");

        app.on_cursor_position(AT);
        // A rung can send the idle position again after a session reopen or
        // output re-entry.
        // Do not send these samples to the Controller without a pipeline.
        app.on_cursor_position(AT);
        let written = std::fs::read_to_string(dir.join("chibipop.log")).unwrap();
        assert!(!written.contains("lookup:"), "nothing to ask yet: {written}");

        // This matches `spawn_worker` after the pipeline exists.
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

    /// The pipeline's first read provides names for the terms list.
    /// `spawn_worker` cannot resolve the list before the pipeline exists, so it
    /// resolves it again.
    /// Without this step, a fresh daemon uses an empty list until reload.
    /// This is the same empty-list defect one step later.
    #[test]
    fn a_fresh_pipeline_is_told_the_split_once_the_names_are_known() {
        let dir = scratch("rescope");
        let log_file = dir.join("chibipop.log");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        app.config.ocr.language = "ja".to_string();
        // The settings Terms section can contain an unchecked name.
        // The config stores the exact name in the disabled list.
        // An installed dictionary absent from both lists is new and searchable.
        app.config.dictionaries.terms_disabled =
            vec!["Jitendex.org [2026-07-09]".to_string()];
        // `spawn_worker` sent no identities, so no new names and an empty list.
        let sent = worker::settings(&app.config, &[]).present_cfg;
        assert!(sent.terms.is_empty(), "an empty library names nothing to search");

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

    /// If the scope has no change, do nothing.
    /// Every respawn already has names, so re-resolve matches the sent scope.
    #[test]
    fn a_pipeline_whose_scope_did_not_change_is_left_alone() {
        let dir = scratch("norescope");
        let log_file = dir.join("chibipop.log");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let (worker, _seams) = fake_worker(None, None);
        app.worker = Some(worker);
        app.dicts = vec![DictInfo { dict_id: 1, name: "Jitendex.org".to_string() }];
        // The respawn has known identities. `spawn_worker` therefore resolves
        // the same list.
        let sent = worker::settings(&app.config, &app.dicts).present_cfg;

        app.rescope_lookups(&sent);

        let written = std::fs::read_to_string(&log_file).unwrap();
        assert!(!written.contains("searches"), "log was: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A live grab must mask the popup. A frozen hold reads through it
    /// (ARCHITECTURE.md#capture-and-masking).
    /// The fake recognizer reports whether input has a mask, so both cases are
    /// observed.
    #[test]
    fn a_live_lookup_masks_the_popup_and_a_hold_reads_through_it() {
        let dir = scratch("livemask");
        let event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &dir.join("chibipop.log"), &event_loop);
        let (worker, log) = fake_worker(None, None);
        app.worker = Some(worker);
        // Put the popup over the hovered point, as a shown popup can be.
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
        // The press takes a full grab and brackets it like every read.
        // The hold lookup uses no backend and no mask because its pixels
        // predate the popup.
        assert_eq!(
            done(&log)[4..],
            ["begin_read", "grab", "end_read", "ocr masked=false"],
            "the hold reads through the popup: {:?}",
            done(&log)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Backpressure paces input.
    /// Samples while an active read run coalesce to the newest.
    /// The daemon queues no extra sample
    /// (ARCHITECTURE.md#hover-cadence: one in flight, latest-wins).
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
        // Both samples arrive while the active read runs.
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

    /// Test the daemon half of the arm rule.
    /// A hold reads immutable pixels, so trigger mode never uses a dwell watch.
    #[test]
    fn a_frozen_hold_is_never_dwell_watched() {
        let output = PhysRect { x: 0, y: 0, w: 1920, h: 1080 };
        assert!(dwell_wanted(None, true), "a shown popup in live mode is watched");
        assert!(!dwell_wanted(Some(Hold { output, latched: false }), true));
        assert!(!dwell_wanted(Some(Hold { output, latched: true }), true));
        assert!(!dwell_wanted(None, false), "nothing shown is nothing to watch");
    }

    /// A watch with no popup asks the pipeline for nothing.
    /// It expires at its deadline, so the idle daemon keeps no timed source
    /// (ARCHITECTURE.md#hover-cadence: zero idle wakeups).
    ///
    /// Arm it by hand. A popup with a rect needs a compositor.
    /// The Controller supplies the other half in core tests.
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

    // -- AnkiConnect --

    use std::io::{Read, Write};

    /// Fake AnkiConnect server.
    ///
    /// The test seam is a socket, not a trait.
    /// `chibipop::anki` sends plain HTTP with `ureq`.
    /// This matches the Windows bin, so daemon calls leave the process.
    /// The fake server is the far end of the wire and stores each request body.
    /// Tests then inspect the sent deck, model, and fields.
    struct FakeAnki {
        url: String,
        seen: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    }

    impl FakeAnki {
        /// Answer `replies` requests, then close.
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

    /// Read one HTTP request body from its `Content-Length`.
    fn read_body(stream: &mut std::net::TcpStream) -> String {
        let mut raw = Vec::new();
        let mut byte = [0u8; 1];
        // Read headers one byte at a time.
        // Do not consume the body into a buffer that this method cannot return.
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

    /// Return the AnkiConnect v6 answer for the two popup actions.
    ///
    /// `canAddNotes` rejects the first note and accepts the rest.
    /// The test can then assert one known duplicate.
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

    /// Point the daemon at the fake server and enable the feature.
    fn anki_at(app: &mut App, url: &str) {
        app.config.anki.enabled = true;
        app.config.anki.url = url.to_string();
        app.config.anki.deck = "Mining".to_string();
        app.config.anki.model = "Lapis".to_string();
        app.controller = Controller::new(controller_config(&app.config));
    }

    /// Run the pump until `wanted` appears or `budget` passes pass.
    /// Then return the log.
    ///
    /// The answer crosses a thread and a calloop channel.
    /// The pump must dispatch it before assertions.
    /// A no-answer test still needs a pass budget.
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

    /// The Controller requests a dupe check after popup placement.
    /// This test sends the real AnkiConnect action with the configured deck and
    /// model.
    /// The answer returns to the pump.
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

    /// The add action uses the same path as the Anki button and `anki-add`
    /// shortcut.
    /// This HTTP call creates the card.
    /// Fields use `anki.field_map`, like Windows.
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

    /// Anki can be down. It must produce one line and no other result.
    /// It must not panic or stop the pump on a dead socket.
    #[test]
    fn an_ankiconnect_that_is_not_listening_is_one_line() {
        let dir = scratch("ankidown");
        let log_file = dir.join("chibipop.log");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        // Bind a port, then release it. No process listens on it.
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

    /// Both entry points use the same guarded Event.
    /// With no popup, there is no card to add.
    /// Neither shortcut nor slot click can reach the network.
    /// This matches the Windows enable rule: no button and no armed hotkey.
    /// The Controller supplies this path instead of a hook.
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
        // Allow enough passes for a request if one existed.
        let written = pump_until(&mut event_loop, &mut app, &log_file, "anki: ", 8);

        assert!(anki.seen().is_empty(), "no popup, no card: {:?}", anki.seen());
        assert!(!written.contains("anki: "), "log was: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Represent a popup that the Controller considers visible and the
    /// dupe-check generation that it orders.
    ///
    /// Call the Controller directly because the real placement round trip
    /// needs a compositor for `PopupPlaced`.
    /// Test the path from a shortcut press to the AnkiConnect call, not the
    /// layer surface.
    ///
    /// The generation is needed to answer the dupe check.
    /// The Controller rejects a stale generation.
    /// The popup `connected` flag decides whether a screenshot add has a card
    /// to file.
    /// The answer sets this flag.
    /// `None` means Anki is off and no check was ordered.
    /// Callers can ignore the result.
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
                        pitch: Vec::new(),
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
        // The dupe check starts when the rect arrives, not when the
        // presentation arrives.
        // `begin_place` ends at `PopupPlaced`.
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

    /// The `anki-add` Portal shortcut creates a card for the current lookup.
    /// This test cannot synthesize a Portal press because it needs an app ID and
    /// a real key.
    /// Enter at the same point as the Portal thread's Events.
    /// Assert the card on the wire.
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

        // Release does not add again (`Action::Nothing`).
        // The Controller rejects a repeat after the first add.
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

    /// The control-socket `anki-add` path creates the same card as the Portal
    /// shortcut.
    /// It uses rung 2, the only rung for a sway user.
    /// The test enters through `handle_request` with the verb from the wire
    /// word.
    /// This covers `chibipop ctl anki-add` except the socket bytes.
    /// `control` owns the socket round-trip test.
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

        // Both rungs use one path, not two.
        // A Portal press after the socket add creates nothing because the
        // Controller knows that the lookup was added.
        app.handle_shortcut(shortcuts::Event::Fired {
            id: shortcuts::ShortcutId::AnkiAdd,
            activated: true,
        });
        pump_until(&mut event_loop, &mut app, &log_file, "never logged", 4);
        assert_eq!(1, anki.seen().len(), "one card, whichever rung asks");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Anki off removes the slot and the check.
    /// Popup placement sends no dupe check, and a press sends no request.
    /// The Windows button and hotkey use the same rule.
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

    // ---- mining screenshot and picture carried by an add ----
    //
    // **Stub for region pick.** These tests use no compositor.
    // Two seams represent the real flow:
    //
    // - `App::took_shot_region(picked, shot)` receives the pick answer, as
    //   `static-region` does. `None` means cancel.
    // - `App::handle_shot(grabbed)` receives the `calloop::channel` message from
    //   the grab thread. A fabricated `Frame` matches real pixels at this seam.
    //
    // Only the drag and `capture::oneshot` remain outside these tests.
    // `tests/surfaces_live.rs` and `capture-dump` cover them with a compositor.

    /// Two by two solid blue pixels.
    /// This is the smallest input `encode_bgra_to_png` accepts: BGRA8,
    /// top-down, `w * h * 4`.
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

    /// Enable the screenshot gate and set the picture path.
    ///
    /// `save_dir` is relative, so it resolves under `test_app`'s XDG
    /// `data_dir`, the scratch directory.
    /// This uses the same `Paths::screenshots_dir` as the daemon.
    fn screenshots_on(app: &mut App) {
        app.config.actions.screenshot.include_on_add = true;
        app.config.actions.screenshot.save_dir = "shots".to_string();
        app.config.anki.field_map.push(chibipop::config::FieldMapping {
            anki_field: "Screenshot".to_string(),
            source: "screenshot".to_string(),
        });
    }

    /// Return the parked plan, or panic with its current state.
    fn parked(app: &mut App) -> Pending {
        match app.shot.take() {
            Some(Shot::Parked(shot)) => shot,
            Some(Shot::Grabbing(_)) => panic!("a grab is in flight, not a parked plan"),
            None => panic!("no plan was parked - the add went out without a picture"),
        }
    }

    /// Test screenshot add end to end without a compositor.
    /// `anki-add` does not dispatch a plain add.
    /// The card call carries a picture.
    /// `chibipop::shot` supplies its filename, target field, and folder.
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

        // The plan is parked and the plain add is suppressed.
        // Core owns the filename and picture field, so the assertions check
        // them here.
        let shot = parked(&mut app);
        assert_eq!(dir.join("shots"), shot.plan.path.parent().expect("a folder"));
        let stem =
            shot.plan.path.file_stem().expect("a stem").to_string_lossy().into_owned();
        assert!(stem.starts_with(WORD), "core names the file after the word: {stem}");
        assert_eq!(Some("Screenshot".to_string()), shot.plan.picture_field);
        assert!(anki.seen().is_empty(), "the plain add must not have gone out too");

        // The grab thread returns the answer.
        // Pixels stand in for a real region (see the note above).
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

    /// Core owns the gate, and the default is off.
    /// With `include_on_add = false`, add matches the old path byte for byte:
    /// no plan, pick, or picture on the wire.
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

    /// Test deferral and cancel.
    /// The plan parks inside the socket callback. The pump handles it later.
    /// A pick that returns no region files the card once without a picture.
    /// No layer shell makes the pick return no region.
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

    /// A failed grab follows the Windows rule: file the card without a
    /// picture, not skip the card.
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

    /// If picture save fails, the popup still receives an answer.
    /// `start_add` marks the add state before picture authorization.
    /// A silent failure would leave "Adding…" on screen forever.
    ///
    /// A file with the folder name makes `create_dir_all` fail without root.
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

    /// The screenshot verb saves the lookup's mining context as its own card.
    /// It ignores `include_on_add`. Core has a separate entry point
    /// (`shot::plan`).
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
        // A served dupe check leaves the popup's AnkiConnect state.
        // This state decides whether a card exists.
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

    /// If AnkiConnect does not serve the popup, still save the picture.
    /// Save it to disk without a card. Do not claim that the word was added.
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
        // A fresh popup sets `connected` true when Anki is enabled
        // (`AnkiPopupState::fresh`).
        // A dupe check with no result makes the state inactive.
        // This matches the AnkiConnect-down state.
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

    /// A screenshot verb with no popup reports a clear reason.
    ///
    /// Windows `is_available` uses the same gate: a visible popup with a card.
    /// Windows fails silently, but a compositor bind has no dialog or return
    /// code.
    /// The log line is the only diagnosis.
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

    /// Allow one pick at a time. `Option<Shot>` is this slot.
    /// Two verbs can arrive in one socket callback.
    /// The second must not put a box over the dim from the first.
    /// The add still files the card.
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

        // The add parks first. The mining verb arrives before the pump idle
        // pass.
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

    // ---- a protocol error ends the daemon ----

    /// A `ProtocolError` like the error that a compositor sends when a client
    /// binds an unknown global.
    fn a_protocol_error() -> ProtocolError {
        ProtocolError {
            code: 0,
            object_id: 2,
            object_interface: "wl_registry".to_string(),
            message: "invalid global wl_seat (4294967295)".to_string(),
        }
    }

    /// Test the fatal decision.
    /// A protocol error applies to the whole connection.
    /// A slow compositor or a lost socket does not.
    /// A fatal response to either one would stop a healthy daemon.
    #[test]
    fn only_a_protocol_error_is_fatal_to_the_daemon() {
        let fatal = DispatchError::Backend(WaylandError::Protocol(a_protocol_error()));
        assert_eq!(
            Some("wl_registry"),
            fatal_protocol_error(&fatal).map(|e| e.object_interface.as_str())
        );

        for kind in [std::io::ErrorKind::WouldBlock, std::io::ErrorKind::BrokenPipe] {
            let io = DispatchError::Backend(WaylandError::Io(std::io::Error::from(kind)));
            assert!(fatal_protocol_error(&io).is_none(), "{kind:?} is the source's business");
        }

        let bad = DispatchError::BadMessage {
            sender_id: ObjectId::null(),
            interface: "wl_surface",
            opcode: 3,
        };
        assert!(fatal_protocol_error(&bad).is_none(), "already fatal one layer down");
    }

    /// Test the connections without a compositor.
    /// The verdict ends the pump like a signal, records the exit status, and
    /// logs once.
    ///
    /// Send the error from inside the loop.
    /// `EventLoop::run` clears the stop flag at start, so an outside stop proves
    /// nothing.
    /// The `select.rs` note explains this.
    #[test]
    fn a_protocol_error_ends_the_pump_and_is_diagnosed_once() {
        let dir = scratch("protoend");
        let log_file = dir.join("chibipop.log");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        event_loop
            .handle()
            .insert_source(Timer::immediate(), |_, _, app: &mut App| {
                // Call twice because the backend error stays set.
                // Each later wakeup sees it again.
                app.end_on_protocol_error(&a_protocol_error());
                app.end_on_protocol_error(&a_protocol_error());
                TimeoutAction::Drop
            })
            .unwrap();

        let passes = run_bounded(&mut event_loop, &mut app);

        assert!(passes < RUNAWAY, "the pump must end on the verdict, not iterate: {passes} passes");
        let err = app.fatal.as_ref().expect("the exit status needs the verdict");
        assert_eq!("wl_registry", err.object_interface);
        let written = std::fs::read_to_string(&log_file).unwrap();
        assert_eq!(
            1,
            written.matches("wayland: protocol error on wl_registry#2 - code 0:").count(),
            "one diagnostic, naming the object, the code and the compositor's message: {written}"
        );
        assert!(written.contains("invalid global wl_seat"), "log was: {written}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Test the real compositor case.
    /// A temporary registry binds an impossible global. The pump then ends.
    ///
    /// This is the target of the protocol-error path.
    /// CI is headless (ARCHITECTURE.md#packaging-and-ci), so the earlier test
    /// checks the connections without a compositor.
    /// Unknown-global refusal belongs to the Wayland library linked by the
    /// compositor, not to the compositor itself.
    #[test]
    fn a_real_protocol_error_ends_the_pump() {
        if std::env::var_os("WAYLAND_DISPLAY").is_none() {
            eprintln!("skipping: WAYLAND_DISPLAY is unset (headless)");
            return;
        }
        let Ok(conn) = Connection::connect_to_env() else {
            eprintln!("skipping: WAYLAND_DISPLAY is set but not connectable");
            return;
        };
        let dir = scratch("protolive");
        let log_file = dir.join("chibipop.log");
        let mut event_loop: EventLoop<App> = EventLoop::try_new().unwrap();
        let mut app = test_app(&dir, &log_file, &event_loop);
        let queue = conn.new_event_queue::<App>();
        let qh = queue.handle();
        insert_wayland_source(&event_loop.handle(), &conn, queue).unwrap();

        // Create a temporary registry.
        // Ask for a global name that no compositor advertises.
        // The compositor answers `wl_display.error` and drops the connection.
        let registry = conn.display().get_registry(&qh, ());
        registry.bind::<WlSeat, (), App>(u32::MAX, 1, &qh, ());

        let passes = run_bounded(&mut event_loop, &mut app);

        let err = app.fatal.as_ref().unwrap_or_else(|| {
            panic!(
                "the compositor's protocol error must end the daemon; {passes} passes, log: {}",
                std::fs::read_to_string(&log_file).unwrap_or_default()
            )
        });
        assert!(
            passes < RUNAWAY,
            "and end it rather than iterate on the dead socket: {passes} passes"
        );
        let written = std::fs::read_to_string(&log_file).unwrap();
        assert_eq!(
            1,
            written.matches("wayland: protocol error on").count(),
            "one diagnostic for {err:?}: {written}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Set the maximum passes before a test stops a pump that does not end.
    /// A live compositor answer needs a round trip.
    /// This guard uses no sleep, so it can consume the budget in microseconds.
    const RUNAWAY: u32 = 50;

    /// Run until the pump stops or `RUNAWAY` passes pass.
    /// Return the pass count.
    /// The escape turns a regression into a failed test, not a hang.
    fn run_bounded(event_loop: &mut EventLoop<'static, App>, app: &mut App) -> u32 {
        let escape = event_loop.get_signal();
        let mut passes = 0;
        event_loop
            .run(Some(Duration::from_millis(20)), app, |_| {
                passes += 1;
                if passes >= RUNAWAY {
                    escape.stop();
                }
            })
            .expect("the pump must end, not error");
        passes
    }
}
