//! The selection, on the one Wayland protocol family a daemon can use
//! (spec D2 — no `wl-copy` process dependency).
//!
//! An ordinary clipboard write goes through `wl_data_device`, which the
//! compositor only honours for a client holding keyboard focus on a
//! surface. chibipop has neither half: the popup is
//! `keyboard_interactivity: none` for good (ADR-0004), and
//! OCR-to-clipboard is invoked from a global key while the *user's*
//! window has focus. Data control is the protocol written for exactly
//! this case — managing the selection with no focus and no surface —
//! which is why it, and not a `wl-copy` subprocess, is the rung here.
//!
//! Two rungs, first advertised wins (ADR-0002/0003's shape):
//!
//! 1. `ext_data_control_manager_v1` — the staged, non-deprecated
//!    protocol (Hyprland ≥ 0.48, sway ≥ 1.11, KWin ≥ 6.3, niri).
//! 2. `zwlr_data_control_manager_v1` — the wlr original, advertised by
//!    every compositor that implements rung 1 and by every one that
//!    predates it. Its own XML calls it deprecated, which is why it is
//!    rung 2 rather than the only rung: a session that eventually drops
//!    it and keeps `ext` must not be told it has no clipboard, because
//!    that diagnostic would be a lie.
//!
//! Neither advertised — stock GNOME, where Mutter implements no data
//! control at all — is a **state**, discovered by [`Clipboard::bind`]
//! the way a missing layer shell is discovered by `Popup::bind`: one
//! diagnostic naming both globals, an honest line in the settings row,
//! and every other channel untouched.
//!
//! **Its own connection, on a thread of its own.** Owning a selection is
//! not a call that returns: the compositor asks for the bytes with a
//! `send` event every time some client pastes, for as long as we hold
//! the offer, and a client that stops answering loses it. The daemon's
//! queue lives inside calloop's `WaylandSource` and cannot be dispatched
//! from within a source callback (the constraint `select::Selector`
//! documents), so this is a second client as far as the compositor is
//! concerned: one connection, one calloop loop, one thread, alive for
//! the daemon's lifetime. The pump only ever hands it bytes and reads
//! its notes back, both over `calloop::channel` — the `spawn_anki`
//! bargain (ADR-0001: nothing blocking on the pump).
//!
//! **What this client is told, and does not read.** Data control makes
//! its holder a clipboard *manager*: the compositor announces every
//! selection any client sets, ours included. That is inherent to the
//! protocol and cannot be opted out of. Every announced offer is
//! destroyed on arrival and `receive` is never sent, so no other
//! application's clipboard content is ever read, let alone logged — the
//! same posture the lookup log takes towards screen content (ADR-0006).

use crate::wayland::Advertised;
use anyhow::{Context, Result};
use std::io::Write;
use std::os::fd::OwnedFd;
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

/// The MIME types a UTF-8 text selection is offered as.
///
/// The first is what a Wayland reader asks for; the four legacy names
/// are the X11 selection targets an XWayland bridge and older toolkits
/// still ask by, and offering them costs one request each. Same set
/// `wl-clipboard` offers, so a paste behaves identically whichever of
/// the two took the selection.
pub const TEXT_MIMES: [&str; 5] =
    ["text/plain;charset=utf-8", "text/plain", "TEXT", "STRING", "UTF8_STRING"];

/// Which data-control protocol serves this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// Rung 1: `ext-data-control-v1`, the staged protocol.
    Ext,
    /// Rung 2: `wlr-data-control-unstable-v1`, deprecated but universal.
    Wlr,
}

impl Rung {
    /// The manager global this rung binds.
    pub fn global(self) -> &'static str {
        match self {
            Rung::Ext => EXT_MANAGER,
            Rung::Wlr => WLR_MANAGER,
        }
    }
}

/// Which rung this session advertises, highest first.
///
/// Pure, and the only place the ladder's order lives: `bind` walks it
/// and the settings window asks it the same question without opening a
/// selection.
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

/// The line a session with no rung earns, naming both globals so a
/// compositor upgrade self-heals the install (ADR-0002's rule).
pub fn unavailable_line() -> String {
    format!(
        "clipboard: unavailable - this compositor advertises neither {EXT_MANAGER} nor \
         {WLR_MANAGER}, so chibipop has no clipboard protocol it can use here; every other \
         channel keeps running"
    )
}

/// Where the clipboard thread's diagnostics go.
///
/// The pump owns the log (ADR-0006) and this thread is not the pump, so
/// a line travels as a line, exactly as an AnkiConnect failure does.
#[derive(Clone)]
struct Notes(calloop::channel::Sender<String>);

impl Notes {
    fn note(&self, line: String) {
        // A pump that has gone away is not this thread's error to
        // report: it is about to be torn down with the process.
        let _ = self.0.send(line);
    }
}

/// The daemon's writable selection: bytes in, an offer held open.
pub struct Clipboard {
    rung: Rung,
    /// The offer thread's inbox. Its receiver lives in that thread's
    /// loop, so a send is also the wake.
    text: calloop::channel::Sender<Arc<[u8]>>,
}

impl Clipboard {
    /// Open a data-control connection and hand it a thread.
    ///
    /// `Ok(None)` is the honest absence: this session advertises no
    /// data-control global, which is a state and not a failure — the
    /// same distinction `Popup::bind` draws for a missing layer shell.
    /// `Err` is a real failure to set one up (no display, a global that
    /// vanished between the probe and the bind, no thread).
    ///
    /// `notes` is where the thread's diagnostics land, on the pump.
    pub fn bind(
        globals: &[Advertised],
        notes: calloop::channel::Sender<String>,
    ) -> Result<Option<Clipboard>> {
        let Some(rung) = rung(globals) else { return Ok(None) };
        let notes = Notes(notes);

        // Its own connection, for the reason the module doc gives.
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
        // Version 1 deliberately: `set_selection` is all this daemon
        // wants, and version 2 of the wlr rung adds the *primary*
        // selection - an announcement stream we would only be
        // destroying offers out of.
        let manager = Manager::bind(rung, &registry, manager_global.name, &qh);

        // The selection is per-seat, and this daemon takes the first
        // seat the session advertises - the same seat a pick's pointer
        // and keyboard come off (`App::seat`). A multi-seat session is
        // outside every channel's model here.
        let seat_global = globals
            .iter()
            .find(|g| g.interface == "wl_seat")
            .context("this session advertises no wl_seat to own a selection on")?;
        let seat = registry.bind::<WlSeat, _, Owner>(seat_global.name, 1, &qh, ());
        let device = manager.device(&seat, &qh);

        let mut owner = Owner { manager, device, source: None, notes, finished: false };
        // One round trip before the thread exists, so a refused bind is
        // an `Err` here rather than a silent thread. It also delivers
        // the device's opening `selection` event, whose offer the
        // handler destroys - the read-nothing posture, exercised at
        // startup.
        queue
            .roundtrip(&mut owner)
            .with_context(|| format!("binding {} on its own connection", rung.global()))?;

        let (text, inbox) = calloop::channel::channel::<Arc<[u8]>>();
        std::thread::Builder::new()
            .name("chibipop-clipboard".to_string())
            .spawn(move || serve(conn, queue, owner, inbox))
            .context("spawning the clipboard thread")?;

        Ok(Some(Clipboard { rung, text }))
    }

    /// Which rung serves this session, for a diagnostic to name.
    pub fn rung(&self) -> Rung {
        self.rung
    }

    /// Take the selection with `text`.
    ///
    /// Returns as soon as the bytes are queued: the offer itself is
    /// serviced on the clipboard thread for as long as the daemon owns
    /// it, so this never blocks the pump and the selection does not die
    /// when the call returns. An `Err` means the thread is gone — the
    /// compositor retired our device, or the process is coming down —
    /// which the caller reports rather than retries.
    pub fn set(&self, text: &str) -> Result<()> {
        let payload: Arc<[u8]> = Arc::from(text.as_bytes());
        self.text.send(payload).map_err(|_| {
            anyhow::anyhow!("the clipboard thread has ended; the selection was not taken")
        })
    }
}

/// The clipboard thread's whole world: the manager, the device, and the
/// source that currently owns the selection.
struct Owner {
    manager: Manager,
    device: Device,
    /// `None` before the first copy and after the compositor cancelled
    /// ours: another client owns the selection, which is normal.
    source: Option<Source>,
    notes: Notes,
    /// The compositor retired the device (`finished`). Nothing can be
    /// taken any more, so the loop stops and the next `set` fails.
    finished: bool,
}

impl Owner {
    /// Offer `payload` and take the selection with it.
    fn take(&mut self, payload: Arc<[u8]>, qh: &QueueHandle<Owner>) {
        // The payload rides as the source's own user data rather than in
        // a field here: a source may not be reused after
        // `set_selection` (it is a protocol error), so every copy makes
        // a new one, and each answers with the bytes it was created for
        // even if a `send` for a replaced source is still in flight.
        let source = self.manager.source(payload, qh);
        for mime in TEXT_MIMES {
            source.offer(mime);
        }
        self.device.set_selection(&source);
        // After `set_selection`, never before: the requests are ordered
        // on the wire, so the compositor has already moved the selection
        // onto the new source by the time it reads this destroy.
        if let Some(old) = self.source.replace(source) {
            old.destroy();
        }
    }

    /// One `send`: the compositor is relaying a paste.
    ///
    /// Answered on a thread of its own, and not because the write is
    /// slow — a reader that opens the pipe and never drains it would
    /// otherwise hold the clipboard thread and with it every later copy.
    /// One throwaway thread per paste is the `spawn_anki` bargain at a
    /// far lower rate: pastes are hand-driven.
    fn answer(&self, payload: &Arc<[u8]>, mime: &str, fd: OwnedFd) {
        let payload = Arc::clone(payload);
        let spawned = std::thread::Builder::new()
            .name("chibipop-clip-send".to_string())
            .spawn(move || {
                let mut pipe = std::fs::File::from(fd);
                // A short write is the reader's problem to notice: it
                // sees a truncated paste, which is the honest outcome of
                // a pipe it closed early. Dropping the file closes the
                // write end, which is its EOF.
                let _ = pipe.write_all(&payload);
            });
        if let Err(e) = spawned {
            self.notes.note(format!("clipboard: no thread to answer a {mime} paste - {e}"));
        }
    }

    /// The compositor cancelled a source of ours: someone else owns the
    /// selection now, which is a state and not a failure.
    fn cancelled(&mut self, source: &Source) {
        if self.source.as_ref().is_some_and(|s| s.is(source)) {
            if let Some(ours) = self.source.take() {
                ours.destroy();
            }
        }
    }

    /// The compositor retired the device. Nothing can be taken any more,
    /// so say so once and let the loop end; a later `set` then fails
    /// with the honest reason rather than queueing into nothing.
    fn retired(&mut self) {
        self.notes.note(
            "clipboard: the compositor retired this data-control device; copies will be \
             refused until the daemon restarts"
                .to_string(),
        );
        self.finished = true;
    }
}

/// The manager, per rung. Three enums rather than a trait object: the
/// two protocols are one protocol twice, `bind` picks a rung once, and a
/// `Dispatch` impl is written against a concrete interface anyway.
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
            // Unreachable by construction: one rung binds both. Nothing
            // to do rather than a panic on the daemon's clipboard.
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

    /// Same protocol object?
    fn is(&self, other: &Source) -> bool {
        match (self, other) {
            (Source::Ext(a), Source::Ext(b)) => a.id() == b.id(),
            (Source::Wlr(a), Source::Wlr(b)) => a.id() == b.id(),
            _ => false,
        }
    }
}

/// The clipboard thread: dispatch this connection until the compositor
/// retires the device or the daemon drops its end.
///
/// A calloop loop rather than a bare `blocking_dispatch`, for one
/// reason: the thread has to wait on the compositor *and* on the pump's
/// bytes at the same time, and this crate already owns that shape
/// (`capture/mod.rs`).
fn serve(
    conn: Connection,
    queue: EventQueue<Owner>,
    mut owner: Owner,
    inbox: calloop::channel::Channel<Arc<[u8]>>,
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
        calloop::channel::Event::Msg(payload) => owner.take(payload, &qh),
        // The daemon dropped its sender: the process is coming down.
        calloop::channel::Event::Closed => owner.finished = true,
    });
    if let Err(e) = inserted {
        owner.notes.note(format!("clipboard: registering the offer inbox failed - {e}"));
        return;
    }

    let signal = events.get_signal();
    let mut events = events;
    // No timeout: this thread is asleep until the compositor or the pump
    // says something (ADR-0010's idle budget).
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

/// The registry: this connection binds by name out of the daemon's probe
/// and never watches for changes, so there is nothing to hear.
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

/// The seat: bound at version 1 for `get_data_device` alone, so its
/// capabilities and name are not this client's business.
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
    // Opcode 0 is `data_offer`, the only event that creates a child.
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
            // Announced, never read: see the module doc.
            ext_device::Event::DataOffer { id } => id.destroy(),
            // The offer object still has to be destroyed even though we
            // never `receive` from it; `None` is the compositor telling
            // us the selection was cleared, and carries no object.
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

/// An announced offer, destroyed on arrival — so this never fires.
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

/// The source we own the selection with. Its user data *is* the payload,
/// so a `send` answers with the bytes that source was created for.
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

    /// The staged protocol wins where both exist, which is every current
    /// wlroots compositor: the wlr XML declares itself deprecated, so
    /// the ladder must not pin new sessions to it.
    #[test]
    fn the_staged_protocol_outranks_the_deprecated_wlr_one_where_both_are_advertised() {
        assert_eq!(
            Some(Rung::Ext),
            rung(&advertised(&["wl_seat", WLR_MANAGER, EXT_MANAGER]))
        );
    }

    /// And a session that only has the wlr rung still copies: today that
    /// is most of them.
    #[test]
    fn the_wlr_rung_serves_a_session_that_advertises_only_it() {
        assert_eq!(Some(Rung::Wlr), rung(&advertised(&["wl_seat", WLR_MANAGER])));
        assert_eq!(WLR_MANAGER, Rung::Wlr.global());
        assert_eq!(EXT_MANAGER, Rung::Ext.global());
    }

    /// Stock GNOME: no rung, which is a state - `bind` answers
    /// `Ok(None)` and opens no connection at all.
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

    /// The refusal names both globals, because that is what lets a
    /// compositor upgrade self-heal the install (ADR-0002).
    #[test]
    fn the_unavailable_line_names_both_globals_it_looked_for() {
        let line = unavailable_line();
        assert!(line.contains(EXT_MANAGER), "{line}");
        assert!(line.contains(WLR_MANAGER), "{line}");
    }

    /// A reader asks for `text/plain;charset=utf-8` first, so it must be
    /// offered first; the legacy X11 targets ride behind it.
    #[test]
    fn the_offered_mime_types_lead_with_utf8_text_and_carry_the_x11_targets() {
        assert_eq!("text/plain;charset=utf-8", TEXT_MIMES[0]);
        for target in ["TEXT", "STRING", "UTF8_STRING"] {
            assert!(TEXT_MIMES.contains(&target), "{target} is what XWayland asks by");
        }
    }
}
