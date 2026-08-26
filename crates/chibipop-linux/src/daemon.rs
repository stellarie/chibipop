//! The daemon: calloop pump + instance lock + control socket + logging
//! (ADR-0001: all sync, calloop as the Linux pump), the popup's layer
//! surfaces (ADR-0004), and the capture channel's startup half — the
//! ADR-0002 backend ladder and, when it picks the portal rung, the
//! eager consent that has to finish before anything reports a channel
//! state. OCR is the one channel still to plug into this loop.

use crate::capture::backend::{self as capture_backend, Backend};
use crate::capture::portal::{self, PortalCapture, PortalSession};
use crate::control::{ControlSocket, StubState, Verb};
use crate::cursor::{self, budget, hyprctl, image_copy};
use crate::cursor::image_copy::{CursorHandler, CursorState};
use crate::settings::child::{self, SettingsChild, SpawnOutcome};
use crate::lock::{self, LockError};
use crate::logging::Log;
use crate::paths::Paths;
use crate::tray::status::{ChannelId, ChannelState, ChannelStatuses};
use crate::tray::{self, TrayHandle, TrayRequest};
use crate::popup::{self, Demo, Popup, ShowRequest};
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
use chibipop::geom::{PhysPoint, PhysRect};
use chibipop::present::DictInfo;
use chibipop::text::mask::{CaptureMask, CaptureMode};
use chibipop::worker::{Hover, Trigger, TriggerKind, Worker};
use std::collections::{HashMap, HashSet};
use std::os::fd::{AsFd, BorrowedFd};
use std::path::PathBuf;
use std::time::Instant;
use wayland_client::delegate_dispatch;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_registry;
use wayland_client::protocol::wl_seat::WlSeat;
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
    /// Where the trigger channel's published state goes, so the
    /// settings window can render who owns the binding (ticket 36).
    state_dir: PathBuf,
    config_file: PathBuf,
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
    /// Everything the portal retry needs to run again from here.
    portal_retry: Option<PortalRetry>,
    /// Where a new source goes. The dwell watch is the one source this
    /// daemon adds and drops at runtime, so the pump's own handle has
    /// to be reachable from the state that decides to.
    pump: LoopHandle<'static, App>,
    /// The dwell re-check's timer while one is armed (ADR-0010).
    dwell: Option<RegistrationToken>,
}

/// One AnkiConnect call, as handed to the thread that will make it.
enum AnkiCall {
    Dupes { generation: u64, exprs: Vec<String> },
    Add { expr: String, fields: HashMap<String, String> },
}

/// One AnkiConnect answer, as it comes back to the pump.
///
/// Failures travel as text rather than being printed where they
/// happen: the log lives on the pump thread.
enum AnkiOutcome {
    /// `Err` = AnkiConnect refused, or is not running at all.
    Dupes { generation: u64, dupes: Result<HashSet<String>, String> },
    Added { expr: String, note: Result<i64, String> },
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

    /// One trigger verb's effect, whichever channel delivered it: the
    /// control socket (ADR-0003's rung 2, always bound) or the
    /// GlobalShortcuts portal (rung 1). Both land here on purpose — a
    /// portal press and a `chibipop ctl trigger-down` that could drift
    /// apart would be two trigger semantics, and the product has one.
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
                    // The keyboard path to the Anki affordance: the
                    // same Event the in-panel slot raises, so both
                    // reach one AnkiConnect flow (ADR-0003).
                    shortcuts::Action::Add => self.feed(Event::AddRequested),
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
        if let Err(e) = shortcuts::state::publish(&self.state_dir, published) {
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
            Command::AddNote { expr, fields } => {
                self.spawn_anki(AnkiCall::Add { expr, fields });
            }
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
    fn hide_popup(&mut self) {
        let started = Instant::now();
        let was = self.popup.as_ref().and_then(Popup::shown).is_some();
        if let Some(popup) = self.popup.as_mut() {
            popup.hide();
        }
        self.flush_popup_notes();
        if was {
            self.log.diag(&format!("popup: hidden in {} us", started.elapsed().as_micros()));
        }
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
    fn note_channel(&mut self, id: ChannelId, state: ChannelState) {
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
        match chibipop::config::load_or_create(&self.config_file) {
            Ok(config) => {
                let was = self.log.show_lookup();
                let now = config.debug.show_lookup_log;
                self.log.set_show_lookup(now);
                self.log.diag(&format!(
                    "config: reloaded {}; lookup log {} -> {}",
                    self.config_file.display(),
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
        let settings = worker::settings(&self.config, &self.dicts);
        let started = Instant::now();
        match worker::spawn(&self.worker_setup, settings, portal, self.worker_ping.clone()) {
            Ok((worker, dicts)) => {
                self.log.diag(&format!(
                    "worker: pipeline up in {} ms; {}",
                    started.elapsed().as_millis(),
                    worker::dict_line(&self.worker_setup.db, &dicts),
                ));
                self.dicts = dicts;
                self.worker = Some(worker);
                self.look_where_the_cursor_is();
            }
            Err(e) => self.log.diag(&format!("worker: unavailable - {e:#}")),
        }
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
        capture_state,
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

    // AnkiConnect's answers, from the threads that made the calls.
    let anki_tx = anki_channel(&event_loop.handle())?;

    let mut app = App {
        log,
        stub: StubState::default(),
        state_dir: paths.state_dir.clone(),
        config_file: paths.config_file.clone(),
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
        dicts: Vec::new(),
        hold: None,
        last_warning: None,
        portal_serving: capture.is_some(),
        capture_selection,
        portal_retry,
        popup,
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
            state_dir: dir.to_path_buf(),
            config_file: dir.join("chibipop.toml"),
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
            dicts: Vec::new(),
            hold: None,
            last_warning: None,
            portal_serving: false,
            capture_selection: capture,
            portal_retry: None,
            popup: None,
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
        assert_eq!(Some("Alt+F".to_string()), published.trigger_description());
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
            shortcuts::state::read(&dir).expect("published").trigger_description()
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
        assert_eq!(None, published.trigger_description());
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
                    serve: None,
                })
            },
            || {},
        )
        .expect("the fake pipeline must start");
        (worker, log)
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

    /// A popup the Controller believes is on screen.
    ///
    /// Driven straight into the Controller because the real placement
    /// round-trip needs a compositor to answer `PopupPlaced`, and what
    /// is under test below is the path from a shortcut press to the
    /// AnkiConnect call - not the layer surface.
    fn place_a_popup(app: &mut App) {
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
        app.controller.handle(Event::PopupPlaced {
            rect: PhysRect { x: 100, y: 150, w: 300, h: 200 },
            content_h: 200,
            view_h: 200,
        });
        assert!(app.controller.popup().is_some(), "the Controller must think it is shown");
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
}
