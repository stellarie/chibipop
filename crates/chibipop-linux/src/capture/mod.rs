//! The wlr-screencopy capture backend is the primary Linux
//! `RegionCapture` described in `ARCHITECTURE.md#capture-and-masking`.
//!
//! This is the only protocol here that captures a region through the
//! compositor. It needs no prompt, so a hover works as soon as the daemon
//! starts on Hyprland, sway, or niri. Selection uses the advertised global
//! from `available`, never the compositor identity. If a capability is
//! absent, the ladder skips that rung.
//!
//! **Shape.** This backend follows the pull shape of `RegionCapture`.
//! `grab` returns the newest content for the box and never waits for
//! damage. The backend uses a plain `copy` for a changed region. A repeated region
//! is a dwell. [`pacing`] runs the damage race for a dwell. Its timeout
//! means that nothing changed, not a failure. The backend reports this
//! as `Frame::unchanged`, so the pipeline skips an OCR pass it already
//! paid for. This is the dwell re-check.
//!
//! **Threading.** `open` creates the connection, queue, event loop, and
//! shm pools. The core Worker runs them on its own thread.
//! This backend does not touch the daemon queue. A copy therefore cannot
//! delay cursor events, and two threads never dispatch one queue.
//!
//! **Layers.** [`geometry`] selects each output and box with pure
//! arithmetic across three coordinate spaces. [`crop`] moves pixels.
//! [`pacing`] selects a plain copy or a damage race. [`session`] owns
//! Wayland protocol objects, and [`shm`] owns buffers. This file uses all five
//! modules and is the only one that blocks.
//!
//! **The other rung.** [`backend`] selects one of two backends from the
//! advertised capability. [`portal`] is rung 2, the xdg-desktop-portal
//! ScreenCast + PipeWire fallback for compositors without screencopy.
//!
//! **What no rung can do.** [`software_cursor`] detects and reports the
//! cursor fact that no request can change. A compositor can draw a
//! software pointer into its framebuffer for every backend. [`software_cursor`]
//! reports that condition and does not send the pointer to OCR.

pub mod backend;
pub mod crop;
pub mod dump;
pub mod geometry;
pub mod pacing;
pub mod portal;
pub mod session;
pub mod shm;
pub mod software_cursor;

use crate::capture::backend::Backend;
use crate::cursor::outputs::OutputGeometry;
use crate::wayland::Advertised;
use anyhow::{Context, Result};
use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use chibipop::geom::{PhysPoint, PhysRect};
use chibipop::text::{Frame, RegionCapture};
use geometry::Piece;
use pacing::{Arm, Pacer, Step, Verdict, ARM_DEADLINE, COPY_DEADLINE, DWELL_DEADLINE};
use session::{Outcome, Session, Slot, State};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use wayland_client::Connection;
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1;
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

/// The value for `Frame::source`: the rung name used by logs and `probe`.
pub const SOURCE: &str = "wlr-screencopy";

/// The backend holds pixels for one region. An unchanged grab can then return them.
struct Held {
    region: PhysRect,
    buf: Vec<u8>,
}

/// Return whether the advertised globals support this rung.
pub fn available(globals: &[Advertised]) -> bool {
    session::available(globals)
}

/// Inputs to open a capture backend outside the Worker.
///
/// This is not [`crate::worker::Setup`]. That type carries a dictionary path
/// but no state directory. A one-shot grab opens no dictionary. Both types
/// share the ladder's two inputs: the advertised session globals and the
/// selected rung. The same startup probe supplies both, so neither path calls
/// [`backend::select`] again behind the daemon.
#[derive(Debug, Clone)]
pub struct Setup {
    /// Globals from the startup capability probe.
    pub globals: Vec<Advertised>,
    /// The rung selected by the ladder. `None` means no capture protocol is available.
    /// This is a state, not a crash.
    pub backend: Option<Backend>,
    /// Directory that stores the portal rung's restore token. A second session on rung 2
    /// can then avoid another prompt.
    pub state_dir: PathBuf,
}

/// A backend and the output layout that it reads.
pub struct Opened {
    pub backend: Box<dyn RegionCapture>,
    /// Outputs in the cursor channel's convention. A caller without an explicit box
    /// chooses from this list.
    pub outputs: Vec<OutputGeometry>,
}

/// Open the rung that the ladder selected on the caller's thread.
///
/// The Worker's backend is thread-affine by construction. See the module doc.
/// Code outside the Worker cannot borrow it, so each other caller opens its own
/// backend. The caller can reach both rungs. This is why this code is not a `grim`
/// shell-out.
///
/// `log` carries consent steps from the portal rung. These are the only open
/// steps that a user needs to see. The screencopy rung emits no log because it
/// needs no prompt.
pub fn open(setup: &Setup, log: &mut dyn FnMut(&str)) -> Result<Opened> {
    match setup.backend {
        Some(Backend::WlrScreencopy) => {
            let backend =
                WlrScreencopy::open(&setup.globals).context("opening the screencopy backend")?;
            let outputs = backend.outputs();
            Ok(Opened { backend: Box::new(backend), outputs })
        }
        Some(Backend::Portal) => {
            let outputs = crate::cursor::image_copy::probe_geometry(&setup.globals);
            log("capture: portal consent follows - a dialog may appear on your screen");
            let session = portal::PortalSession::open(
                &setup.state_dir,
                &outputs,
                PhysPoint { x: 0, y: 0 },
                None,
                |line| log(&line),
            )
            .context("opening the portal capture session")?;
            log(&format!(
                "capture: portal session {} node {} health {:?}",
                session.session_path(),
                session.node_id(),
                session.health()
            ));
            Ok(Opened { backend: Box::new(portal::PortalCapture::new(session)), outputs })
        }
        // The ladder skips an absent rung. A ladder with no rung left is a named state.
        None => anyhow::bail!("this compositor advertises no capture protocol chibipop can use"),
    }
}

/// Read one complete region with the bracket that the Worker uses.
/// This gives a caller the same hover cadence.
pub fn read(backend: &mut dyn RegionCapture, region: PhysRect) -> Result<Frame> {
    anyhow::ensure!(
        region.w > 0 && region.h > 0,
        "a {}x{} region has no pixels to grab",
        region.w,
        region.h
    );
    backend.begin_read();
    let frame = backend.grab(region);
    backend.end_read();
    let frame = frame.with_context(|| {
        format!("grabbing {}x{} at ({},{})", region.w, region.h, region.x, region.y)
    })?;
    anyhow::ensure!(
        frame.w == region.w && frame.h == region.h,
        "the backend answered {}x{} for a {}x{} request",
        frame.w,
        frame.h,
        region.w,
        region.h
    );
    Ok(frame)
}

/// Grab one arbitrary rect with a separate backend.
///
/// **The caller owns the thread.** This function blocks on a screencopy
/// roundtrip or a portal handshake on rung 2. The daemon must not call it on
/// the calloop pump. The intended caller is `spawn_anki`: clone a [`Setup`],
/// move it into a `std::thread::Builder::spawn`, and answer the pump through a
/// `calloop::channel`, as an AnkiConnect call does.
///
/// ```ignore
/// let setup = self.capture_setup();
/// let tx = self.shot_tx.clone();
/// std::thread::Builder::new().name("chibipop-shot".into()).spawn(move || {
///     let _ = tx.send(capture::oneshot(&setup, region).map_err(|e| format!("{e:#}")));
/// })?;
/// ```
///
/// A separate backend per grab prevents shared access. The Worker's backend is
/// thread-affine, so a second reader makes two threads dispatch one queue.
pub fn oneshot(setup: &Setup, region: PhysRect) -> Result<Frame> {
    // A one-shot has no place for portal consent output. The log lives on the pump
    // thread, but this function can run on another thread. A refusal still reaches
    // the caller as an error.
    let mut opened = open(setup, &mut |_| {})?;
    read(opened.backend.as_mut(), region)
}

/// wlr-screencopy region capture on the caller's own connection.
pub struct WlrScreencopy {
    conn: Connection,
    events: EventLoop<'static, State>,
    session: Session,
    st: State,
    manager: ZwlrScreencopyManagerV1,
    /// `copy_with_damage` exists in screencopy v2 and later.
    damage_capable: bool,
    /// `buffer_done` exists in screencopy v3.
    sends_buffer_done: bool,
    copy_pool: shm::Pool,
    watch_pool: shm::Pool,
    /// Damage race that runs between reads, if present.
    watch_frame: Option<ZwlrScreencopyFrameV1>,
    pacer: Pacer,
    /// The backend holds pixels for this read and the prior read. Two generations
    /// keep tiles from one read separate from tiles in the other.
    held: Vec<Held>,
    previous: Vec<Held>,
    /// Reused across grabs. A hover must not allocate to decide.
    pieces: Vec<Piece>,
    geoms: Vec<OutputGeometry>,
    bytes: Vec<u8>,
    /// Report an absent damage race once, not once per grab.
    said_no_race: bool,
}

impl WlrScreencopy {
    /// Bind the backend on the caller's thread.
    ///
    /// `globals` is the daemon's startup probe. The selected rung comes from
    /// advertised compositor capabilities and nothing else.
    pub fn open(globals: &[Advertised]) -> Result<WlrScreencopy> {
        let bound = session::bind(globals)?;
        let damage_capable = bound.damage_capable();
        let sends_buffer_done = bound.sends_buffer_done();
        let session::Bound { conn, queue, state, manager, shm, .. } = bound;
        let qh = queue.handle();
        let copy_pool = shm::Pool::new(&shm, &qh, "copy", 4)?;
        let watch_pool = shm::Pool::new(&shm, &qh, "watch", 4)?;
        let events: EventLoop<'static, State> =
            EventLoop::try_new().context("creating the capture event loop")?;
        WaylandSource::new(conn.clone(), queue)
            .insert(events.handle())
            .context("registering the capture Wayland source")?;
        Ok(WlrScreencopy {
            conn,
            session: Session::new(qh),
            events,
            st: state,
            manager,
            damage_capable,
            sends_buffer_done,
            copy_pool,
            watch_pool,
            watch_frame: None,
            pacer: Pacer::default(),
            held: Vec::new(),
            previous: Vec::new(),
            pieces: Vec::new(),
            geoms: Vec::new(),
            bytes: Vec::new(),
            said_no_race: false,
        })
    }

    /// Return the number of outputs known to the backend for the dump hook.
    pub fn outputs(&self) -> Vec<OutputGeometry> {
        self.st.outputs.iter().map(|o| o.geom).collect()
    }

    /// Dispatch until `done` returns true or `deadline` arrives. Do not dispatch
    /// after the deadline.
    ///
    /// Every wait in this backend has a deadline. Compositor silence cannot block
    /// a lookup.
    fn pump(&mut self, deadline: Instant, done: impl Fn(&State) -> bool) -> Result<()> {
        self.conn.flush().context("flushing the capture connection")?;
        while !done(&self.st) {
            let now = Instant::now();
            if now >= deadline {
                return Ok(());
            }
            self.events
                .dispatch(Some(deadline - now), &mut self.st)
                .context("dispatching capture events")?;
        }
        Ok(())
    }

    /// Return pixels held for `region` and promote them into this read.
    fn take_held(&mut self, region: PhysRect) -> Option<&[u8]> {
        if let Some(i) = self.held.iter().position(|h| h.region == region) {
            return Some(&self.held[i].buf);
        }
        let i = self.previous.iter().position(|h| h.region == region)?;
        let entry = self.previous.remove(i);
        self.held.push(entry);
        self.held.last().map(|h| h.buf.as_slice())
    }

    /// Keep the pixels for the next read.
    fn hold(&mut self, region: PhysRect, buf: &[u8]) {
        match self.held.iter_mut().find(|h| h.region == region) {
            Some(slot) => {
                slot.buf.clear();
                slot.buf.extend_from_slice(buf);
            }
            None => self.held.push(Held { region, buf: buf.to_vec() }),
        }
    }

    /// Settle the damage race and report whether the screen changed.
    ///
    /// If no race is active, there is no evidence. Copy the region instead.
    /// Do not return stale pixels.
    fn settle(&mut self) -> Result<Verdict> {
        let Some(frame) = self.watch_frame.clone() else {
            self.pacer.disarmed();
            return Ok(Verdict::Damaged);
        };
        self.pump(Instant::now() + DWELL_DEADLINE, |st| st.watch.outcome.is_some())?;
        if self.st.watch.outcome.is_none() {
            // The deadline won. No damage reached the watched box, so the held pixels
            // still match the screen. Keep the race active. A static dwell then needs no copies.
            return Ok(Verdict::Static);
        }
        frame.destroy();
        self.watch_frame = None;
        self.pacer.disarmed();
        Ok(Verdict::Damaged)
    }

    /// Arm a damage race on `watch` for the next read.
    fn arm(&mut self, watch: PhysRect) -> Result<()> {
        anyhow::ensure!(self.damage_capable, "this compositor's screencopy has no copy_with_damage");
        self.st.geometries(&mut self.geoms);
        geometry::split(&self.geoms, watch, &mut self.pieces);
        // Use one race for one output. A box across two outputs has two damage
        // streams, so one verdict cannot cover both.
        anyhow::ensure!(self.pieces.len() == 1, "the watched box spans {} outputs", self.pieces.len());
        let piece = self.pieces[0];
        let output = self.st.outputs[piece.output].output.clone();
        self.st.watch = session::FrameSlot::default();
        let frame = self.session.capture(&self.manager, &output, piece.logical, Slot::Watch);
        let armed = self.enumerate(Slot::Watch, ARM_DEADLINE).and_then(|shape| {
            let buffer = self.watch_pool.buffer(
                self.session.handle(),
                shape.w,
                shape.h,
                shape.stride,
                shape.format,
            )?;
            frame.copy_with_damage(&buffer);
            self.conn.flush().context("flushing the armed damage race")
        });
        match armed {
            Ok(()) => {
                self.watch_frame = Some(frame);
                Ok(())
            }
            Err(e) => {
                frame.destroy();
                Err(e)
            }
        }
    }

    /// Drop the active damage race, if one exists.
    fn disarm(&mut self) {
        if let Some(frame) = self.watch_frame.take() {
            // Destroy the old race before the next copy request. The old race then cannot
            // write to a buffer that the next grab reads. The compositor handles requests
            // in order.
            frame.destroy();
        }
    }

    /// Wait for a frame's buffer enumeration for no more than `within`.
    fn enumerate(&mut self, slot: Slot, within: Duration) -> Result<session::Shape> {
        let buffer_done = self.sends_buffer_done;
        self.pump(Instant::now() + within, move |st| {
            let s = st.slot(slot);
            s.outcome.is_some() || if buffer_done { s.enumerated } else { s.shape.is_some() }
        })?;
        let s = self.st.slot(slot);
        if s.outcome == Some(Outcome::Failed) {
            anyhow::bail!("the compositor failed the frame before offering a buffer");
        }
        s.shape.with_context(|| {
            format!("the compositor offered no shm buffer within {within:?}")
        })
    }

    /// Copy `region` from every output that it covers.
    fn copy_region(&mut self, region: PhysRect) -> Result<Vec<u8>> {
        self.st.geometries(&mut self.geoms);
        geometry::split(&self.geoms, region, &mut self.pieces);
        anyhow::ensure!(
            !self.pieces.is_empty(),
            "{}x{} at ({},{}) is on no output",
            region.w,
            region.h,
            region.x,
            region.y
        );
        let len = i64::from(region.w) * i64::from(region.h) * 4;
        let len = usize::try_from(len).with_context(|| format!("a {}x{} region", region.w, region.h))?;
        // Fill areas outside outputs with black. Off-screen areas have no pixels.
        // Blank pixels cost nothing for one hover, but a failed grab costs every edge hover.
        let mut out = vec![0u8; len];
        for i in 0..self.pieces.len() {
            let piece = self.pieces[i];
            self.copy_piece(piece, region, &mut out)?;
        }
        Ok(out)
    }

    /// Copy one output's share of a region.
    fn copy_piece(&mut self, piece: Piece, region: PhysRect, out: &mut [u8]) -> Result<()> {
        let output = self.st.outputs[piece.output].output.clone();
        self.st.copy = session::FrameSlot::default();
        let frame = self.session.capture(&self.manager, &output, piece.logical, Slot::Copy);
        let copied = self.copy_into(&frame, piece, region, out);
        // The protocol allows one use. A ready or failed frame is spent.
        frame.destroy();
        copied
    }

    fn copy_into(
        &mut self,
        frame: &ZwlrScreencopyFrameV1,
        piece: Piece,
        region: PhysRect,
        out: &mut [u8],
    ) -> Result<()> {
        let shape = self.enumerate(Slot::Copy, COPY_DEADLINE)?;
        let order = shape.order().with_context(|| {
            format!("the compositor offered {:?}, which core's Frame cannot describe", shape.format)
        })?;
        anyhow::ensure!(
            shape.stride >= shape.w * order.bpp(),
            "a {}-wide {:?} buffer cannot have stride {}",
            shape.w,
            shape.format,
            shape.stride
        );
        let buffer = self.copy_pool.buffer(
            self.session.handle(),
            shape.w,
            shape.h,
            shape.stride,
            shape.format,
        )?;
        frame.copy(&buffer);
        self.pump(Instant::now() + COPY_DEADLINE, |st| st.copy.outcome.is_some())?;
        match self.st.copy.outcome {
            Some(Outcome::Ready) => {}
            Some(Outcome::Failed) => anyhow::bail!("the compositor failed the copy"),
            // The protocol has no third answer. `copy` is followed by `flags` and
            // `ready`, or by `failed` (`wlr-screencopy-unstable-v1`, the `copy`
            // request). Silence means neither answer. A compositor shows this
            // behavior for an output that it does not repaint. Hyprland 0.55.4
            // left a DPMS-off display unanswered, although the same awake
            // display answered in 2 ms. `COPY_DEADLINE` is 400 ms. Report that
            // condition, not the timer.
            None => anyhow::bail!(
                "the copy went unanswered for {COPY_DEADLINE:?}: the compositor owes ready \
                 or failed and sent neither, which is what an output nothing is repainting \
                 does - a display asleep or powered off"
            ),
        }
        let cut = geometry::cut(&piece, shape.w, shape.h)
            .context("the copied buffer holds none of the requested pixels")?;
        let len = (shape.stride as i64 * shape.h as i64) as usize;
        self.copy_pool.read(len, &mut self.bytes)?;
        let buf = crop::Buffer {
            w: shape.w,
            h: shape.h,
            stride: shape.stride,
            order,
            y_invert: self.st.copy.y_invert,
        };
        crop::blit(&self.bytes, &buf, cut.src, cut.dest, out, region.w, region.h);
        Ok(())
    }
}

impl RegionCapture for WlrScreencopy {
    fn grab(&mut self, region: PhysRect) -> Result<Frame> {
        anyhow::ensure!(
            region.w > 0 && region.h > 0,
            "a {}x{} region has no pixels",
            region.w,
            region.h
        );
        let cached = self.take_held(region).is_some();
        let mut step = self.pacer.step(region, cached);
        if step == Step::Settle {
            let verdict = self.settle()?;
            self.pacer.settled(verdict);
            step = self.pacer.step(region, true);
        }
        if step == Step::Serve {
            if let Some(buf) = self.take_held(region) {
                return Ok(Frame {
                    buf: buf.to_vec(),
                    w: region.w,
                    h: region.h,
                    source: SOURCE,
                    fallback: None,
                    unchanged: true,
                });
            }
        }
        let buf = self.copy_region(region)?;
        self.hold(region, &buf);
        Ok(Frame { buf, w: region.w, h: region.h, source: SOURCE, fallback: None, unchanged: false })
    }

    fn bounds_containing(&self, p: PhysPoint) -> PhysRect {
        let mut geoms = Vec::with_capacity(self.st.outputs.len());
        geoms.extend(self.st.outputs.iter().map(|o| o.geom));
        geometry::bounds_containing(&geoms, p)
    }

    /// Start a read. Clear the prior pacing verdict and move its pixels into the
    /// generation that a dwell can reuse.
    fn begin_read(&mut self) {
        self.pacer.begin_read();
        self.previous = std::mem::take(&mut self.held);
    }

    /// End a read. Leave a damage race that watches exactly the read's regions.
    /// The next read can then distinguish static content from changed content
    /// without another copy.
    fn end_read(&mut self) {
        match self.pacer.end_read(self.watch_frame.is_some()) {
            Arm::Keep => {}
            Arm::Disarm => self.disarm(),
            Arm::Rearm(watch) => {
                self.disarm();
                if let Err(e) = self.arm(watch) {
                    self.pacer.disarmed();
                    if !self.said_no_race {
                        self.said_no_race = true;
                        eprintln!(
                            "chibipop: capture damage pacing unavailable ({e:#}); \
                             every hover will copy"
                        );
                    }
                }
            }
        }
    }
}

impl Drop for WlrScreencopy {
    fn drop(&mut self) {
        self.disarm();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advertised(interface: &str) -> Advertised {
        Advertised { name: 1, interface: interface.to_string(), version: 3 }
    }

    #[test]
    fn the_rung_needs_its_globals() {
        let globals = [advertised(session::MANAGER_GLOBAL), advertised("wl_shm"), advertised("wl_output")];
        assert!(available(&globals));
        assert!(!available(&[advertised("wl_shm"), advertised("wl_output")]));
    }

    /// The ladder must skip an absent global without a panic.
    #[test]
    fn opening_without_the_global_is_an_error_not_a_panic() {
        let Err(err) = WlrScreencopy::open(&[advertised("wl_compositor")]) else {
            panic!("a compositor without the global must not open a backend");
        };
        assert!(format!("{err:#}").contains(session::MANAGER_GLOBAL), "{err:#}");
    }
}
