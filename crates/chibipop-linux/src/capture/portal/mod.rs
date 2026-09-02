//! The fallback capture rung (ARCHITECTURE.md#capture-and-masking):
//! xdg-desktop-portal ScreenCast frames arriving over PipeWire,
//! cropped into core's `RegionCapture`.
//!
//! This is the only path on GNOME and KDE, and the shape of the rung
//! is visible in the files here:
//!
//! - [`dbus`] runs the consent handshake, **eagerly at startup**: one
//!   dialog covering every monitor (`SelectSources` with
//!   `multiple=true`), `persist_mode=2`, and a restore token so the
//!   second launch is silent. A denial is a named channel state with a
//!   retry, never an exit.
//! - [`token`] persists that token, rotating it every time the portal
//!   issues a fresh one.
//! - [`pipewire`] consumes the stream on PipeWire's own thread; the
//!   library is dlopened, so nothing here needs a build-time PipeWire.
//! - [`frame`] parks the newest frame and crops out of it without
//!   copying the monitor.
//! - [`metadata`] turns `cursor_mode=METADATA` samples into core
//!   cursor positions - the cursor ladder's rung 2, riding this same
//!   stream so cursor tracking costs no second consent.
//! - [`pod`] serialises the SPA format handshake by hand, because
//!   PipeWire's builders are `static inline` and unreachable through
//!   dlopen.
//!
//! **Only one stream is connected.** The dialog asks for every
//! monitor, because asking again per monitor is a second dialog; but
//! the streams for monitors the user is not on stay unconnected, so a
//! four-monitor desk pays for one. Crossing to another monitor swaps
//! the connected node ([`PortalCapture::grab`] does it as part of
//! answering), which costs a frame and no dialog.
//!
//! **Shape.** Pull-shaped like the trait demands and like the
//! screencopy rung: `grab` crops whatever the stream has most recently
//! parked and never waits for damage. The one wait in the file is for
//! the *first* frame after a connect, which is the stream starting
//! rather than the screen changing, and it has a deadline.

pub mod dbus;
pub mod frame;
pub mod metadata;
pub mod pipewire;
pub mod pod;
pub mod token;

use crate::capture::geometry;
use crate::cursor::outputs::OutputGeometry;
use chibipop::geom::{PhysPoint, PhysRect};
use chibipop::text::{Frame, RegionCapture};
use metadata::StreamPlacement;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// What `Frame::source` says: the rung, for logs and `probe`.
pub const SOURCE: &str = "portal-screencast";

/// How long the whole consent handshake may take.
///
/// Generous on purpose: this is a human reading a dialog, and the
/// budget is only there so a portal that never answers cannot hang
/// startup forever.
pub const CONSENT_TIMEOUT: Duration = Duration::from_secs(120);

/// How long a `grab` may wait for the first frame of a stream it just
/// connected.
///
/// Not a wait for damage - the stream has to hand over one frame
/// before there is anything to crop, and a monitor swap re-opens that
/// window. Missing the deadline fails one grab, never the daemon.
pub const FIRST_FRAME_DEADLINE: Duration = Duration::from_millis(500);

/// Is the portal's ScreenCast interface on the session bus?
pub fn available() -> bool {
    dbus::probe()
}

/// Does the portal advertise `cursor_mode=METADATA`, i.e. can the
/// cursor ladder's rung 2 exist on this session?
pub fn cursor_metadata_available() -> bool {
    dbus::available_cursor_modes().is_some_and(|m| m & dbus::CURSOR_MODE_METADATA != 0)
}

/// Where a cursor sample goes once it is in core's coordinates. The
/// daemon backs this with a calloop channel; nothing here knows that.
pub type CursorSink = Arc<dyn Fn(PhysPoint) + Send + Sync>;

/// Why the portal rung is not serving. Every variant has a `detail`
/// short enough for a tray row and specific enough to act on.
#[derive(Debug)]
pub enum OpenError {
    /// The handshake itself failed - denied, absent, timed out.
    Portal(dbus::PortalError),
    /// The portal said yes but PipeWire could not deliver.
    PipeWire(String),
    /// Consent was granted for nothing this backend can read.
    NoMonitors,
}

impl OpenError {
    /// The channel-status row: what is wrong, and the way back.
    pub fn detail(&self) -> String {
        match self {
            OpenError::Portal(e) => e.detail(),
            OpenError::PipeWire(e) => {
                format!("screen capture stream failed: {e} - retry with `chibipop ctl reload`")
            }
            OpenError::NoMonitors => {
                "screen capture approved no monitor - retry with `chibipop ctl reload` and \
                 pick a display"
                    .to_string()
            }
        }
    }
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail())
    }
}

impl std::error::Error for OpenError {}

/// One approved monitor: the node to connect for it, and where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Monitor {
    pub node_id: u32,
    /// Its top-left in core's global physical space, when we could
    /// anchor it against a known output.
    pub anchor: Option<PhysPoint>,
    /// Its size in stream buffer pixels, as far as the portal and the
    /// output geometry agree.
    pub size: (i32, i32),
}

impl Monitor {
    /// This monitor's box in the global physical space.
    pub fn bounds(&self) -> Option<PhysRect> {
        let a = self.anchor?;
        (self.size.0 > 0 && self.size.1 > 0)
            .then_some(PhysRect { x: a.x, y: a.y, w: self.size.0, h: self.size.1 })
    }
}

/// Place `region` on a stream anchored at `anchor` and `w x h` big.
///
/// Answers the source box inside the stream buffer and where it lands
/// in the caller's `region`-sized frame, or `None` when the region and
/// the stream do not overlap at all. Clipping here rather than in the
/// blit is what lets a box that hangs off the monitor still yield the
/// pixels that exist, with the rest of the frame left black - the same
/// bargain the screencopy rung strikes (`capture::geometry`).
pub fn cut(region: PhysRect, anchor: PhysPoint, w: i32, h: i32) -> Option<(PhysRect, PhysPoint)> {
    if region.w <= 0 || region.h <= 0 || w <= 0 || h <= 0 {
        return None;
    }
    let stream = PhysRect { x: anchor.x, y: anchor.y, w, h };
    let overlap = geometry::intersect(region, stream)?;
    let src = PhysRect {
        x: overlap.x - anchor.x,
        y: overlap.y - anchor.y,
        w: overlap.w,
        h: overlap.h,
    };
    let dest = PhysPoint { x: overlap.x - region.x, y: overlap.y - region.y };
    Some((src, dest))
}

/// The granted session: the consent, the monitors it covers, the one
/// PipeWire connection, and the one stream currently on it.
pub struct PortalSession {
    /// Held for its lifetime: dropping it closes the portal session
    /// and revokes the grant.
    session: dbus::Session,
    monitors: Vec<Monitor>,
    /// Outlives every stream, because it owns the descriptor
    /// `OpenPipeWireRemote` handed over (see [`pipewire::Remote`]).
    remote: Arc<pipewire::Remote>,
    stream: pipewire::Stream,
    /// Rebuilt on every monitor swap, because the anchor changes.
    cursor: Option<CursorSink>,
    /// Whatever output geometry the daemon had at startup; used to
    /// bound tiling when a region is on no approved monitor.
    outputs: Vec<OutputGeometry>,
}

impl PortalSession {
    /// The eager startup consent, end to end.
    ///
    /// `state_dir` holds the rotating restore token that makes the
    /// second launch silent. `at` is where the cursor is now, which
    /// decides the one monitor whose stream gets connected. A denial
    /// comes back as [`OpenError`], never a panic and never an exit.
    pub fn open(
        state_dir: &Path,
        outputs: &[OutputGeometry],
        at: PhysPoint,
        cursor: Option<CursorSink>,
        mut diagnostic: impl FnMut(String),
    ) -> Result<PortalSession, OpenError> {
        let store = token::TokenStore::new(state_dir);
        let want_cursor = cursor.is_some();
        let stored = store.load();
        diagnostic(format!(
            "portal: requesting screen capture consent ({}; token {})",
            if want_cursor { "cursor_mode=METADATA" } else { "cursor hidden" },
            if stored.is_some() { "restored" } else { "none - a dialog will appear" }
        ));

        let consent = match dbus::open(stored.as_deref(), want_cursor, CONSENT_TIMEOUT) {
            Ok(consent) => consent,
            Err(e) if stored.is_some() && e.retry_needs_fresh_consent() => {
                // A token the portal no longer honours is indis-
                // tinguishable from a denial in the reply, so the
                // stored one is dropped and the user gets the dialog
                // they would have got on a first launch. Exactly one
                // retry: two dialogs is already one too many.
                diagnostic(format!("portal: stored restore token rejected ({e}); prompting once"));
                let _ = store.clear();
                dbus::open(None, want_cursor, CONSENT_TIMEOUT).map_err(OpenError::Portal)?
            }
            Err(e) => return Err(OpenError::Portal(e)),
        };
        let dbus::Consent { streams, restore_token, version, persists, pipewire_fd, session } =
            consent;

        match restore_token {
            Some(fresh) => match store.rotate(&fresh) {
                Ok(()) => diagnostic(format!(
                    "portal: restore token rotated into {} - the next launch is silent",
                    store.path().display()
                )),
                // A token we cannot persist costs one dialog next
                // launch. It is not worth failing a granted session.
                Err(e) => diagnostic(format!("portal: could not persist the restore token: {e:#}")),
            },
            // Two different facts, and conflating them would send
            // somebody hunting a bug that is not there.
            None if !persists => diagnostic(format!(
                "portal: ScreenCast v{version} cannot remember a grant (persist_mode needs v{}), \
                 so every launch shows the dialog on this portal",
                dbus::PERSIST_MIN_VERSION
            )),
            None => diagnostic(
                "portal: no restore token issued; every launch will show the dialog".to_string(),
            ),
        }

        let monitors = monitors_of(&streams, outputs);
        if monitors.is_empty() {
            return Err(OpenError::NoMonitors);
        }
        diagnostic(format!(
            "portal: consent granted for {} monitor(s); connecting the one under the cursor",
            monitors.len()
        ));

        let lib = pipewire::Lib::open().map_err(|e| OpenError::PipeWire(format!("{e:#}")))?;
        let remote = pipewire::Remote::connect(lib, pipewire_fd)
            .map_err(|e| OpenError::PipeWire(format!("{e:#}")))?;
        let chosen = pick(&monitors, at);
        let stream = connect(&remote, monitors[chosen], cursor.as_ref())
            .map_err(|e| OpenError::PipeWire(format!("{e:#}")))?;
        diagnostic(format!(
            "portal: streaming node {} (1 of {} monitors connected; the rest stay unconnected)",
            stream.node_id(),
            monitors.len()
        ));

        Ok(PortalSession {
            session,
            monitors,
            remote,
            stream,
            cursor,
            outputs: outputs.to_vec(),
        })
    }

    /// The monitors the user approved.
    pub fn monitors(&self) -> &[Monitor] {
        &self.monitors
    }

    /// What the connected stream is doing, for the status registry.
    pub fn health(&self) -> pipewire::Health {
        self.stream.health()
    }

    /// The node currently connected.
    pub fn node_id(&self) -> u32 {
        self.stream.node_id()
    }

    /// The portal session object path, for the startup log: it is what
    /// `busctl` and the portal's own logs name a grant by.
    pub fn session_path(&self) -> &str {
        self.session.path()
    }

    /// Connect the monitor `region` lives on, if it is not the one
    /// already connected.
    ///
    /// Answers whether a swap happened, so the caller knows to wait
    /// for a first frame.
    fn ensure_monitor_for(&mut self, region: PhysRect) -> anyhow::Result<bool> {
        let centre =
            PhysPoint { x: region.x + region.w / 2, y: region.y + region.h / 2 };
        let wanted = self.monitors[pick(&self.monitors, centre)];
        if wanted.node_id == self.stream.node_id() {
            return Ok(false);
        }
        // The old stream goes first: two connected streams is the
        // multi-monitor cost this rung declines to pay. The connection
        // underneath survives, so the swap costs a frame, not a
        // handshake.
        let replacement = connect(&self.remote, wanted, self.cursor.as_ref())?;
        self.stream = replacement;
        Ok(true)
    }

    /// The anchor the connected stream's pixels are measured from.
    fn anchor(&self) -> Option<PhysPoint> {
        self.monitors.iter().find(|m| m.node_id == self.stream.node_id())?.anchor
    }
}

/// Which approved monitor `p` is on, else the first: an answer always
/// beats an error here, exactly as `RegionCapture::bounds_containing`
/// argues.
fn pick(monitors: &[Monitor], p: PhysPoint) -> usize {
    monitors
        .iter()
        .position(|m| m.bounds().is_some_and(|b| geometry::intersect(b, PhysRect { x: p.x, y: p.y, w: 1, h: 1 }).is_some()))
        .unwrap_or(0)
}

/// Turn the portal's stream list into monitors anchored in core's
/// global physical space.
fn monitors_of(streams: &[dbus::StreamInfo], outputs: &[OutputGeometry]) -> Vec<Monitor> {
    streams
        .iter()
        .map(|s| {
            let placement = StreamPlacement {
                logical_origin: s.position,
                logical_size: s.size,
                // The negotiated video size is not known until the
                // format fixates; the matched output's physical size is
                // the same number on every compositor that scales its
                // capture to the mode, and it only bounds the cursor
                // sample.
                buffer_size: (0, 0),
            };
            let size = match metadata::match_output(&placement, outputs) {
                Some(i) => (outputs[i].physical_w(), physical_h(&outputs[i])),
                // Unmatched: take the portal's own logical size, which
                // is right at scale 1 and least-wrong elsewhere.
                None => s.size.unwrap_or((0, 0)),
            };
            Monitor { node_id: s.node_id, anchor: metadata::anchor(&placement, outputs), size }
        })
        .collect()
}

/// An output's physical height, mirroring `capture::geometry`'s own
/// transform handling without exposing it.
fn physical_h(g: &OutputGeometry) -> i32 {
    if g.transform_swaps {
        g.mode_w
    } else {
        g.mode_h
    }
}

/// Open one PipeWire stream for `monitor`, wiring the cursor sink to
/// that monitor's anchor.
fn connect(
    remote: &Arc<pipewire::Remote>,
    monitor: Monitor,
    cursor: Option<&CursorSink>,
) -> anyhow::Result<pipewire::Stream> {
    let sink: Option<pipewire::CursorSink> = match (cursor, monitor.anchor) {
        (Some(sink), Some(anchor)) => {
            let sink = Arc::clone(sink);
            let size = monitor.size;
            Some(Box::new(move |x: i32, y: i32| {
                if let Some(p) = metadata::cursor_to_global(anchor, size, x, y) {
                    sink(p);
                }
            }))
        }
        // Without an anchor a sample cannot be placed, and a cursor
        // placed wrong is worse than no cursor rung at all.
        _ => None,
    };
    pipewire::Stream::connect(remote, monitor.node_id, sink)
}

/// Pixels served for one region, so a dwell on unchanged content has
/// something honest to answer with.
struct Held {
    region: PhysRect,
    /// The content counter those pixels came from.
    seq: u64,
    buf: Vec<u8>,
}

/// The portal rung's `RegionCapture`.
///
/// It owns the session, because the session owns the stream and the
/// stream is what a grab reads. The daemon opens one at startup (the
/// eager consent) and the Worker thread reads through it.
pub struct PortalCapture {
    session: PortalSession,
    /// This read's pixels, and the previous read's: two generations,
    /// so one read's tiles cannot evict each other. Same arrangement
    /// as the screencopy rung.
    held: Vec<Held>,
    previous: Vec<Held>,
}

impl PortalCapture {
    pub fn new(session: PortalSession) -> PortalCapture {
        PortalCapture { session, held: Vec::new(), previous: Vec::new() }
    }

    /// Pixels held for `region`, promoted into this read.
    fn take_held(&mut self, region: PhysRect) -> Option<&Held> {
        if let Some(i) = self.held.iter().position(|h| h.region == region) {
            return Some(&self.held[i]);
        }
        let i = self.previous.iter().position(|h| h.region == region)?;
        let entry = self.previous.remove(i);
        self.held.push(entry);
        self.held.last()
    }

    fn hold(&mut self, region: PhysRect, seq: u64, buf: &[u8]) {
        match self.held.iter_mut().find(|h| h.region == region) {
            Some(slot) => {
                slot.seq = seq;
                slot.buf.clear();
                slot.buf.extend_from_slice(buf);
            }
            None => self.held.push(Held { region, seq, buf: buf.to_vec() }),
        }
    }
}

impl RegionCapture for PortalCapture {
    fn grab(&mut self, region: PhysRect) -> anyhow::Result<Frame> {
        anyhow::ensure!(
            region.w > 0 && region.h > 0,
            "a {}x{} region has no pixels",
            region.w,
            region.h
        );
        let swapped = self.session.ensure_monitor_for(region)?;
        let store = Arc::clone(self.session.stream.store());
        if swapped || store.shape().is_none() {
            anyhow::ensure!(
                store.wait_for_first(FIRST_FRAME_DEADLINE),
                "the portal stream delivered no frame within {FIRST_FRAME_DEADLINE:?}"
            );
        }

        let seq = store.content_seq();
        if let Some(held) = self.take_held(region) {
            if held.seq == seq {
                // Nothing on this monitor has damaged since these
                // pixels were served, so they are still the screen's -
                // and the pipeline can skip an OCR pass it has paid
                // for (core's `Frame::unchanged`).
                return Ok(Frame {
                    buf: held.buf.clone(),
                    w: region.w,
                    h: region.h,
                    source: SOURCE,
                    fallback: None,
                    unchanged: true,
                });
            }
        }

        let anchor = self
            .session
            .anchor()
            .context_or("the connected portal stream is anchored to no known output")?;
        let shape = store
            .shape()
            .context_or("the portal stream has not negotiated a format yet")?;
        let len = usize::try_from(i64::from(region.w) * i64::from(region.h) * 4)
            .map_err(|_| anyhow::anyhow!("a {}x{} region is too large", region.w, region.h))?;
        // Black where the stream does not reach: off-monitor has no
        // pixels, and blank costs one hover nothing where failing
        // costs every hover near an edge.
        let mut buf = vec![0u8; len];
        let served = match cut(region, anchor, shape.w, shape.h) {
            Some((src, dest)) => store.crop_into(src, dest, &mut buf, region.w, region.h),
            None => Some(seq),
        };
        let seq = served
            .context_or("the portal stream has no frame parked")?;
        self.hold(region, seq, &buf);
        Ok(Frame { buf, w: region.w, h: region.h, source: SOURCE, fallback: None, unchanged: false })
    }

    fn bounds_containing(&self, p: PhysPoint) -> PhysRect {
        if let Some(bounds) =
            self.session.monitors.iter().filter_map(Monitor::bounds).find(|b| {
                p.x >= b.x && p.y >= b.y && p.x < b.x + b.w && p.y < b.y + b.h
            })
        {
            return bounds;
        }
        // No approved monitor holds it: fall back to the same output
        // arithmetic the screencopy rung uses, which always answers.
        geometry::bounds_containing(&self.session.outputs, p)
    }

    fn begin_read(&mut self) {
        self.previous = std::mem::take(&mut self.held);
    }
}

/// `Option::ok_or_else` with an `anyhow` message, spelled once.
trait ContextOr<T> {
    fn context_or(self, message: &'static str) -> anyhow::Result<T>;
}

impl<T> ContextOr<T> for Option<T> {
    fn context_or(self, message: &'static str) -> anyhow::Result<T> {
        self.ok_or_else(|| anyhow::anyhow!(message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAM: (i32, i32) = (2560, 1440);

    fn at(x: i32, y: i32) -> PhysPoint {
        PhysPoint { x, y }
    }

    /// A box wholly inside the stream is a straight translation into
    /// stream coordinates, landing at the frame's origin.
    #[test]
    fn a_region_inside_the_stream_translates_by_the_anchor() {
        let region = PhysRect { x: 2660, y: 300, w: 400, h: 200 };
        let (src, dest) = cut(region, at(2560, 0), STREAM.0, STREAM.1).expect("an overlap");
        assert_eq!(PhysRect { x: 100, y: 300, w: 400, h: 200 }, src);
        assert_eq!(at(0, 0), dest);
    }

    /// A box hanging off the left edge yields only the pixels that
    /// exist, placed where they belong in the frame; the caller's
    /// black stays put over the rest.
    #[test]
    fn a_region_overhanging_the_stream_is_clipped_and_offset() {
        let region = PhysRect { x: -50, y: -20, w: 200, h: 100 };
        let (src, dest) = cut(region, at(0, 0), STREAM.0, STREAM.1).expect("an overlap");
        assert_eq!(PhysRect { x: 0, y: 0, w: 150, h: 80 }, src);
        assert_eq!(at(50, 20), dest);
    }

    #[test]
    fn a_region_past_the_far_edge_keeps_only_what_the_stream_holds() {
        let region = PhysRect { x: 2500, y: 1400, w: 200, h: 100 };
        let (src, dest) = cut(region, at(0, 0), STREAM.0, STREAM.1).expect("an overlap");
        assert_eq!(PhysRect { x: 2500, y: 1400, w: 60, h: 40 }, src);
        assert_eq!(at(0, 0), dest);
    }

    /// No overlap is not a clip of size zero: there is nothing to
    /// blit, and the caller must know.
    #[test]
    fn a_region_on_another_monitor_does_not_cut() {
        assert_eq!(None, cut(PhysRect { x: 3000, y: 0, w: 100, h: 100 }, at(0, 0), 2560, 1440));
        assert_eq!(None, cut(PhysRect { x: 0, y: 0, w: 0, h: 100 }, at(0, 0), 2560, 1440));
        assert_eq!(None, cut(PhysRect { x: 0, y: 0, w: 10, h: 10 }, at(0, 0), 0, 1440));
    }

    /// Picking a monitor is what keeps exactly one stream connected.
    #[test]
    fn the_monitor_under_the_point_is_the_one_connected() {
        let left = Monitor { node_id: 11, anchor: Some(at(0, 0)), size: (2560, 1440) };
        let right = Monitor { node_id: 22, anchor: Some(at(2560, 0)), size: (1920, 1080) };
        let monitors = [left, right];
        assert_eq!(0, pick(&monitors, at(10, 10)));
        assert_eq!(1, pick(&monitors, at(3000, 500)));
        // Off every monitor: an answer beats an error.
        assert_eq!(0, pick(&monitors, at(-500, -500)));
    }

    /// A monitor we could not anchor has no box, and must never be
    /// treated as covering the origin.
    #[test]
    fn an_unanchored_monitor_has_no_bounds() {
        let unplaced = Monitor { node_id: 5, anchor: None, size: (1920, 1080) };
        assert_eq!(None, unplaced.bounds());
        let zero = Monitor { node_id: 6, anchor: Some(at(0, 0)), size: (0, 0) };
        assert_eq!(None, zero.bounds());
    }

    /// The tray row must name the way back, whatever went wrong.
    #[test]
    fn every_failure_names_something_to_do_about_it() {
        for error in [
            OpenError::Portal(dbus::PortalError::Denied),
            OpenError::PipeWire("no libpipewire-0.3.so.0".to_string()),
            OpenError::NoMonitors,
        ] {
            let detail = error.detail();
            assert!(!detail.is_empty(), "{error:?}");
            assert!(
                detail.contains("retry") || detail.contains("settings"),
                "{detail} names no way back"
            );
        }
    }

    /// The rule `PortalSession::open` acts on: a refusal or a token the
    /// portal no longer honours must drop the stored token, so the next
    /// attempt shows a dialog instead of silently failing forever.
    #[test]
    fn a_refusal_drops_the_stored_token_and_a_stream_failure_does_not() {
        assert!(dbus::PortalError::Denied.retry_needs_fresh_consent());
        assert!(!dbus::PortalError::TimedOut("Start".to_string()).retry_needs_fresh_consent());
    }
}
