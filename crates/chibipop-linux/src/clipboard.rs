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
//! **What this client receives and does not read.** Data control makes its holder a clipboard
//! *manager*. The compositor announces every selection that any client sets, ours included.
//! The protocol requires this behavior, and the client cannot disable it. This module destroys
//! every announced offer on arrival and never sends `receive`. It never reads or logs another
//! application's clipboard content. The lookup log uses the same rule for screen content
//! (ARCHITECTURE.md#platform-integration).

use crate::wayland::Advertised;
use anyhow::{Context, Result};
use std::io::Write;
use std::os::fd::OwnedFd;
use std::sync::mpsc;
use std::sync::Arc;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_device_v1::{
    self as ext_device, ExtDataControlDeviceV1,
};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_manager_v1::ExtDataControlManagerV1;
use wayland_protocols::ext::data_control::v1::client::ext_data_control_offer_v1::{
    self as ext_offer, ExtDataControlOfferV1,
};
use wayland_protocols::ext::data_control::v1::client::ext_data_control_source_v1::{
    self as ext_source, ExtDataControlSourceV1,
};
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_device_v1::{
    self as wlr_device, ZwlrDataControlDeviceV1,
};
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_manager_v1::ZwlrDataControlManagerV1;
use wayland_protocols_wlr::data_control::v1::client::zwlr_data_control_offer_v1::{
    self as wlr_offer, ZwlrDataControlOfferV1,
};
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

/// The daemon's writable selection. It sends bytes to a thread that holds
/// the offer open.
pub struct Clipboard {
    rung: Rung,
    /// The offer thread's inbox. Its receiver lives in that thread's loop, so
    /// each send also wakes the loop.
    text: calloop::channel::Sender<Take>,
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
        let manager = Manager::bind(rung, &registry, manager_global.name, &qh);

        // The selection belongs to a seat. This daemon takes the first seat that
        // the session advertises. This is the same seat that supplies the pointer
        // and keyboard for a pick (`App::seat`). A multi-seat session is outside
        // every channel's model here.
        let seat_global = globals
            .iter()
            .find(|g| g.interface == "wl_seat")
            .context("this session advertises no wl_seat to own a selection on")?;
        let seat = registry.bind::<WlSeat, _, Owner>(seat_global.name, 1, &qh, ());
        let device = manager.device(&seat, &qh);

        let mut owner =
            Owner { conn: conn.clone(), manager, device, source: None, notes, finished: false };
        // Use one roundtrip before the thread starts. A refused bind returns
        // `Err` here instead of a silent thread. The roundtrip also
        // delivers the device's first `selection` event. Its offer handler
        // destroys that offer, so startup exercises the no-read rule.
        queue
            .roundtrip(&mut owner)
            .with_context(|| format!("binding {} on its own connection", rung.global()))?;

        let (text, inbox) = calloop::channel::channel::<Take>();
        std::thread::Builder::new()
            .name("chibipop-clipboard".to_string())
            .spawn(move || serve(conn, queue, owner, inbox))
            .context("spawning the clipboard thread")?;

        Ok(Some(Clipboard { rung, text }))
    }

    /// The rung that serves this session, for diagnostics.
    pub fn rung(&self) -> Rung {
        self.rung
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
        self.text.send(Take { payload, settled }).map_err(|_| {
            anyhow::anyhow!("the clipboard thread has ended; the selection was not taken")
        })
    }
}

/// The clipboard thread's complete state: the manager, the device, and the
/// source that currently owns the selection.
struct Owner {
    /// This thread's own connection supports the one operation that the event
    /// queue inside calloop's source cannot call from a callback. That operation is
    /// a roundtrip. [`Connection::roundtrip`] dispatches no events, so its
    /// events wait for calloop to pass them on as usual.
    conn: Connection,
    manager: Manager,
    device: Device,
    /// `None` before the first copy and after the compositor cancels ours.
    /// Another client owns the selection in that normal state.
    source: Option<Source>,
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
        let source = self.manager.source(copy.payload, qh);
        for mime in TEXT_MIMES {
            source.offer(mime);
        }
        self.device.set_selection(&source);
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
        self.notes.note(
            "clipboard: the compositor retired this data-control device; copies will be \
             refused until the daemon restarts"
                .to_string(),
        );
        self.finished = true;
    }
}

/// The manager for each rung. The three protocol enums avoid a trait object.
/// Both protocols provide the same operations, `bind` chooses one rung once,
/// and each `Dispatch` implementation uses a concrete interface.
enum Manager {
    Ext(ExtDataControlManagerV1),
    Wlr(ZwlrDataControlManagerV1),
}

enum Device {
    Ext(ExtDataControlDeviceV1),
    Wlr(ZwlrDataControlDeviceV1),
}

enum Source {
    Ext(ExtDataControlSourceV1),
    Wlr(ZwlrDataControlSourceV1),
}

impl Manager {
    fn bind(rung: Rung, registry: &WlRegistry, name: u32, qh: &QueueHandle<Owner>) -> Manager {
        match rung {
            Rung::Ext => {
                Manager::Ext(registry.bind::<ExtDataControlManagerV1, _, Owner>(name, 1, qh, ()))
            }
            Rung::Wlr => {
                Manager::Wlr(registry.bind::<ZwlrDataControlManagerV1, _, Owner>(name, 1, qh, ()))
            }
        }
    }

    fn device(&self, seat: &WlSeat, qh: &QueueHandle<Owner>) -> Device {
        match self {
            Manager::Ext(m) => Device::Ext(m.get_data_device(seat, qh, ())),
            Manager::Wlr(m) => Device::Wlr(m.get_data_device(seat, qh, ())),
        }
    }

    fn source(&self, payload: Arc<[u8]>, qh: &QueueHandle<Owner>) -> Source {
        match self {
            Manager::Ext(m) => Source::Ext(m.create_data_source(qh, payload)),
            Manager::Wlr(m) => Source::Wlr(m.create_data_source(qh, payload)),
        }
    }
}

impl Device {
    fn set_selection(&self, source: &Source) {
        match (self, source) {
            (Device::Ext(d), Source::Ext(s)) => d.set_selection(Some(s)),
            (Device::Wlr(d), Source::Wlr(s)) => d.set_selection(Some(s)),
            // One rung always pairs with one protocol. This arm cannot occur.
            // Do nothing instead of a panic in the daemon's clipboard path.
            _ => {}
        }
    }
}

impl Source {
    fn offer(&self, mime: &str) {
        match self {
            Source::Ext(s) => s.offer(mime.to_string()),
            Source::Wlr(s) => s.offer(mime.to_string()),
        }
    }

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
    inbox: calloop::channel::Channel<Take>,
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
    let inserted = handle.insert_source(inbox, move |event, _, owner: &mut Owner| match event {
        calloop::channel::Event::Msg(copy) => owner.take(copy, &qh),
        // The daemon dropped its sender. The process will stop.
        calloop::channel::Event::Closed => owner.finished = true,
    });
    if let Err(e) = inserted {
        owner.notes.note(format!("clipboard: registering the offer inbox failed - {e}"));
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

/// The seat. This client uses it only to request `get_data_device`. It ignores
/// the seat capabilities and other seat events.
impl Dispatch<WlSeat, ()> for Owner {
    fn event(
        _: &mut Owner,
        _: &WlSeat,
        _: wayland_client::protocol::wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Owner>,
    ) {
    }
}

impl Dispatch<ExtDataControlManagerV1, ()> for Owner {
    fn event(
        _: &mut Owner,
        _: &ExtDataControlManagerV1,
        _: <ExtDataControlManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Owner>,
    ) {
    }
}

impl Dispatch<ZwlrDataControlManagerV1, ()> for Owner {
    fn event(
        _: &mut Owner,
        _: &ZwlrDataControlManagerV1,
        _: <ZwlrDataControlManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Owner>,
    ) {
    }
}

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
            // The device announces this offer, but this client never reads it. See the
            // module documentation.
            ext_device::Event::DataOffer { id } => id.destroy(),
            // Destroy the offer even though this client never sends `receive`. `None`
            // means that the compositor cleared the selection and provided no object.
            ext_device::Event::Selection { id: Some(offer) }
            | ext_device::Event::PrimarySelection { id: Some(offer) } => offer.destroy(),
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
            wlr_device::Event::DataOffer { id } => id.destroy(),
            wlr_device::Event::Selection { id: Some(offer) }
            | wlr_device::Event::PrimarySelection { id: Some(offer) } => offer.destroy(),
            wlr_device::Event::Finished => owner.retired(),
            _ => {}
        }
    }
}

/// An announced offer. The device destroys it on arrival, so this handler
/// receives no event.
impl Dispatch<ExtDataControlOfferV1, ()> for Owner {
    fn event(
        _: &mut Owner,
        _: &ExtDataControlOfferV1,
        _: ext_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Owner>,
    ) {
    }
}

impl Dispatch<ZwlrDataControlOfferV1, ()> for Owner {
    fn event(
        _: &mut Owner,
        _: &ZwlrDataControlOfferV1,
        _: wlr_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Owner>,
    ) {
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
        assert!(
            Clipboard::bind(&globals, tx).expect("an absent protocol is not an error").is_none(),
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
}
