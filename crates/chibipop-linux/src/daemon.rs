//! The daemon: calloop pump + instance lock + control socket + logging
//! (ADR-0001: all sync, calloop as the Linux pump). No capture, OCR, or
//! popup yet — those tickets plug into this loop.

use crate::control::{ControlSocket, StubState, Verb};
use crate::cursor::{self, budget, hyprctl};
use crate::cursor::image_copy::{CursorHandler, CursorState};
use crate::lock::{self, LockError};
use crate::logging::Log;
use crate::paths::Paths;
use crate::wayland;
use anyhow::{bail, Context, Result};
use calloop::generic::Generic;
use calloop::signals::{Signal, Signals};
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, Interest, LoopSignal, Mode, PostAction};
use calloop_wayland_source::WaylandSource;
use chibipop::controller::{Controller, ControllerConfig, Event};
use chibipop::geom::PhysPoint;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::PathBuf;
use std::time::Instant;
use wayland_client::delegate_dispatch;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_client::protocol::wl_registry;
use wayland_client::protocol::wl_seat::WlSeat;
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

/// The pump's shared state.
struct App {
    log: Log,
    stub: StubState,
    config_file: PathBuf,
    signal: LoopSignal,
    /// The cursor channel's Wayland side (ticket 33).
    cursor: CursorState,
    /// Driven by cursor Events; its Commands are logged no-ops until
    /// tickets 35/37 execute them.
    controller: Controller,
    /// CHIBIPOP_CURSOR_TRACE=1: log every sample and poll interval.
    trace: bool,
    /// Last hyprctl sample (logical), for move detection.
    last_poll: Option<(i32, i32)>,
    /// When the hyprctl rung last saw the cursor move.
    last_move: Instant,
}

impl App {
    fn handle_request(&mut self, request: &str, verb: Option<Verb>) {
        let Some(verb) = verb else {
            self.log.diag(&format!("control: rejected {request:?}"));
            return;
        };
        let outcome = self.stub.apply(verb);
        self.log.diag(&format!("control: {} - {}", verb.as_str(), outcome));
        if verb == Verb::Reload {
            self.reload_config();
        }
        if verb == Verb::TriggerDown {
            // Exercises the exact gate real lookups will use: this line
            // reaches the log only when debug.show_lookup_log is on.
            self.log.lookup("(no capture yet - a trigger-down lookup would land here)");
        }
    }

    /// The one piece of state `reload` already really re-reads: the
    /// lookup-log gate. Everything else waits for the core Controller.
    fn reload_config(&mut self) {
        match chibipop::config::load_or_create(&self.config_file) {
            Ok(config) => {
                self.log.set_show_lookup(config.debug.show_lookup_log);
                self.log.diag(&format!(
                    "config: reloaded {}; lookup log {}",
                    self.config_file.display(),
                    if self.log.show_lookup() { "on" } else { "off" }
                ));
            }
            Err(e) => self.log.diag(&format!("config: reload failed: {e:#}")),
        }
    }

    /// One `hyprctl cursorpos` poll tick: sample, feed the seam on
    /// change, re-arm at the adaptive cadence (ADR-0010).
    fn poll_hyprctl(&mut self) -> TimeoutAction {
        if let Some((lx, ly)) = hyprctl::sample() {
            if self.last_poll != Some((lx, ly)) {
                self.last_poll = Some((lx, ly));
                self.last_move = Instant::now();
                if let Some(pos) = self.cursor.logical_to_global(f64::from(lx), f64::from(ly)) {
                    self.on_cursor_position(pos);
                }
            }
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
        for cmd in self.controller.handle(Event::CursorMoved { pos }) {
            self.log.diag(&format!("controller: {cmd:?} (no-op until tickets 35/37)"));
        }
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
        summary_chars: config.popup.summary_chars,
        log_lookups: config.debug.show_lookup_log,
        tick_ms: DISPATCH_TICK_MS,
    }
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
    let caps = cursor::Capabilities::scan(&globals, hyprctl::available());
    let selection = cursor::select(&caps, ladder_override);
    log.diag(&selection.startup_line());
    let trace = std::env::var("CHIBIPOP_CURSOR_TRACE").is_ok_and(|v| v == "1");

    let socket = ControlSocket::bind(runtime_dir, &display)
        .with_context(|| format!("binding the control socket in {}", runtime_dir.display()))?;
    log.diag(&format!("control: listening on {}", socket.path().display()));

    let mut event_loop: EventLoop<App> = EventLoop::try_new().context("creating the event loop")?;

    // The long-lived Wayland queue; its own registry so future tickets
    // see dynamic global changes.
    let mut queue = conn.new_event_queue::<App>();
    let registry = conn.display().get_registry(&queue.handle(), ());

    let mut app = App {
        log,
        stub: StubState::default(),
        config_file: paths.config_file.clone(),
        signal: event_loop.get_signal(),
        cursor: CursorState::default(),
        controller: Controller::new(controller_config(&config)),
        trace,
        last_poll: None,
        last_move: Instant::now(),
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

    app.log.diag("ready: pump running (cursor channel wired; no capture/OCR/popup yet)");

    event_loop.run(None, &mut app, |_| {}).context("running the event loop")?;

    // Dropping the loop drops the control source, which unlinks the
    // socket file; the lock file stays (see lock.rs) and the kernel
    // releases the flock when `lock` drops.
    drop(event_loop);
    app.log.diag("shutdown: control socket unlinked, instance lock released");
    drop(lock);
    Ok(())
}
