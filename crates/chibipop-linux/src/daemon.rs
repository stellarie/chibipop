//! The daemon: calloop pump + instance lock + control socket + logging
//! (ADR-0001: all sync, calloop as the Linux pump). No capture, OCR, or
//! popup yet — those tickets plug into this loop.

use crate::control::{ControlSocket, StubState, Verb};
use crate::cursor::{self, budget, hyprctl};
use crate::cursor::image_copy::{CursorHandler, CursorState};
use crate::settings::child::{self, SettingsChild, SpawnOutcome};
use crate::lock::{self, LockError};
use crate::logging::Log;
use crate::paths::Paths;
use crate::tray::status::{ChannelId, ChannelState, ChannelStatuses};
use crate::tray::{self, TrayHandle, TrayRequest};
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
    /// At most one settings child (ADR-0005), spawned from the tray's
    /// Settings item; the settings-scoped flock is the cross-process
    /// guard, this is the daemon's own.
    settings: SettingsChild,
    /// Channel health plus the SNI tray mirroring it (ADR-0006). Also
    /// the daemon's own view: it works unchanged when there is no tray.
    tray: TrayHandle,
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
    /// honors today: the lookup-log gate live, `popup.layer` logged for
    /// the popup ticket. The config file is the sole source of truth
    /// (ADR-0005); nothing structured crosses the socket.
    fn reload_config(&mut self) {
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
                self.log.diag(&format!(
                    "config: popup.layer = {} (takes effect when the popup lands)",
                    match config.popup_layer() {
                        chibipop::config::PopupLayer::Overlay => "overlay",
                        chibipop::config::PopupLayer::Top => "top",
                    }
                ));
            }
            Err(e) => self.log.diag(&format!("config: reload failed: {e:#}")),
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

    // The SNI tray (ADR-0006). It runs its own D-Bus thread and its
    // activations arrive here as `TrayRequest`s, so the pump stays sync.
    // Non-fatal by construction: `spawn` hands back diagnostics instead
    // of an error, because a trayless session is normal (stock GNOME,
    // bare Hyprland) and must cost nothing. The registry it carries is
    // the daemon's own view of channel health, tray or no tray.
    let (tray_tx, tray_rx) = calloop::channel::channel::<TrayRequest>();
    let (tray_handle, tray_diagnostics) = tray::spawn(ChannelStatuses::startup(&selection), tray_tx);
    for line in tray_diagnostics {
        log.diag(&line);
    }
    for row in tray_handle.statuses().rows() {
        log.diag(&format!("channel: {row}"));
    }

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
        settings: SettingsChild::new(),
        tray: tray_handle,
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

    app.log.diag(&format!(
        "ready: pump running (cursor channel wired; tray {}; no capture/OCR/popup yet)",
        if app.tray.is_connected() { "published" } else { "trayless" }
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
        let mut app = App {
            log: Log::open(&dir.join("chibipop.log"), false),
            stub: StubState::default(),
            config_file: config_file.clone(),
            signal: event_loop.get_signal(),
            settings: SettingsChild::new(),
            cursor: CursorState::default(),
            controller: Controller::new(controller_config(&chibipop::config::Config::default())),
            trace: false,
            last_poll: None,
            last_move: Instant::now(),
            tray: TrayHandle::trayless(ChannelStatuses::startup(&cursor::Selection::Rung(
                cursor::Rung::HyprctlPoll,
            ))),
        };

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

    fn test_app(dir: &std::path::Path, log_file: &std::path::Path, event_loop: &EventLoop<App>) -> App {
        App {
            log: Log::open(log_file, false),
            stub: StubState::default(),
            config_file: dir.join("chibipop.toml"),
            signal: event_loop.get_signal(),
            settings: SettingsChild::new(),
            cursor: CursorState::default(),
            controller: Controller::new(controller_config(&chibipop::config::Config::default())),
            trace: false,
            last_poll: None,
            last_move: Instant::now(),
            tray: TrayHandle::trayless(ChannelStatuses::startup(&cursor::Selection::Rung(
                cursor::Rung::HyprctlPoll,
            ))),
        }
    }
}
