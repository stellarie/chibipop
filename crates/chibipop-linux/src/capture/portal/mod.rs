//! This module implements the fallback capture rung (ARCHITECTURE.md#capture-and-masking).
//! xdg-desktop-portal ScreenCast frames arrive over PipeWire and become
//! cropped `RegionCapture` frames.
//!
//! This rung is the only capture path on GNOME and KDE. These modules provide:
//!
//! - [`dbus`] runs the consent handshake **at startup**. One dialog covers
//!   every monitor (`SelectSources` with `multiple=true`), `persist_mode=2`,
//!   and a restore token. The second launch has no dialog. A denial becomes
//!   a named channel state with a retry, and the daemon does not exit.
//! - [`token`] stores the token and rotates it after each fresh portal token.
//! - [`pipewire`] reads the stream on PipeWire's thread. The code loads the
//!   library at runtime, so this module needs no build-time PipeWire.
//! - [`frame`] stores the newest frame and crops regions from it. It never
//!   copies the monitor.
//! - [`metadata`] converts `cursor_mode=METADATA` samples to core cursor
//!   positions. This is cursor ladder rung 2. Cursor updates need no second
//!   consent because they use this stream.
//! - [`pod`] serializes the SPA format handshake by hand because PipeWire's
//!   builders are `static inline`. dlopen cannot access them.
//!
//! **The code connects only one stream.** The dialog asks for every monitor
//! because one question for each monitor would create one dialog per monitor.
//! The code leaves streams for other monitors unconnected, so a four-monitor
//! desk uses one stream. A move to another monitor changes the connected node.
//! [`PortalCapture::grab`] makes this change as part of its result. The change
//! needs one frame but no dialog.
//!
//! **Shape.** This rung has the pull shape that the trait and screencopy rung
//! require. `grab` crops the newest parked frame and never waits for damage.
//! The only wait is for the first frame after a connect. It waits for stream
//! start, not for a screen change, and it has a deadline.

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

/// Maximum duration for the complete consent handshake.
///
/// The duration gives a person time to read the dialog. A portal that never
/// answers must not block startup forever. This duration is the only guard.
pub const CONSENT_TIMEOUT: Duration = Duration::from_secs(120);

/// Maximum time that `grab` can wait for the first frame after it connects.
///
/// This is not a wait for damage. The stream must deliver one frame before
/// the code can crop pixels. A monitor change starts this wait again. A missed
/// deadline fails one grab, not the daemon.
pub const FIRST_FRAME_DEADLINE: Duration = Duration::from_millis(500);

/// Is the portal's ScreenCast interface on the session bus?
pub fn available() -> bool {
    dbus::probe()
}

/// Returns whether the portal advertises `cursor_mode=METADATA`. This indicates
/// whether cursor ladder rung 2 can exist on this session.
pub fn cursor_metadata_available() -> bool {
    dbus::available_cursor_modes().is_some_and(|m| m & dbus::CURSOR_MODE_METADATA != 0)
}

/// Receives a cursor sample after the code converts it to core coordinates.
/// The daemon connects this sink to a calloop channel. This module does not
/// access that channel.
pub type CursorSink = Arc<dyn Fn(PhysPoint) + Send + Sync>;

/// Describes why the portal rung cannot serve. Every variant has a `detail`.
/// The `detail` fits a tray row and names a repair.
#[derive(Debug)]
pub enum OpenError {
    /// The portal denied, is absent, or timed out during the handshake.
    Portal(dbus::PortalError),
    /// The portal approved the request, but PipeWire could not deliver a stream.
    PipeWire(String),
    /// The user approved no monitor that this backend can read.
    NoMonitors,
}

impl OpenError {
    /// Returns the channel-status row with the fault and a repair.
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

/// An approved monitor: the node to connect and its placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Monitor {
    pub node_id: u32,
    /// The top-left corner in core's global physical space when the code can
    /// anchor this monitor to a known output.
    pub anchor: Option<PhysPoint>,
    /// The size in stream buffer pixels. The portal and output geometry agree
    /// on this size.
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

/// Places `region` on a stream with top-left `anchor` and size `w x h`.
///
/// Returns the source box inside the stream buffer and the destination point
/// in the caller's frame. Returns `None` when the region and stream do not
/// overlap. The code clips here instead of in the blit. A box beyond the
/// monitor returns its available pixels, and the rest of the frame stays
/// black. The screencopy rung uses the same rule (`capture::geometry`).
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

/// The consent session, its approved monitors, its PipeWire connection, and
/// its one connected stream.
pub struct PortalSession {
    /// The code holds this handle for the session. `Drop` closes the portal
    /// session and revokes the grant.
    session: dbus::Session,
    monitors: Vec<Monitor>,
    /// This connection outlives every stream because it owns the descriptor that
    /// `OpenPipeWireRemote` returned. See [`pipewire::Remote`].
    remote: Arc<pipewire::Remote>,
    stream: pipewire::Stream,
    /// The code rebuilds this sink after each monitor change because the anchor
    /// changes.
    cursor: Option<CursorSink>,
    /// Output geometry from startup. The code uses it to bound tiling when a
    /// region lies on no approved monitor.
    outputs: Vec<OutputGeometry>,
}

impl PortalSession {
    /// Runs the startup consent handshake from start to finish.
    ///
    /// `state_dir` holds the restore token that changes after each fresh token.
    /// This removes the dialog on the second launch. `at` is the current cursor
    /// position. It selects the monitor whose stream the code connects. A denial
    /// returns [`OpenError`]. It never panics or exits the daemon.
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
                // The portal uses the same reply for an invalid token and a denial. The code
                // clears the stored token, so the next call shows a first-launch dialog. The
                // code retries once because two dialogs would delay startup twice.
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
                // If the code cannot store a token, the next launch shows one dialog. This
                // must not fail a session that already has consent.
                Err(e) => diagnostic(format!("portal: could not persist the restore token: {e:#}")),
            },
            // These cases have different causes. One message for both would make a
            // reader investigate a fault that does not exist.
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

    /// The state of the connected stream, for the status registry.
    pub fn health(&self) -> pipewire::Health {
        self.stream.health()
    }

    /// The node that is connected now.
    pub fn node_id(&self) -> u32 {
        self.stream.node_id()
    }

    /// The portal session object path for the startup log. `busctl` and the
    /// portal logs identify a grant with this path.
    pub fn session_path(&self) -> &str {
        self.session.path()
    }

    /// Connects the monitor that contains `region` when that monitor is not
    /// connected.
    ///
    /// Returns whether the code changed the stream. The caller must then wait for
    /// the first frame.
    fn ensure_monitor_for(&mut self, region: PhysRect) -> anyhow::Result<bool> {
        let centre =
            PhysPoint { x: region.x + region.w / 2, y: region.y + region.h / 2 };
        let wanted = self.monitors[pick(&self.monitors, centre)];
        if wanted.node_id == self.stream.node_id() {
            return Ok(false);
        }
        // The code replaces the old stream before it connects the new one. Two
        // connected streams would add the multi-monitor cost that this rung rejects.
        // The connection remains active, so the change needs one frame, not a
        // handshake.
        let replacement = connect(&self.remote, wanted, self.cursor.as_ref())?;
        self.stream = replacement;
        Ok(true)
    }

    /// The anchor for the pixels of the connected stream.
    fn anchor(&self) -> Option<PhysPoint> {
        self.monitors.iter().find(|m| m.node_id == self.stream.node_id())?.anchor
    }
}

/// Returns the approved monitor that contains `p`, or the first monitor. This
/// function returns an answer instead of an error, as
/// `RegionCapture::bounds_containing` requires.
fn pick(monitors: &[Monitor], p: PhysPoint) -> usize {
    monitors
        .iter()
        .position(|m| m.bounds().is_some_and(|b| geometry::intersect(b, PhysRect { x: p.x, y: p.y, w: 1, h: 1 }).is_some()))
        .unwrap_or(0)
}

/// Converts the portal stream list to monitors with anchors in core's global
/// physical space.
fn monitors_of(streams: &[dbus::StreamInfo], outputs: &[OutputGeometry]) -> Vec<Monitor> {
    streams
        .iter()
        .map(|s| {
            let placement = StreamPlacement {
                logical_origin: s.position,
                logical_size: s.size,
                // The code must fixate the format before it knows the negotiated video size.
                // A matched output's physical size gives the same value on compositors that
                // scale capture to the mode. This value only bounds cursor samples.
                buffer_size: (0, 0),
            };
            let size = match metadata::match_output(&placement, outputs) {
                Some(i) => (outputs[i].physical_w(), physical_h(&outputs[i])),
                // No output matched. Use the portal's logical size. It is correct at scale 1
                // and the closest available value at other scales.
                None => s.size.unwrap_or((0, 0)),
            };
            Monitor { node_id: s.node_id, anchor: metadata::anchor(&placement, outputs), size }
        })
        .collect()
}

/// Returns an output's physical height. This matches the transform rule in
/// `capture::geometry` without exposing that code.
fn physical_h(g: &OutputGeometry) -> i32 {
    if g.transform_swaps {
        g.mode_w
    } else {
        g.mode_h
    }
}

/// Opens one PipeWire stream for `monitor`. It binds the cursor sink to the
/// monitor's anchor.
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
        // Without an anchor, the code cannot place a sample. A wrong cursor position
        // is worse than no cursor rung.
        _ => None,
    };
    pipewire::Stream::connect(remote, monitor.node_id, sink)
}

/// Pixels that the code served for one region. A dwell on unchanged content
/// can then return the correct answer.
struct Held {
    region: PhysRect,
    /// The value of the content counter for those pixels.
    seq: u64,
    buf: Vec<u8>,
}

/// The portal rung's `RegionCapture`.
///
/// It owns the session, which owns the stream. A grab reads the stream. The
/// daemon opens one session at startup, so consent occurs early. The Worker
/// thread reads through this capture.
pub struct PortalCapture {
    session: PortalSession,
    /// Pixels from this read and the previous read. Two generations keep tiles
    /// from one read apart, so one read cannot evict the other. The screencopy rung
    /// uses the same arrangement.
    held: Vec<Held>,
    previous: Vec<Held>,
}

impl PortalCapture {
    pub fn new(session: PortalSession) -> PortalCapture {
        PortalCapture { session, held: Vec::new(), previous: Vec::new() }
    }

    /// Returns pixels that the code holds for `region` and promotes them into
    /// this read.
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
                // No damage reached this monitor after the code served these pixels. They
                // still match the screen, so the pipeline can skip one OCR pass
                // (core's `Frame::unchanged`).
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
        // The buffer stays black where the stream has no pixels. An off-monitor area
        // has no content. This blank area affects one hover, but a failure would
        // affect every hover near an edge.
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
        // No approved monitor contains the point. Use the same output arithmetic as
        // the screencopy rung, which always returns a bound.
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

    /// A region inside the stream needs only translation into stream coordinates.
    /// It reaches the frame origin.
    #[test]
    fn a_region_inside_the_stream_translates_by_the_anchor() {
        let region = PhysRect { x: 2660, y: 300, w: 400, h: 200 };
        let (src, dest) = cut(region, at(2560, 0), STREAM.0, STREAM.1).expect("an overlap");
        assert_eq!(PhysRect { x: 100, y: 300, w: 400, h: 200 }, src);
        assert_eq!(at(0, 0), dest);
    }

    /// A region can extend beyond the stream. The function returns only pixels
    /// that the stream has and places them at the correct frame positions. The
    /// caller's black buffer remains elsewhere.
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

    /// No overlap is not a zero-size clip. The function has no pixels to blit, so
    /// the caller must receive `None`.
    #[test]
    fn a_region_on_another_monitor_does_not_cut() {
        assert_eq!(None, cut(PhysRect { x: 3000, y: 0, w: 100, h: 100 }, at(0, 0), 2560, 1440));
        assert_eq!(None, cut(PhysRect { x: 0, y: 0, w: 0, h: 100 }, at(0, 0), 2560, 1440));
        assert_eq!(None, cut(PhysRect { x: 0, y: 0, w: 10, h: 10 }, at(0, 0), 0, 1440));
    }

    /// One monitor choice keeps exactly one stream connected.
    #[test]
    fn the_monitor_under_the_point_is_the_one_connected() {
        let left = Monitor { node_id: 11, anchor: Some(at(0, 0)), size: (2560, 1440) };
        let right = Monitor { node_id: 22, anchor: Some(at(2560, 0)), size: (1920, 1080) };
        let monitors = [left, right];
        assert_eq!(0, pick(&monitors, at(10, 10)));
        assert_eq!(1, pick(&monitors, at(3000, 500)));
        // The point is outside every monitor. The function returns the first monitor.
        assert_eq!(0, pick(&monitors, at(-500, -500)));
    }

    /// A monitor without an anchor has no bounds. The code must not treat it as a
    /// cover of the origin.
    #[test]
    fn an_unanchored_monitor_has_no_bounds() {
        let unplaced = Monitor { node_id: 5, anchor: None, size: (1920, 1080) };
        assert_eq!(None, unplaced.bounds());
        let zero = Monitor { node_id: 6, anchor: Some(at(0, 0)), size: (0, 0) };
        assert_eq!(None, zero.bounds());
    }

    /// The tray row must identify a repair for every error.
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

    /// `PortalSession::open` must clear the stored token after a refusal or a
    /// token rejection. The next attempt shows a dialog instead of failing
    /// without a message.
    #[test]
    fn a_refusal_drops_the_stored_token_and_a_stream_failure_does_not() {
        assert!(dbus::PortalError::Denied.retry_needs_fresh_consent());
        assert!(!dbus::PortalError::TimedOut("Start".to_string()).retry_needs_fresh_consent());
    }
}
