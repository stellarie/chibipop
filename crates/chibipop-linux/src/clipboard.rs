//! This module owns the selection through the only Wayland protocol family that a daemon can use.
//! It does not start a `wl-copy` process.
//!
//! A normal clipboard write uses `wl_data_device`. The compositor honors it only for a client
//! that has keyboard focus on a surface. chibipop has neither condition. The popup sets
//! `keyboard_interactivity: none`, and OCR-to-clipboard starts from a global key while the
//! user's window has focus. Data control manages the selection without focus or a surface.
//! Therefore, this module uses data control instead of a `wl-copy` subprocess.
//!
//! Two rungs exist. The first advertised rung wins:
//!
//! 1. `ext_data_control_manager_v1` is the staged, non-deprecated protocol. It exists on
//!    Hyprland ≥ 0.48, sway ≥ 1.11, KWin ≥ 6.3, and niri.
//! 2. `zwlr_data_control_manager_v1` is the original wlr protocol. Every compositor that
//!    implements rung 1 also advertises it. Older compositors advertise it as well. Its XML
//!    marks it deprecated. This protocol remains the second rung. A session that drops it
//!    but keeps `ext` still has clipboard support.
//!
//! If the compositor advertises neither global, this module reports a **state**.
//! [`Clipboard::bind`] finds this state as [`Popup::bind`] finds an absent layer shell.
//! It emits one diagnostic with both globals, shows an honest line in the settings row,
//! and leaves every other channel unchanged.
//!
//! **Its own connection and thread.** A selection owner must answer a `send` event whenever
//! another client pastes. The owner must answer while it holds the offer. A client that does not
//! answer loses the selection. The daemon's queue lives inside calloop's `WaylandSource`.
//! A source callback cannot dispatch that queue because of the constraint that `select::Selector`
//! documents. Therefore, this module uses a second client for the compositor: one connection,
//! one calloop loop, and one thread for the daemon's lifetime. The pump gives this thread bytes
//! and receives its notes through `calloop::channel`. This arrangement follows the `spawn_anki`
//! bargain. The pump never blocks
//! (ARCHITECTURE.md#workspace-and-seams).
//!
//! **What this client reads.** Data control announces both CLIPBOARD and PRIMARY selections.
//! This module destroys every CLIPBOARD offer without reading it. It retains only the current
//! PRIMARY offer and reads it after an explicit selected-text action. The source application
//! controls that offer's lifetime. Wayland cannot prove that its highlight remains visible.

use crate::wayland::Advertised;
use anyhow::{Context, Result};
use chibipop::controller::RequestId;
use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_device_v1::{
    self as ext_device, ExtDataControlDeviceV1,
};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_manager_v1::ExtDataControlManagerV1;
use wayland_protocols::ext::data_control::v1::client::ext_data_control_offer_v1::ExtDataControlOfferV1;
use wayland_protocols::ext::data_control::v1::client::ext_data_control_source_v1::{
    self as ext_source, ExtDataControlSourceV1,
};
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_device_v1::{
    self as wlr_device, ZwlrDataControlDeviceV1,
};
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_manager_v1::ZwlrDataControlManagerV1;
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_offer_v1::ZwlrDataControlOfferV1;
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_source_v1::{
    self as wlr_source, ZwlrDataControlSourceV1,
};

/// The manager global of each rung, in ladder order.
pub const EXT_MANAGER: &str = "ext_data_control_manager_v1";
pub const WLR_MANAGER: &str = "zwlr_data_control_manager_v1";

/// MIME types for a UTF-8 text selection.
///
/// A Wayland reader asks for the first type. An XWayland bridge and older
/// toolkits ask for the four legacy names. This module offers all five.
/// Each extra name costs one request. `wl-clipboard` offers the same set,
/// so a paste behaves the same with either protocol.
pub const TEXT_MIMES: [&str; 5] =
    ["text/plain;charset=utf-8", "text/plain", "TEXT", "STRING", "UTF8_STRING"];

const PRIMARY_MIMES: [&str; 3] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];
const PRIMARY_MAX_BYTES: usize = 65_536;
pub const PRIMARY_TIMEOUT: Duration = Duration::from_secs(2);

/// The data-control protocol that serves this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// Rung 1: `ext-data-control-v1`, the staged protocol.
    Ext,
    /// Rung 2: `wlr-data-control-unstable-v1`, the deprecated but universal protocol.
    Wlr,
}

impl Rung {
    /// The manager global that this rung binds.
    pub fn global(self) -> &'static str {
        match self {
            Rung::Ext => EXT_MANAGER,
            Rung::Wlr => WLR_MANAGER,
        }
    }
}

/// The highest advertised rung for this session.
///
/// This pure function is the only place that defines ladder order. `bind` and
/// the settings window use it without a selection request.
pub fn rung(globals: &[Advertised]) -> Option<Rung> {
    let has = |interface: &str| globals.iter().any(|g| g.interface == interface);
    if has(EXT_MANAGER) {
        Some(Rung::Ext)
    } else if has(WLR_MANAGER) {
        Some(Rung::Wlr)
    } else {
        None
    }
}

/// The line for a session with no rung. It names both globals so a compositor
/// upgrade can make the clipboard available
/// (ARCHITECTURE.md#capture-and-masking).
pub fn unavailable_line() -> String {
    format!(
        "clipboard: unavailable - this compositor advertises neither {EXT_MANAGER} nor \
         {WLR_MANAGER}, so chibipop has no clipboard protocol it can use here; every other \
         channel keeps running"
    )
}

/// The destination for clipboard-thread diagnostics.
///
/// The pump owns the log (ARCHITECTURE.md#platform-integration). This thread
/// does not own the log, so it sends each line to the pump. This matches
/// AnkiConnect failures.
#[derive(Clone)]
struct Notes(calloop::channel::Sender<String>);

impl Notes {
    fn note(&self, line: String) {
        // If the pump has gone away, this thread does not report the error.
        // The process will end this thread at shutdown.
        let _ = self.0.send(line);
    }
}

/// One copy for the offer thread. It contains the bytes and an optional
/// receipt for the caller.
struct Take {
    payload: Arc<[u8]>,
    /// `Some` applies only to [`Clipboard::set_and_settle`]. The thread sends
    /// the receipt after the compositor receives the offer. It drops the
    /// receipt if the roundtrip fails. A caller that waits then learns whether
    /// the selection became active.
    settled: Option<mpsc::SyncSender<()>>,
}

enum ClipboardCommand {
    Take(Take),
    Read(ReadRequest),
}

struct ReadRequest {
    id: RequestId,
    deadline: Instant,
}

struct ReadDone {
    id: RequestId,
    generation: u64,
    text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActiveRead {
    id: RequestId,
    deadline: Instant,
}

pub struct SelectedText {
    pub id: RequestId,
    pub text: Option<String>,
}

/// The daemon's selection client.
pub struct Clipboard {
    rung: Rung,
    primary_supported: bool,
    /// The offer thread's inbox. Its receiver lives in that thread's loop, so
    /// each send also wakes the loop.
    commands: calloop::channel::Sender<ClipboardCommand>,
}

impl Clipboard {
    /// Open a data-control connection and start its thread.
    ///
    /// `Ok(None)` reports an honest absence. The session advertises no
    /// data-control global. This is a state, not a failure, like the absent
    /// layer shell that `Popup::bind` handles.
    /// `Err` reports a real setup failure. Examples include no display, a
    /// global that vanished between the probe and bind, and no thread.
    ///
    /// `notes` receives diagnostics from the thread and sends them to the pump.
    pub fn bind(
        globals: &[Advertised],
        notes: calloop::channel::Sender<String>,
    ) -> Result<Option<Clipboard>> {
        let (selected, _selected_rx) = calloop::channel::channel::<SelectedText>();
        Self::bind_with_selected(globals, notes, selected)
    }

    /// Bind with PRIMARY results.
    pub fn bind_with_selected(
        globals: &[Advertised],
        notes: calloop::channel::Sender<String>,
        selected: calloop::channel::Sender<SelectedText>,
    ) -> Result<Option<Clipboard>> {
        let Some(rung) = rung(globals) else { return Ok(None) };
        let notes = Notes(notes);

        // This connection belongs to this thread for the reason in the module
        // documentation.
        let conn = Connection::connect_to_env()
            .context("connecting the clipboard's own Wayland display")?;
        let mut queue = conn.new_event_queue::<Owner>();
        let qh = queue.handle();
        let registry = conn.display().get_registry(&qh, ());

        let manager_global = globals
            .iter()
            .find(|g| g.interface == rung.global())
            .with_context(|| {
                format!("{} vanished between the probe and the bind", rung.global())
            })?;
        // Use version 1 deliberately. `set_selection` is all this daemon needs.
        // Version 2 of the wlr rung adds the *primary* selection. It adds an
        // announcement stream whose offers this client would only destroy.
        // The selection belongs to a seat. This daemon takes the first seat that
        // the session advertises. This is the same seat that supplies the pointer
        // and keyboard for a pick (`App::seat`). A multi-seat session is outside
        // every channel's model here.
        let seat_global = globals
            .iter()
            .find(|g| g.interface == "wl_seat")
            .context("this session advertises no wl_seat to own a selection on")?;
        let seat = registry.bind::<WlSeat, _, Owner>(seat_global.name, 1, &qh, ());
        let manager_version = manager_global.version;
        let clip = Clip::bind(rung, manager_version, &registry, manager_global.name, &seat, &qh);
        let primary_supported = supports_primary(rung, manager_version);

        let mut owner = Owner {
            conn: conn.clone(),
            clip,
            source: None,
            primary: None,
            primary_generation: 0,
            active_read: None,
            ext_offers: Vec::new(),
            wlr_offers: Vec::new(),
            selected,
            notes,
            finished: false,
        };
        // Use one roundtrip before the thread starts. A refused bind returns
        // `Err` here instead of a silent thread. The roundtrip also
        // delivers the device's first `selection` event. Its offer handler
        // destroys that offer, so startup exercises the no-read rule.
        queue
            .roundtrip(&mut owner)
            .with_context(|| format!("binding {} on its own connection", rung.global()))?;

        let (commands, inbox) = calloop::channel::channel::<ClipboardCommand>();
        std::thread::Builder::new()
            .name("chibipop-clipboard".to_string())
            .spawn(move || serve(conn, queue, owner, inbox))
            .context("spawning the clipboard thread")?;

        Ok(Some(Clipboard { rung, primary_supported, commands }))
    }

    /// The rung that serves this session, for diagnostics.
    pub fn rung(&self) -> Rung {
        self.rung
    }

    /// PRIMARY availability.
    pub fn primary_supported(&self) -> bool {
        self.primary_supported
    }

    /// Read PRIMARY once.
    pub fn read_primary(&self, id: RequestId) -> Result<()> {
        let deadline = Instant::now()
            .checked_add(PRIMARY_TIMEOUT)
            .context("computing the PRIMARY deadline")?;
        self.commands.send(ClipboardCommand::Read(ReadRequest { id, deadline })).map_err(|_| {
            anyhow::anyhow!("the clipboard thread has ended; PRIMARY was not read")
        })
    }

    /// Take the selection with `text`.
    ///
    /// This returns when the thread queues the bytes. The offer thread serves the
    /// offer while the daemon owns it.
    /// This call does not block the pump. The selection survives after this call
    /// returns. An `Err` means that the offer thread has stopped receiving commands.
    /// The caller reports this error and does not retry.
    pub fn set(&self, text: &str) -> Result<()> {
        self.copy(text, None)
    }

    /// Take the selection with `text` and return after the compositor receives
    /// the offer.
    ///
    /// [`Clipboard::set`] only promises that the thread queues the bytes. It
    /// provides no waitable event for the caller. The thread still must build the
    /// source and put `set_selection` on the wire. `clipboard-check` is the caller
    /// that needs to tell a reader that the selection is ready. This method lets it
    /// report "selection taken" only after the compositor processes the request.
    pub fn set_and_settle(&self, text: &str) -> Result<()> {
        // Use a channel with depth one. The thread sends the receipt and continues.
        // No other code waits on this channel.
        let (settled, taken) = mpsc::sync_channel::<()>(1);
        self.copy(text, Some(settled))?;
        taken.recv().map_err(|_| {
            anyhow::anyhow!("the compositor never took the selection; see the clipboard's notes")
        })
    }

    fn copy(&self, text: &str, settled: Option<mpsc::SyncSender<()>>) -> Result<()> {
        let payload: Arc<[u8]> = Arc::from(text.as_bytes());
        self.commands.send(ClipboardCommand::Take(Take { payload, settled })).map_err(|_| {
            anyhow::anyhow!("the clipboard thread has ended; the selection was not taken")
        })
    }
}

/// The clipboard thread's complete state: the bound clip and the source that
/// currently owns the selection.
struct Owner {
    /// This thread's own connection supports the one operation that the event
    /// queue inside calloop's source cannot call from a callback. That operation is
    /// a roundtrip. [`Connection::roundtrip`] dispatches no events, so its
    /// events wait for calloop to pass them on as usual.
    conn: Connection,
    clip: Clip,
    /// `None` before the first copy and after the compositor cancels ours.
    /// Another client owns the selection in that normal state.
    source: Option<Source>,
    primary: Option<PrimaryOffer>,
    primary_generation: u64,
    active_read: Option<ActiveRead>,
    ext_offers: Vec<ExtOffer>,
    wlr_offers: Vec<WlrOffer>,
    selected: calloop::channel::Sender<SelectedText>,
    notes: Notes,
    /// The compositor retired the device (`finished`). No copy can succeed,
    /// so the loop stops and the next `set` fails.
    finished: bool,
}

impl Owner {
    /// Offer a copy's bytes and take the selection with them.
    fn take(&mut self, copy: Take, qh: &QueueHandle<Owner>) {
        // Store the payload in the source's user data, not in this struct.
        // The protocol forbids source reuse after `set_selection`.
        // Each copy therefore needs a new source.
        // Each source answers a `send` with its own bytes.
        // A replaced source can still have a `send` event in flight.
        let source = self.clip.publish(copy.payload, qh);
        // Call `destroy` only after `set_selection`. Requests keep wire order.
        // The compositor moves the selection to the new source before it reads
        // this destroy request.
        if let Some(old) = self.source.replace(source) {
            old.destroy();
        }
        let Some(settled) = copy.settled else {
            // No caller waits for this receipt. The event loop flushes the request
            // before it sleeps.
            return;
        };
        // A caller waits for this receipt. It needs the compositor to process the
        // request, not only this process to write it. The roundtrip provides that
        // point. If the compositor does not answer, the dropped receipt tells the
        // caller the selection never became active.
        match self.conn.roundtrip() {
            Ok(_) => {
                let _ = settled.send(());
            }
            Err(e) => self
                .notes
                .note(format!("clipboard: the compositor did not answer the offer - {e}")),
        }
    }

    /// Answer one `send` event. The compositor sends this event when a client
    /// pastes.
    ///
    /// This method uses a separate thread because a reader can open the pipe
    /// and never drain it. Such a reader would block this thread and delay every
    /// later copy. One temporary thread per paste matches the `spawn_anki`
    /// bargain at a lower rate. Users start pastes by hand.
    fn answer(&self, payload: &Arc<[u8]>, mime: &str, fd: OwnedFd) {
        let payload = Arc::clone(payload);
        let spawned = std::thread::Builder::new()
            .name("chibipop-clip-send".to_string())
            .spawn(move || {
                let mut pipe = std::fs::File::from(fd);
                // If the reader closes the pipe early, it can receive only a prefix of the
                // payload. Dropping the file closes the write end and sends EOF.
                let _ = pipe.write_all(&payload);
            });
        if let Err(e) = spawned {
            self.notes.note(format!("clipboard: no thread to answer a {mime} paste - {e}"));
        }
    }

    fn read_primary(
        &mut self,
        request: ReadRequest,
        finished: &calloop::channel::Sender<ReadDone>,
    ) {
        let ReadRequest { id, deadline } = request;
        let active = ActiveRead { id, deadline };
        if !start_read(&mut self.active_read, active) {
            let _ = self.selected.send(SelectedText { id, text: None });
            return;
        }
        if deadline <= Instant::now() {
            self.complete(ReadDone { id, generation: self.primary_generation, text: None });
            return;
        }
        let generation = self.primary_generation;
        let Some(primary) = self.primary.as_ref() else {
            self.complete(ReadDone { id, generation, text: None });
            return;
        };
        let Some(mime) = primary_mime(primary.mimes()) else {
            self.complete(ReadDone { id, generation, text: None });
            return;
        };
        let Ok((reader, writer)) = UnixStream::pair() else {
            self.complete(ReadDone { id, generation, text: None });
            return;
        };
        primary.receive(mime, writer.as_fd());
        if self.conn.flush().is_err() {
            self.complete(ReadDone { id, generation, text: None });
            return;
        }
        let finished = finished.clone();
        let spawned = std::thread::Builder::new()
            .name("chibipop-primary-read".to_string())
            .spawn(move || {
                let text = read_bounded(reader, deadline);
                let _ = finished.send(ReadDone { id, generation, text });
            });
        if spawned.is_err() {
            self.complete(ReadDone { id, generation, text: None });
        }
    }

    fn complete(&mut self, result: ReadDone) {
        let Some(active) = self.active_read.filter(|active| active.id == result.id) else {
            return;
        };
        self.active_read = None;
        let text = if Instant::now() <= active.deadline {
            accept_generation(self.primary_generation, result.generation, result.text)
        } else {
            None
        };
        let _ = self.selected.send(SelectedText { id: result.id, text });
    }

    fn fail_active(&mut self) {
        let Some(id) = retire_active(&mut self.active_read) else { return };
        let _ = self.selected.send(SelectedText { id, text: None });
    }

    fn ext_announced(&mut self, offer: ExtDataControlOfferV1) {
        self.ext_offers.push(ExtOffer { offer, mimes: Vec::new() });
    }

    fn wlr_announced(&mut self, offer: ZwlrDataControlOfferV1) {
        self.wlr_offers.push(WlrOffer { offer, mimes: Vec::new() });
    }

    fn ext_mime(&mut self, offer: &ExtDataControlOfferV1, mime: String) {
        if let Some(pending) = self.ext_offers.iter_mut().find(|item| item.offer.id() == offer.id()) {
            pending.mimes.push(mime);
        }
    }

    fn wlr_mime(&mut self, offer: &ZwlrDataControlOfferV1, mime: String) {
        if let Some(pending) = self.wlr_offers.iter_mut().find(|item| item.offer.id() == offer.id()) {
            pending.mimes.push(mime);
        }
    }

    fn ext_primary(&mut self, offer: Option<ExtDataControlOfferV1>) {
        let primary = offer.map(|offer| {
            let pending = take_ext(&mut self.ext_offers, &offer)
                .unwrap_or(ExtOffer { offer, mimes: Vec::new() });
            PrimaryOffer::Ext(pending)
        });
        self.replace_primary(primary);
    }

    fn wlr_primary(&mut self, offer: Option<ZwlrDataControlOfferV1>) {
        let primary = offer.map(|offer| {
            let pending = take_wlr(&mut self.wlr_offers, &offer)
                .unwrap_or(WlrOffer { offer, mimes: Vec::new() });
            PrimaryOffer::Wlr(pending)
        });
        self.replace_primary(primary);
    }

    fn replace_primary(&mut self, primary: Option<PrimaryOffer>) {
        self.primary_generation = self.primary_generation.wrapping_add(1);
        if let Some(old) = std::mem::replace(&mut self.primary, primary) {
            old.destroy();
        }
    }

    fn discard_ext(&mut self, offer: ExtDataControlOfferV1) {
        let _ = take_ext(&mut self.ext_offers, &offer);
        offer.destroy();
    }

    fn discard_wlr(&mut self, offer: ZwlrDataControlOfferV1) {
        let _ = take_wlr(&mut self.wlr_offers, &offer);
        offer.destroy();
    }

    /// The compositor cancelled one of this client's sources. Another client
    /// owns the selection now. This is a state, not a failure.
    fn cancelled(&mut self, source: &Source) {
        if self.source.as_ref().is_some_and(|s| s.is(source)) {
            if let Some(ours) = self.source.take() {
                ours.destroy();
            }
        }
    }

    /// The compositor retired the device. No further copy can succeed.
    /// Report this state once, then end the loop. A later `set` fails with the
    /// real reason instead of a send when no thread exists.
    fn retired(&mut self) {
        self.fail_active();
        self.notes.note(
            "clipboard: the compositor retired this data-control device; copies will be \
             refused until the daemon restarts"
                .to_string(),
        );
        self.finished = true;
    }
}

/// The manager and device for each rung. The enum keeps both proxies in the
/// same protocol family, so a source and a device cannot belong to different
/// protocols.
enum Clip {
    Ext { manager: ExtDataControlManagerV1, device: ExtDataControlDeviceV1 },
    Wlr { manager: ZwlrDataControlManagerV1, device: ZwlrDataControlDeviceV1 },
}

enum Source {
    Ext(ExtDataControlSourceV1),
    Wlr(ZwlrDataControlSourceV1),
}

struct ExtOffer {
    offer: ExtDataControlOfferV1,
    mimes: Vec<String>,
}

struct WlrOffer {
    offer: ZwlrDataControlOfferV1,
    mimes: Vec<String>,
}

enum PrimaryOffer {
    Ext(ExtOffer),
    Wlr(WlrOffer),
}

impl PrimaryOffer {
    fn mimes(&self) -> &[String] {
        match self {
            PrimaryOffer::Ext(offer) => &offer.mimes,
            PrimaryOffer::Wlr(offer) => &offer.mimes,
        }
    }

    fn receive(&self, mime: &str, fd: BorrowedFd<'_>) {
        match self {
            PrimaryOffer::Ext(offer) => offer.offer.receive(mime.to_string(), fd),
            PrimaryOffer::Wlr(offer) => offer.offer.receive(mime.to_string(), fd),
        }
    }

    fn destroy(self) {
        match self {
            PrimaryOffer::Ext(offer) => offer.offer.destroy(),
            PrimaryOffer::Wlr(offer) => offer.offer.destroy(),
        }
    }
}

impl Clip {
    fn bind(
        rung: Rung,
        advertised_version: u32,
        registry: &WlRegistry,
        name: u32,
        seat: &WlSeat,
        qh: &QueueHandle<Owner>,
    ) -> Clip {
        match rung {
            Rung::Ext => {
                let manager =
                    registry.bind::<ExtDataControlManagerV1, _, Owner>(name, 1, qh, ());
                let device = manager.get_data_device(seat, qh, ());
                Clip::Ext { manager, device }
            }
            Rung::Wlr => {
                let version = advertised_version.min(2);
                let manager =
                    registry.bind::<ZwlrDataControlManagerV1, _, Owner>(name, version, qh, ());
                let device = manager.get_data_device(seat, qh, ());
                Clip::Wlr { manager, device }
            }
        }
    }

    /// Create a source for `payload`, offer every text MIME, and take the selection with it.
    fn publish(&self, payload: Arc<[u8]>, qh: &QueueHandle<Owner>) -> Source {
        match self {
            Clip::Ext { manager, device } => {
                let source = manager.create_data_source(qh, payload);
                for mime in TEXT_MIMES {
                    source.offer(mime.to_string());
                }
                device.set_selection(Some(&source));
                Source::Ext(source)
            }
            Clip::Wlr { manager, device } => {
                let source = manager.create_data_source(qh, payload);
                for mime in TEXT_MIMES {
                    source.offer(mime.to_string());
                }
                device.set_selection(Some(&source));
                Source::Wlr(source)
            }
        }
    }
}

impl Source {
    fn destroy(self) {
        match self {
            Source::Ext(s) => s.destroy(),
            Source::Wlr(s) => s.destroy(),
        }
    }

    /// Whether both values refer to the same protocol object.
    fn is(&self, other: &Source) -> bool {
        match (self, other) {
            (Source::Ext(a), Source::Ext(b)) => a.id() == b.id(),
            (Source::Wlr(a), Source::Wlr(b)) => a.id() == b.id(),
            _ => false,
        }
    }
}

/// Run the clipboard thread until the compositor retires the device or the
/// daemon drops its sender.
///
/// Use a calloop loop instead of bare `blocking_dispatch`. This thread must
/// wait for compositor events and pump bytes at the same time. This crate
/// already uses this shape in `capture/mod.rs`.
fn serve(
    conn: Connection,
    queue: EventQueue<Owner>,
    mut owner: Owner,
    inbox: calloop::channel::Channel<ClipboardCommand>,
) {
    let events: calloop::EventLoop<'static, Owner> = match calloop::EventLoop::try_new() {
        Ok(events) => events,
        Err(e) => {
            owner.notes.note(format!("clipboard: no event loop for the offer thread - {e}"));
            return;
        }
    };
    let handle = events.handle();
    let qh = queue.handle();
    if let Err(e) = calloop_wayland_source::WaylandSource::new(conn, queue).insert(handle.clone()) {
        owner.notes.note(format!("clipboard: registering the offer connection failed - {e}"));
        return;
    }
    let (finished, completions) = calloop::channel::channel::<ReadDone>();
    let read_finished = finished.clone();
    let inserted = handle.insert_source(inbox, move |event, _, owner: &mut Owner| match event {
        calloop::channel::Event::Msg(ClipboardCommand::Take(copy)) => owner.take(copy, &qh),
        calloop::channel::Event::Msg(ClipboardCommand::Read(request)) => {
            owner.read_primary(request, &read_finished)
        }
        // The daemon dropped its sender. The process will stop.
        calloop::channel::Event::Closed => owner.finished = true,
    });
    if let Err(e) = inserted {
        owner.notes.note(format!("clipboard: registering the offer inbox failed - {e}"));
        return;
    }
    if let Err(e) = handle.insert_source(completions, |event, _, owner: &mut Owner| {
        if let calloop::channel::Event::Msg(result) = event {
            owner.complete(result);
        }
    }) {
        owner.notes.note(format!("clipboard: registering the PRIMARY result failed - {e}"));
        return;
    }

    let signal = events.get_signal();
    let mut events = events;
    // No timeout. This thread sleeps until the compositor or pump sends an
    // event (the idle budget, ARCHITECTURE.md#hover-cadence).
    let ran = events.run(None, &mut owner, |owner| {
        if owner.finished {
            signal.stop();
        }
    });
    owner.fail_active();
    if let Err(e) = ran {
        owner.notes.note(format!("clipboard: the offer thread stopped - {e}"));
    }
}

// ---- dispatch ----

/// The registry. This connection binds globals by name from the daemon's
/// probe and does not watch for changes, so no event needs a handler.
impl Dispatch<WlRegistry, ()> for Owner {
    fn event(
        _: &mut Owner,
        _: &WlRegistry,
        _: wayland_client::protocol::wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Owner>,
    ) {
    }
}

// Every proxy below takes `ignore`. A bare `delegate_noop!` panics on the
// first event, and the daemon must not die on a protocol event it does not
// use.
// The seat. This client uses it only to request `get_data_device`. It ignores
// the seat capabilities and other seat events.
wayland_client::delegate_noop!(Owner: ignore WlSeat);
// The managers. Neither protocol defines a manager event.
wayland_client::delegate_noop!(Owner: ignore ExtDataControlManagerV1);
wayland_client::delegate_noop!(Owner: ignore ZwlrDataControlManagerV1);

impl Dispatch<ExtDataControlDeviceV1, ()> for Owner {
    // Opcode 0 is `data_offer`. It is the only event that creates a child.
    wayland_client::event_created_child!(Owner, ExtDataControlDeviceV1, [
        0 => (ExtDataControlOfferV1, ()),
    ]);

    fn event(
        owner: &mut Owner,
        _: &ExtDataControlDeviceV1,
        event: ext_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Owner>,
    ) {
        match event {
            // Await its selection kind.
            ext_device::Event::DataOffer { id } => owner.ext_announced(id),
            // Never read CLIPBOARD.
            ext_device::Event::Selection { id: Some(offer) } => owner.discard_ext(offer),
            ext_device::Event::PrimarySelection { id } => owner.ext_primary(id),
            ext_device::Event::Finished => owner.retired(),
            _ => {}
        }
    }
}

impl Dispatch<ZwlrDataControlDeviceV1, ()> for Owner {
    wayland_client::event_created_child!(Owner, ZwlrDataControlDeviceV1, [
        0 => (ZwlrDataControlOfferV1, ()),
    ]);

    fn event(
        owner: &mut Owner,
        _: &ZwlrDataControlDeviceV1,
        event: wlr_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Owner>,
    ) {
        match event {
            wlr_device::Event::DataOffer { id } => owner.wlr_announced(id),
            wlr_device::Event::Selection { id: Some(offer) } => owner.discard_wlr(offer),
            wlr_device::Event::PrimarySelection { id } => owner.wlr_primary(id),
            wlr_device::Event::Finished => owner.retired(),
            _ => {}
        }
    }
}

// MIME events precede selection kind.
impl Dispatch<ExtDataControlOfferV1, ()> for Owner {
    fn event(
        owner: &mut Owner,
        offer: &ExtDataControlOfferV1,
        event: wayland_protocols::ext::data_control::v1::client::ext_data_control_offer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Owner>,
    ) {
        if let wayland_protocols::ext::data_control::v1::client::ext_data_control_offer_v1::Event::Offer { mime_type } = event {
            owner.ext_mime(offer, mime_type);
        }
    }
}

fn take_ext(
    offers: &mut Vec<ExtOffer>,
    offer: &ExtDataControlOfferV1,
) -> Option<ExtOffer> {
    let index = offers.iter().position(|item| item.offer.id() == offer.id())?;
    Some(offers.remove(index))
}

fn take_wlr(
    offers: &mut Vec<WlrOffer>,
    offer: &ZwlrDataControlOfferV1,
) -> Option<WlrOffer> {
    let index = offers.iter().position(|item| item.offer.id() == offer.id())?;
    Some(offers.remove(index))
}

fn primary_mime(mimes: &[String]) -> Option<&str> {
    PRIMARY_MIMES
        .into_iter()
        .find(|preferred| mimes.iter().any(|offered| offered == preferred))
}

fn supports_primary(rung: Rung, advertised_version: u32) -> bool {
    rung == Rung::Ext || advertised_version >= 2
}

fn read_bounded(mut reader: UnixStream, deadline: Instant) -> Option<String> {
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        reader.set_read_timeout(Some(remaining)).ok()?;
        let mut chunk = [0u8; 8_192];
        let limit = PRIMARY_MAX_BYTES.saturating_add(1).saturating_sub(bytes.len());
        let amount = chunk.len().min(limit);
        let read = reader.read(&mut chunk[..amount]).ok()?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > PRIMARY_MAX_BYTES {
            return None;
        }
    }
    let text = String::from_utf8(bytes).ok()?;
    (!text.trim().is_empty()).then_some(text)
}

fn retire_active(active: &mut Option<ActiveRead>) -> Option<RequestId> {
    active.take().map(|read| read.id)
}

fn start_read(active: &mut Option<ActiveRead>, read: ActiveRead) -> bool {
    if active.is_some() {
        return false;
    }
    *active = Some(read);
    true
}

fn accept_generation(current: u64, requested: u64, text: Option<String>) -> Option<String> {
    (current == requested).then_some(text).flatten()
}

impl Dispatch<ZwlrDataControlOfferV1, ()> for Owner {
    fn event(
        owner: &mut Owner,
        offer: &ZwlrDataControlOfferV1,
        event: wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_offer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Owner>,
    ) {
        if let wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_offer_v1::Event::Offer { mime_type } = event {
            owner.wlr_mime(offer, mime_type);
        }
    }
}

/// The source that owns the selection. Its user data is the payload, so a
/// `send` event answers with the bytes that created the source.
impl Dispatch<ExtDataControlSourceV1, Arc<[u8]>> for Owner {
    fn event(
        owner: &mut Owner,
        source: &ExtDataControlSourceV1,
        event: ext_source::Event,
        payload: &Arc<[u8]>,
        _: &Connection,
        _: &QueueHandle<Owner>,
    ) {
        match event {
            ext_source::Event::Send { mime_type, fd } => owner.answer(payload, &mime_type, fd),
            ext_source::Event::Cancelled => {
                owner.cancelled(&Source::Ext(source.clone()));
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrDataControlSourceV1, Arc<[u8]>> for Owner {
    fn event(
        owner: &mut Owner,
        source: &ZwlrDataControlSourceV1,
        event: wlr_source::Event,
        payload: &Arc<[u8]>,
        _: &Connection,
        _: &QueueHandle<Owner>,
    ) {
        match event {
            wlr_source::Event::Send { mime_type, fd } => owner.answer(payload, &mime_type, fd),
            wlr_source::Event::Cancelled => {
                owner.cancelled(&Source::Wlr(source.clone()));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advertised(interfaces: &[&str]) -> Vec<Advertised> {
        interfaces
            .iter()
            .enumerate()
            .map(|(i, interface)| Advertised {
                name: i as u32 + 1,
                interface: (*interface).to_string(),
                version: 1,
            })
            .collect()
    }

    /// The staged protocol wins when both globals exist. The wlr protocol XML
    /// marks that protocol as deprecated, so this code uses it only as a fallback.
    #[test]
    fn the_staged_protocol_outranks_the_deprecated_wlr_one_where_both_are_advertised() {
        assert_eq!(
            Some(Rung::Ext),
            rung(&advertised(&["wl_seat", WLR_MANAGER, EXT_MANAGER]))
        );
    }

    /// A session with only the wlr rung still copies. Most compositors use this
    /// path today.
    #[test]
    fn the_wlr_rung_serves_a_session_that_advertises_only_it() {
        assert_eq!(Some(Rung::Wlr), rung(&advertised(&["wl_seat", WLR_MANAGER])));
        assert_eq!(WLR_MANAGER, Rung::Wlr.global());
        assert_eq!(EXT_MANAGER, Rung::Ext.global());
    }

    /// Stock GNOME advertises no rung, which is a state. `bind` returns
    /// `Ok(None)` and opens no connection.
    #[test]
    fn a_session_with_neither_global_has_no_rung_and_binds_to_nothing() {
        let globals = advertised(&["wl_seat", "wl_shm", "zwlr_layer_shell_v1"]);
        assert_eq!(None, rung(&globals));
        let (tx, _rx) = calloop::channel::channel::<String>();
        let (selected, _selected_rx) = calloop::channel::channel::<SelectedText>();
        assert!(
            Clipboard::bind_with_selected(&globals, tx, selected)
                .expect("an absent protocol is not an error")
                .is_none(),
            "no rung must be a state, not a Clipboard"
        );
    }

    /// The refusal names both globals. A compositor upgrade can make the
    /// clipboard available without a code change
    /// (ARCHITECTURE.md#capture-and-masking).
    #[test]
    fn the_unavailable_line_names_both_globals_it_looked_for() {
        let line = unavailable_line();
        assert!(line.contains(EXT_MANAGER), "{line}");
        assert!(line.contains(WLR_MANAGER), "{line}");
    }

    /// A reader asks for `text/plain;charset=utf-8` first. This array must offer
    /// it first, followed by the legacy X11 targets.
    #[test]
    fn the_offered_mime_types_lead_with_utf8_text_and_carry_the_x11_targets() {
        assert_eq!("text/plain;charset=utf-8", TEXT_MIMES[0]);
        for target in ["TEXT", "STRING", "UTF8_STRING"] {
            assert!(TEXT_MIMES.contains(&target), "{target} is what XWayland asks by");
        }
    }

    #[test]
    fn primary_support_requires_ext_v1_or_wlr_v2() {
        assert!(supports_primary(Rung::Ext, 1));
        assert!(!supports_primary(Rung::Wlr, 1));
        assert!(supports_primary(Rung::Wlr, 2));
    }

    #[test]
    fn primary_mime_uses_utf8_priority() {
        let mimes = vec!["text/plain".to_string(), "UTF8_STRING".to_string()];
        assert_eq!(Some("UTF8_STRING"), primary_mime(&mimes));
        let mimes = vec![
            "text/plain".to_string(),
            "UTF8_STRING".to_string(),
            "text/plain;charset=utf-8".to_string(),
        ];
        assert_eq!(Some("text/plain;charset=utf-8"), primary_mime(&mimes));
        assert_eq!(None, primary_mime(&["image/png".to_string()]));
    }

    fn read_bytes(bytes: Vec<u8>) -> Option<String> {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        let sending = std::thread::spawn(move || writer.write_all(&bytes));
        let result = read_bounded(reader, Instant::now() + Duration::from_secs(1));
        sending.join().unwrap().unwrap();
        result
    }

    #[test]
    fn primary_read_accepts_utf8_and_rejects_empty_invalid_or_oversized_bytes() {
        assert_eq!(Some("日本語".to_string()), read_bytes("日本語".as_bytes().to_vec()));
        assert_eq!(None, read_bytes(Vec::new()));
        assert_eq!(None, read_bytes(vec![0xff]));
        assert!(read_bytes(vec![b'a'; PRIMARY_MAX_BYTES]).is_some());
        assert_eq!(None, read_bytes(vec![b'a'; PRIMARY_MAX_BYTES + 1]));
    }

    #[test]
    fn primary_read_uses_one_overall_deadline() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        let sending = std::thread::spawn(move || {
            for _ in 0..6 {
                if writer.write_all(b"x").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        });
        assert_eq!(None, read_bounded(reader, Instant::now() + Duration::from_millis(70)));
        sending.join().unwrap();
    }

    #[test]
    fn an_expired_primary_deadline_reads_nothing() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(b"stale").unwrap();
        drop(writer);
        let deadline = Instant::now().checked_sub(Duration::from_millis(1)).unwrap();
        assert_eq!(None, read_bounded(reader, deadline));
    }

    #[test]
    fn changed_generation_rejects_text_and_unchanged_generation_can_repeat() {
        assert_eq!(None, accept_generation(8, 7, Some("old".to_string())));
        for _ in 0..2 {
            assert_eq!(Some("same".to_string()), accept_generation(8, 8, Some("same".to_string())));
        }
    }

    #[test]
    fn retiring_the_device_fails_the_active_request_once() {
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut active = Some(ActiveRead { id: RequestId(9), deadline });
        assert_eq!(Some(RequestId(9)), retire_active(&mut active));
        assert_eq!(None, retire_active(&mut active));
    }

    #[test]
    fn only_one_primary_read_can_wait() {
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut active = None;
        assert!(start_read(&mut active, ActiveRead { id: RequestId(1), deadline }));
        assert!(!start_read(&mut active, ActiveRead { id: RequestId(2), deadline }));
        assert_eq!(Some(RequestId(1)), retire_active(&mut active));
    }
}
