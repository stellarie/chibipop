//! The wlr-screencopy capture backend: the primary Linux
//! `RegionCapture` (ARCHITECTURE.md#capture-and-masking).
//!
//! It is the only protocol with compositor-side *region* capture and it
//! prompts for nothing, so on Hyprland/sway/niri a hover works the
//! instant the daemon starts. Selection is by advertised global
//! (`available`), never by compositor identity: absence is a rung that
//! is not there, and the ladder moves on.
//!
//! **Shape.** Pull-shaped, as the trait demands: `grab` answers with
//! the most recent content of the box and never waits for damage. A
//! plain `copy` is what a changed region gets; a region asked for twice
//! is a dwell, and dwells run the damage race in [`pacing`] instead -
//! whose timeout is not a failure but the answer *nothing changed*,
//! handed up as `Frame::unchanged` so the pipeline can skip an OCR pass
//! it has already paid for (the dwell re-check).
//!
//! **Threading.** Thread-affine by construction: the connection, the
//! queue, the loop and the shm pools are all made in `open`, which the
//! core Worker runs on its own thread. The daemon's queue is never
//! touched, so a copy can never stall cursor events and two threads
//! never dispatch one queue.
//!
//! **Layers.** [`geometry`] decides which output and which box (three
//! coordinate spaces, all pure arithmetic), [`crop`] moves the pixels,
//! [`pacing`] decides plain-copy versus race, [`session`] owns the
//! Wayland plumbing, [`shm`] the buffers. Only this file needs all
//! five, and only this file blocks.
//!
//! **The other rung.** [`backend`] is the ladder itself: which of the
//! two backends this session gets, by advertised capability. [`portal`]
//! is rung 2, the xdg-desktop-portal ScreenCast + PipeWire fallback for
//! the compositors with no screencopy at all.
//!
//! **What no rung can do.** [`software_cursor`] is the one cursor fact
//! that is not a request: a compositor drawing a software pointer into
//! its framebuffer hands it to every backend here, so that condition is
//! detected and said out loud rather than silently OCR'd.

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

/// What `Frame::source` says: the rung, for logs and `probe`.
pub const SOURCE: &str = "wlr-screencopy";

/// Pixels held for one region, so an unchanged grab has something
/// honest to answer with.
struct Held {
    region: PhysRect,
    buf: Vec<u8>,
}

/// Advertised globals enough for this rung.
pub fn available(globals: &[Advertised]) -> bool {
    session::available(globals)
}

/// What opening a capture backend *outside* the Worker needs.
///
/// Deliberately not [`crate::worker::Setup`]: that one carries the
/// dictionary path and no state dir, and a one-shot grab opens no
/// dictionary. What the two share is the only two inputs the ladder
/// has: what this session advertises, and which rung it picked.
/// Both are read from the same startup probe, so neither re-runs
/// [`backend::select`] behind the daemon's back.
#[derive(Debug, Clone)]
pub struct Setup {
    /// The startup capability probe.
    pub globals: Vec<Advertised>,
    /// Which rung the ladder picked. `None` is a session with no
    /// capture protocol at all, which is a state and not a crash.
    pub backend: Option<Backend>,
    /// Where the portal rung's restore token lives, so a second
    /// session on rung 2 is silent instead of prompting again.
    pub state_dir: PathBuf,
}

/// A backend and the output layout it reads through.
pub struct Opened {
    pub backend: Box<dyn RegionCapture>,
    /// The outputs, in the cursor channel's convention: what a caller
    /// with no explicit box has to pick one from.
    pub outputs: Vec<OutputGeometry>,
}

/// Open the rung the ladder picked, on the calling thread.
///
/// The Worker's backend is thread-affine by construction (see the
/// module doc), so nothing outside the Worker may borrow it: everything
/// else opens its own here. Both rungs are reachable, which is the
/// whole reason this is not a `grim` shell-out.
///
/// `log` carries the portal rung's consent steps, which are the only
/// part of an open a user ever needs to see. The screencopy rung says
/// nothing: it prompts for nothing.
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
        // An absent rung is a rung the ladder skips, and a ladder with
        // no rungs left is a named state.
        None => anyhow::bail!("this compositor advertises no capture protocol chibipop can use"),
    }
}

/// One whole read - bracket included, exactly as the Worker reads - so
/// the pacing a caller sees is the pacing a hover gets.
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

/// One arbitrary-rect grab with a backend of its own.
///
/// **The caller owns the thread.** This blocks - a screencopy round
/// trip, or a whole portal handshake on rung 2 - so the daemon must
/// never call it on the calloop pump. The shape it is written for is
/// `spawn_anki`'s: clone a [`Setup`], move it into a
/// `std::thread::Builder::spawn`, and answer the pump over a
/// `calloop::channel` the way an AnkiConnect call answers it.
///
/// ```ignore
/// let setup = self.capture_setup();
/// let tx = self.shot_tx.clone();
/// std::thread::Builder::new().name("chibipop-shot".into()).spawn(move || {
///     let _ = tx.send(capture::oneshot(&setup, region).map_err(|e| format!("{e:#}")));
/// })?;
/// ```
///
/// A backend per grab rather than a shared one is the point: the
/// Worker's is thread-affine and a second reader on it would have two
/// threads dispatching one queue.
pub fn oneshot(setup: &Setup, region: PhysRect) -> Result<Frame> {
    // A one-shot has nowhere to put the portal's consent chatter: the
    // log lives on the pump thread, and this function may be running on
    // a thread of its own. A refusal still travels, as the error.
    let mut opened = open(setup, &mut |_| {})?;
    read(opened.backend.as_mut(), region)
}

/// wlr-screencopy region capture, on this thread's own connection.
pub struct WlrScreencopy {
    conn: Connection,
    events: EventLoop<'static, State>,
    session: Session,
    st: State,
    manager: ZwlrScreencopyManagerV1,
    /// `copy_with_damage` exists (screencopy v2+).
    damage_capable: bool,
    /// `buffer_done` exists (screencopy v3).
    sends_buffer_done: bool,
    copy_pool: shm::Pool,
    watch_pool: shm::Pool,
    /// The damage race in flight between reads, if any.
    watch_frame: Option<ZwlrScreencopyFrameV1>,
    pacer: Pacer,
    /// This read's pixels, and the previous read's: two generations, so
    /// one read's tiles cannot evict each other.
    held: Vec<Held>,
    previous: Vec<Held>,
    /// Reused across grabs; a hover must not allocate to think.
    pieces: Vec<Piece>,
    geoms: Vec<OutputGeometry>,
    bytes: Vec<u8>,
    /// A missing damage race is said once, not once per grab.
    said_no_race: bool,
}

impl WlrScreencopy {
    /// Bind the backend on the calling thread.
    ///
    /// `globals` is the daemon's startup probe: this rung is chosen
    /// from what the compositor advertises and nothing else.
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

    /// How many outputs the backend knows; for the dump hook.
    pub fn outputs(&self) -> Vec<OutputGeometry> {
        self.st.outputs.iter().map(|o| o.geom).collect()
    }

    /// Dispatch until `done`, or until `deadline`. Never longer.
    ///
    /// This is the whole never-block contract in one place: every wait
    /// in this backend has a deadline, so no compositor behaviour -
    /// silence included - can hang a lookup.
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

    /// Pixels held for `region`, promoted into this read.
    fn take_held(&mut self, region: PhysRect) -> Option<&[u8]> {
        if let Some(i) = self.held.iter().position(|h| h.region == region) {
            return Some(&self.held[i].buf);
        }
        let i = self.previous.iter().position(|h| h.region == region)?;
        let entry = self.previous.remove(i);
        self.held.push(entry);
        self.held.last().map(|h| h.buf.as_slice())
    }

    /// Keep these pixels for the next read.
    fn hold(&mut self, region: PhysRect, buf: &[u8]) {
        match self.held.iter_mut().find(|h| h.region == region) {
            Some(slot) => {
                slot.buf.clear();
                slot.buf.extend_from_slice(buf);
            }
            None => self.held.push(Held { region, buf: buf.to_vec() }),
        }
    }

    /// Settle the damage race: did anything change?
    ///
    /// No race in flight means no evidence, and no evidence means
    /// copy - never a stale answer.
    fn settle(&mut self) -> Result<Verdict> {
        let Some(frame) = self.watch_frame.clone() else {
            self.pacer.disarmed();
            return Ok(Verdict::Damaged);
        };
        self.pump(Instant::now() + DWELL_DEADLINE, |st| st.watch.outcome.is_some())?;
        if self.st.watch.outcome.is_none() {
            // The deadline won: nothing has damaged the watched box, so
            // the pixels we hold are still the screen's. The race stays
            // in flight, which is why a static dwell costs no copies.
            return Ok(Verdict::Static);
        }
        frame.destroy();
        self.watch_frame = None;
        self.pacer.disarmed();
        Ok(Verdict::Damaged)
    }

    /// Arm a damage race on `watch` for the next read to settle.
    fn arm(&mut self, watch: PhysRect) -> Result<()> {
        anyhow::ensure!(self.damage_capable, "this compositor's screencopy has no copy_with_damage");
        self.st.geometries(&mut self.geoms);
        geometry::split(&self.geoms, watch, &mut self.pieces);
        // One race, one output: a box spanning two outputs has two
        // damage streams, and a single verdict could not honour both.
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

    /// Drop the race in flight, if any.
    fn disarm(&mut self) {
        if let Some(frame) = self.watch_frame.take() {
            // Destroying before the next copy request is what keeps a
            // retired race from writing into a buffer the next grab is
            // reading: the compositor handles our requests in order.
            frame.destroy();
        }
    }

    /// Wait for the buffer enumeration a frame opens with, no longer
    /// than `deadline` allows.
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

    /// Copy `region` fresh, over however many outputs it covers.
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
        // Black where no output reaches: off-screen has no pixels, and
        // blank costs one hover nothing where failing costs every hover
        // near an edge.
        let mut out = vec![0u8; len];
        for i in 0..self.pieces.len() {
            let piece = self.pieces[i];
            self.copy_piece(piece, region, &mut out)?;
        }
        Ok(out)
    }

    /// One output's share of one region.
    fn copy_piece(&mut self, piece: Piece, region: PhysRect, out: &mut [u8]) -> Result<()> {
        let output = self.st.outputs[piece.output].output.clone();
        self.st.copy = session::FrameSlot::default();
        let frame = self.session.capture(&self.manager, &output, piece.logical, Slot::Copy);
        let copied = self.copy_into(&frame, piece, region, out);
        // Single-use by protocol: ready or failed, it is spent.
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
            // The protocol leaves no third answer: `copy` is followed by
            // `flags` and `ready`, or by `failed`
            // (wlr-screencopy-unstable-v1, the `copy` request). Silence is
            // neither, and it is what a compositor does for an output it is
            // not repainting - measured on Hyprland 0.55.4 with the display
            // DPMS-off, where the very grab that answers in 2 ms awake is
            // still unanswered at 10 s. So the refusal names that condition
            // rather than the timer, which is already 85x the cost of a copy
            // this compositor does answer.
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

    /// Open a read: this read's pacing verdict is not in yet, and last
    /// read's pixels become the generation a dwell may reuse.
    fn begin_read(&mut self) {
        self.pacer.begin_read();
        self.previous = std::mem::take(&mut self.held);
    }

    /// Close a read: leave a damage race watching exactly what this
    /// read looked at, so the next one can tell static from changed
    /// without copying anything.
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

    /// Absence must fall through the ladder, not crash it.
    #[test]
    fn opening_without_the_global_is_an_error_not_a_panic() {
        let Err(err) = WlrScreencopy::open(&[advertised("wl_compositor")]) else {
            panic!("a compositor without the global must not open a backend");
        };
        assert!(format!("{err:#}").contains(session::MANAGER_GLOBAL), "{err:#}");
    }
}
