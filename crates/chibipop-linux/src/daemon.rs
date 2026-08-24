//! The daemon: calloop pump + instance lock + control socket + logging
//! (ADR-0001: all sync, calloop as the Linux pump). No capture, OCR, or
//! popup yet — those tickets plug into this loop.

use crate::control::{ControlSocket, StubState, Verb};
use crate::lock::{self, LockError};
use crate::logging::Log;
use crate::paths::Paths;
use crate::wayland;
use anyhow::{bail, Context, Result};
use calloop::generic::Generic;
use calloop::signals::{Signal, Signals};
use calloop::{EventLoop, Interest, LoopSignal, Mode, PostAction};
use calloop_wayland_source::WaylandSource;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::PathBuf;
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle};

/// The pump's shared state.
struct App {
    log: Log,
    stub: StubState,
    config_file: PathBuf,
    signal: LoopSignal,
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
    for line in wayland::report(&wayland::collect_globals(&conn)?) {
        log.diag(&line);
    }

    let socket = ControlSocket::bind(runtime_dir, &display)
        .with_context(|| format!("binding the control socket in {}", runtime_dir.display()))?;
    log.diag(&format!("control: listening on {}", socket.path().display()));

    let mut event_loop: EventLoop<App> = EventLoop::try_new().context("creating the event loop")?;

    // The long-lived Wayland queue; its own registry so future tickets
    // see dynamic global changes.
    let queue = conn.new_event_queue::<App>();
    let _registry = conn.display().get_registry(&queue.handle(), ());
    WaylandSource::new(conn.clone(), queue)
        .insert(event_loop.handle())
        .context("registering the Wayland source")?;

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

    let mut app = App {
        log,
        stub: StubState::default(),
        config_file: paths.config_file.clone(),
        signal: event_loop.get_signal(),
    };
    app.log.diag("ready: pump running (no capture/OCR/popup yet - bootstrap ticket 29)");

    event_loop.run(None, &mut app, |_| {}).context("running the event loop")?;

    // Dropping the loop drops the control source, which unlinks the
    // socket file; the lock file stays (see lock.rs) and the kernel
    // releases the flock when `lock` drops.
    drop(event_loop);
    app.log.diag("shutdown: control socket unlinked, instance lock released");
    drop(lock);
    Ok(())
}
