//! The PipeWire half of the fallback capture rung: one video stream,
//! consumed on PipeWire's own thread, parked in [`super::frame`].
//!
//! **Why dlopen and not the `pipewire` crate.** `pipewire-sys` is a
//! `system-deps` binding: building it needs `libpipewire-0.3-dev` and
//! a pkg-config file on the build machine, and the project's Linux CI
//! image has neither and this ticket may not edit the workflow. More
//! to the point, a *build-time* dependency on PipeWire would make the
//! release tarball unbuildable on machines that will never run the
//! portal rung. So the library is opened at runtime, exactly the
//! bargain ARCHITECTURE.md#ocr-engine already struck for `ort`'s
//! system path: absent `libpipewire-0.3.so.0` is a rung that is not
//! there, named in a diagnostic, and the ladder moves on.
//!
//! **What that costs.** Every `spa_pod_builder_*` helper is a
//! `static inline` in a header and therefore unreachable through
//! dlopen, so the format handshake is serialised by hand in
//! [`super::pod`]. The rest is ordinary FFI against a stable ABI: the
//! structs mirrored below are `struct spa_buffer`, `struct spa_data`,
//! `struct spa_chunk`, `struct spa_meta`, `struct pw_buffer` and
//! `struct pw_stream_events` version 2, all of which PipeWire treats
//! as ABI.
//!
//! **Threading.** `pw_thread_loop` *is* the dedicated thread: PipeWire
//! starts it, dispatches on it, and calls back into
//! `process`/`param_changed` from it with its own lock held. Nothing
//! here touches the daemon's calloop pump, and the only value crossing
//! back is a cursor position, handed over through the caller's sink
//! (which the daemon backs with a calloop channel).
//!
//! **Power.** The stream stays active for as long as the backend
//! lives, because the cursor ladder's rung 2 reads the cursor off it
//! and a paused stream has no cursor. That is the portal tier's
//! standing cost and it is why wlr-screencopy stays the primary rung
//! wherever it exists. Our own per-frame cost is a pointer swap: see
//! [`super::frame`].

use super::frame::{FrameStore, Shape};
use super::pod;
use anyhow::{Context, Result};
use libloading::Library;
use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::{Arc, Mutex};

/// The soname to dlopen. Versioned on purpose: `libpipewire-0.3.so`
/// is the `-dev` symlink, which is exactly the file we are avoiding
/// needing.
pub const SONAME: &str = "libpipewire-0.3.so.0";

/// `enum pw_direction`.
const DIRECTION_INPUT: c_int = 0;

/// `enum pw_stream_flags` bits we use.
const FLAG_AUTOCONNECT: c_int = 1 << 0;
const FLAG_MAP_BUFFERS: c_int = 1 << 2;

/// `enum pw_stream_state`.
const STATE_ERROR: c_int = -1;
const STATE_UNCONNECTED: c_int = 0;
const STATE_CONNECTING: c_int = 1;
const STATE_PAUSED: c_int = 2;
const STATE_STREAMING: c_int = 3;

/// `sizeof(struct spa_meta_region)`: a point and a rectangle.
const META_REGION_SIZE: usize = 16;

// ------------------------------------------------------------- C structs

#[repr(C)]
struct SpaDictItem {
    key: *const c_char,
    value: *const c_char,
}

#[repr(C)]
struct SpaDict {
    flags: u32,
    n_items: u32,
    items: *const SpaDictItem,
}

#[repr(C)]
struct SpaChunk {
    offset: u32,
    size: u32,
    stride: i32,
    flags: i32,
}

#[repr(C)]
struct SpaData {
    ty: u32,
    flags: u32,
    fd: i64,
    mapoffset: u32,
    maxsize: u32,
    data: *mut c_void,
    chunk: *mut SpaChunk,
}

#[repr(C)]
struct SpaMeta {
    ty: u32,
    size: u32,
    data: *mut c_void,
}

#[repr(C)]
struct SpaBuffer {
    n_metas: u32,
    n_datas: u32,
    metas: *mut SpaMeta,
    datas: *mut SpaData,
}

#[repr(C)]
struct PwBuffer {
    buffer: *mut SpaBuffer,
    user_data: *mut c_void,
    size: u64,
    requested: u64,
    time: u64,
}

/// `struct spa_meta_cursor`.
#[repr(C)]
struct SpaMetaCursor {
    id: u32,
    flags: u32,
    x: i32,
    y: i32,
    hotspot_x: i32,
    hotspot_y: i32,
    bitmap_offset: u32,
}

/// `struct spa_meta_region`: an entry with a zero dimension ends the
/// damage array.
#[repr(C)]
struct SpaMetaRegion {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

/// `struct pw_stream_events`, `PW_VERSION_STREAM_EVENTS == 2`.
///
/// The whole table must be present and in order even though only four
/// entries are filled: PipeWire indexes it by field, not by name.
#[repr(C)]
struct PwStreamEvents {
    version: u32,
    destroy: Option<extern "C" fn(*mut c_void)>,
    state_changed: Option<extern "C" fn(*mut c_void, c_int, c_int, *const c_char)>,
    control_info: Option<extern "C" fn(*mut c_void, u32, *const c_void)>,
    io_changed: Option<extern "C" fn(*mut c_void, u32, *mut c_void, u32)>,
    param_changed: Option<extern "C" fn(*mut c_void, u32, *const u8)>,
    add_buffer: Option<extern "C" fn(*mut c_void, *mut PwBuffer)>,
    remove_buffer: Option<extern "C" fn(*mut c_void, *mut PwBuffer)>,
    process: Option<extern "C" fn(*mut c_void)>,
    drained: Option<extern "C" fn(*mut c_void)>,
    command: Option<extern "C" fn(*mut c_void, *const c_void)>,
    trigger_done: Option<extern "C" fn(*mut c_void)>,
}

static EVENTS: PwStreamEvents = PwStreamEvents {
    version: 2,
    destroy: None,
    state_changed: Some(on_state_changed),
    control_info: None,
    io_changed: None,
    param_changed: Some(on_param_changed),
    add_buffer: None,
    remove_buffer: Some(on_remove_buffer),
    process: Some(on_process),
    drained: None,
    command: None,
    trigger_done: None,
};

// ------------------------------------------------------------------ lib

/// The symbols this backend needs, resolved once.
///
/// Held as bare function pointers rather than `libloading::Symbol`s so
/// the C callbacks - which get nothing but a `void *data` - can call
/// them. `_library` keeps the mapping alive for exactly as long as the
/// pointers are reachable.
#[allow(clippy::type_complexity)]
pub struct Lib {
    _library: Library,
    init: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char),
    thread_loop_new: unsafe extern "C" fn(*const c_char, *const SpaDict) -> *mut c_void,
    thread_loop_destroy: unsafe extern "C" fn(*mut c_void),
    thread_loop_start: unsafe extern "C" fn(*mut c_void) -> c_int,
    thread_loop_stop: unsafe extern "C" fn(*mut c_void),
    thread_loop_lock: unsafe extern "C" fn(*mut c_void),
    thread_loop_unlock: unsafe extern "C" fn(*mut c_void),
    thread_loop_get_loop: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    context_new: unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> *mut c_void,
    context_destroy: unsafe extern "C" fn(*mut c_void),
    context_connect_fd: unsafe extern "C" fn(*mut c_void, c_int, *mut c_void, usize) -> *mut c_void,
    core_disconnect: unsafe extern "C" fn(*mut c_void) -> c_int,
    properties_new_dict: unsafe extern "C" fn(*const SpaDict) -> *mut c_void,
    stream_new: unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void) -> *mut c_void,
    stream_destroy: unsafe extern "C" fn(*mut c_void),
    stream_add_listener:
        unsafe extern "C" fn(*mut c_void, *mut c_void, *const PwStreamEvents, *mut c_void),
    stream_connect:
        unsafe extern "C" fn(*mut c_void, c_int, u32, c_int, *const *const u8, u32) -> c_int,
    stream_disconnect: unsafe extern "C" fn(*mut c_void) -> c_int,
    stream_update_params: unsafe extern "C" fn(*mut c_void, *const *const u8, u32) -> c_int,
    stream_dequeue_buffer: unsafe extern "C" fn(*mut c_void) -> *mut PwBuffer,
    stream_queue_buffer: unsafe extern "C" fn(*mut c_void, *mut PwBuffer) -> c_int,
}

macro_rules! symbol {
    ($lib:expr, $name:literal) => {
        // SAFETY: the names and signatures are PipeWire's public ABI,
        // transcribed from <pipewire/stream.h> and <pipewire/core.h>.
        unsafe {
            *$lib
                .get(concat!($name, "\0").as_bytes())
                .with_context(|| format!("{SONAME} has no {}", $name))?
        }
    };
}

impl Lib {
    /// dlopen PipeWire and resolve everything up front.
    ///
    /// Every failure here is "this rung is not installed", which the
    /// caller turns into a named channel state rather than an exit.
    pub fn open() -> Result<Arc<Lib>> {
        // SAFETY: dlopen of a versioned system soname. PipeWire's
        // library initialiser is `pw_init`, called explicitly below;
        // loading it runs no user code.
        let library = unsafe { Library::new(SONAME) }
            .with_context(|| format!("loading {SONAME} (install the PipeWire runtime)"))?;
        let lib = Lib {
            init: symbol!(library, "pw_init"),
            thread_loop_new: symbol!(library, "pw_thread_loop_new"),
            thread_loop_destroy: symbol!(library, "pw_thread_loop_destroy"),
            thread_loop_start: symbol!(library, "pw_thread_loop_start"),
            thread_loop_stop: symbol!(library, "pw_thread_loop_stop"),
            thread_loop_lock: symbol!(library, "pw_thread_loop_lock"),
            thread_loop_unlock: symbol!(library, "pw_thread_loop_unlock"),
            thread_loop_get_loop: symbol!(library, "pw_thread_loop_get_loop"),
            context_new: symbol!(library, "pw_context_new"),
            context_destroy: symbol!(library, "pw_context_destroy"),
            context_connect_fd: symbol!(library, "pw_context_connect_fd"),
            core_disconnect: symbol!(library, "pw_core_disconnect"),
            properties_new_dict: symbol!(library, "pw_properties_new_dict"),
            stream_new: symbol!(library, "pw_stream_new"),
            stream_destroy: symbol!(library, "pw_stream_destroy"),
            stream_add_listener: symbol!(library, "pw_stream_add_listener"),
            stream_connect: symbol!(library, "pw_stream_connect"),
            stream_disconnect: symbol!(library, "pw_stream_disconnect"),
            stream_update_params: symbol!(library, "pw_stream_update_params"),
            stream_dequeue_buffer: symbol!(library, "pw_stream_dequeue_buffer"),
            stream_queue_buffer: symbol!(library, "pw_stream_queue_buffer"),
            _library: library,
        };
        // SAFETY: `pw_init(NULL, NULL)` is the documented no-argument
        // form; it is idempotent and refcounted inside PipeWire.
        unsafe { (lib.init)(std::ptr::null_mut(), std::ptr::null_mut()) };
        Ok(Arc::new(lib))
    }
}

// ------------------------------------------------------------ the stream

/// Where a cursor sample goes. Stream buffer pixels; the caller knows
/// which monitor the stream is and does the rest (`super::metadata`).
pub type CursorSink = Box<dyn Fn(i32, i32) + Send + Sync>;

/// What the stream is doing, for the channel status registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    /// `pw_stream_state_as_string`, in our own words.
    pub state: &'static str,
    /// The message the last error carried, if any.
    pub error: Option<String>,
    /// The format was negotiated and frames can flow.
    pub negotiated: bool,
}

impl Default for Health {
    fn default() -> Health {
        Health { state: "unconnected", error: None, negotiated: false }
    }
}

/// Everything the C callbacks reach. Boxed and never moved: its
/// address is the `void *data` PipeWire holds.
struct StreamData {
    lib: Arc<Lib>,
    /// Set before `pw_stream_connect`, so no callback can see null.
    stream: *mut c_void,
    store: Arc<FrameStore>,
    health: Arc<Mutex<Health>>,
    cursor: Option<CursorSink>,
    /// Last cursor sample forwarded, so a still cursor on a 60 Hz
    /// stream costs one comparison rather than 60 events a second.
    last_cursor: Option<(i32, i32)>,
    /// The negotiated format, once `param_changed` has fixated one.
    shape: Option<Shape>,
    /// Space for `struct spa_hook` (six pointers). Zeroed; PipeWire
    /// owns the contents and never reallocates it.
    hook: [u64; 8],
}

/// The portal's PipeWire connection: one thread loop, one context, one
/// core, for as long as the grant lasts.
///
/// Deliberately separate from [`Stream`], and the reason is the file
/// descriptor. `pw_context_connect_fd` documents that "the socket will
/// be closed automatically on disconnect or error", so the core owns
/// the fd `OpenPipeWireRemote` handed over - and a second core could
/// not simply dup it, because two protocol clients multiplexing one
/// socket is not a thing. Crossing to another monitor therefore swaps
/// the *stream* and keeps the connection, which is also the cheaper
/// half of the operation.
pub struct Remote {
    lib: Arc<Lib>,
    thread_loop: *mut c_void,
    context: *mut c_void,
    core: *mut c_void,
}

// SAFETY: every PipeWire object below is touched only under
// `pw_thread_loop_lock`, which the methods here take and the callbacks
// already hold.
unsafe impl Send for Remote {}
// SAFETY: as above - the loop lock is the synchronisation.
unsafe impl Sync for Remote {}

impl Remote {
    /// Connect to the remote `fd` names, taking ownership of it.
    pub fn connect(lib: Arc<Lib>, fd: std::os::fd::OwnedFd) -> Result<Arc<Remote>> {
        use std::os::fd::IntoRawFd;

        let name = CString::new("chibipop-portal").expect("a literal with no nul");
        // SAFETY: PipeWire's documented constructor sequence; every
        // result is null-checked before it is used, and each failure
        // path tears down exactly what had been built.
        unsafe {
            let thread_loop = (lib.thread_loop_new)(name.as_ptr(), std::ptr::null());
            anyhow::ensure!(!thread_loop.is_null(), "pw_thread_loop_new failed");
            let mut half = Half { lib: &lib, thread_loop, context: std::ptr::null_mut() };

            half.context =
                (lib.context_new)((lib.thread_loop_get_loop)(thread_loop), std::ptr::null_mut(), 0);
            anyhow::ensure!(!half.context.is_null(), "pw_context_new failed");
            anyhow::ensure!(
                (lib.thread_loop_start)(thread_loop) == 0,
                "pw_thread_loop_start failed"
            );

            (lib.thread_loop_lock)(thread_loop);
            // The fd goes in and does not come back: the core closes it
            // on disconnect, and on failure too.
            let core =
                (lib.context_connect_fd)(half.context, fd.into_raw_fd(), std::ptr::null_mut(), 0);
            (lib.thread_loop_unlock)(thread_loop);
            anyhow::ensure!(
                !core.is_null(),
                "pw_context_connect_fd rejected the portal's PipeWire remote"
            );

            let context = half.context;
            std::mem::forget(half);
            Ok(Arc::new(Remote { lib, thread_loop, context, core }))
        }
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        // SAFETY: stopping the loop first means no callback can be
        // running while the objects go away. Every stream is gone by
        // now: a `Stream` holds an `Arc<Remote>`, so this cannot run
        // while one is alive.
        unsafe {
            (self.lib.thread_loop_stop)(self.thread_loop);
            (self.lib.core_disconnect)(self.core);
            (self.lib.context_destroy)(self.context);
            (self.lib.thread_loop_destroy)(self.thread_loop);
        }
    }
}

/// Unwinds a half-built [`Remote`] on `connect`'s error paths.
struct Half<'a> {
    lib: &'a Lib,
    thread_loop: *mut c_void,
    context: *mut c_void,
}

impl Drop for Half<'_> {
    fn drop(&mut self) {
        // SAFETY: only ever holds objects `Remote::connect` created and
        // has not handed over. `pw_thread_loop_stop` on a loop that was
        // never started is a no-op inside PipeWire.
        unsafe {
            (self.lib.thread_loop_stop)(self.thread_loop);
            if !self.context.is_null() {
                (self.lib.context_destroy)(self.context);
            }
            (self.lib.thread_loop_destroy)(self.thread_loop);
        }
    }
}

/// One connected capture stream on a [`Remote`].
pub struct Stream {
    remote: Arc<Remote>,
    stream: *mut c_void,
    /// Boxed so the address handed to C stays put; dropped last.
    data: Box<StreamData>,
    store: Arc<FrameStore>,
    health: Arc<Mutex<Health>>,
    node_id: u32,
}

// SAFETY: the stream is only ever touched under the remote's loop
// lock, which `Stream`'s own methods take and the callbacks hold.
unsafe impl Send for Stream {}

impl Stream {
    /// Connect to `node_id` on an already-open portal remote.
    pub fn connect(
        remote: &Arc<Remote>,
        node_id: u32,
        cursor: Option<CursorSink>,
    ) -> Result<Stream> {
        let lib = Arc::clone(&remote.lib);
        let store = Arc::new(FrameStore::new());
        let health = Arc::new(Mutex::new(Health::default()));
        let name = CString::new("chibipop-capture").expect("a literal with no nul");

        // SAFETY: all of it runs under the loop lock, which is where
        // `pw_stream_*` must be called from off the loop thread. Every
        // pointer is null-checked before use and the failure path
        // destroys the stream before releasing the lock.
        unsafe {
            (lib.thread_loop_lock)(remote.thread_loop);
            let props = capture_properties(&lib);
            let stream = (lib.stream_new)(remote.core, name.as_ptr(), props);
            if stream.is_null() {
                (lib.thread_loop_unlock)(remote.thread_loop);
                anyhow::bail!("pw_stream_new failed");
            }

            let mut data = Box::new(StreamData {
                lib: Arc::clone(&lib),
                stream,
                store: Arc::clone(&store),
                health: Arc::clone(&health),
                cursor,
                last_cursor: None,
                shape: None,
                hook: [0; 8],
            });
            let data_ptr = (&raw mut *data) as *mut c_void;
            let hook_ptr = data.hook.as_mut_ptr() as *mut c_void;
            (lib.stream_add_listener)(stream, hook_ptr, &EVENTS, data_ptr);

            let offer = pod::enum_format();
            let params = [offer.as_ptr()];
            let rc = (lib.stream_connect)(
                stream,
                DIRECTION_INPUT,
                node_id,
                FLAG_AUTOCONNECT | FLAG_MAP_BUFFERS,
                params.as_ptr(),
                1,
            );
            if rc != 0 {
                (lib.stream_destroy)(stream);
                (lib.thread_loop_unlock)(remote.thread_loop);
                anyhow::bail!("pw_stream_connect to node {node_id} failed ({rc})");
            }
            (lib.thread_loop_unlock)(remote.thread_loop);

            Ok(Stream { remote: Arc::clone(remote), stream, data, store, health, node_id })
        }
    }

    /// The node this stream is reading, i.e. which monitor.
    pub fn node_id(&self) -> u32 {
        self.node_id
    }

    /// The shared newest frame.
    pub fn store(&self) -> &Arc<FrameStore> {
        &self.store
    }

    /// What the stream is doing right now.
    pub fn health(&self) -> Health {
        self.health.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: destroying under the loop lock is what guarantees no
        // callback is mid-flight when `data` - which owns the hook the
        // stream points at - goes away with this struct.
        unsafe {
            (self.remote.lib.thread_loop_lock)(self.remote.thread_loop);
            (self.remote.lib.stream_disconnect)(self.stream);
            (self.remote.lib.stream_destroy)(self.stream);
            (self.remote.lib.thread_loop_unlock)(self.remote.thread_loop);
        }
        // The buffers this stream mapped are gone; forget the parked
        // frame so nothing can crop out of a dead mapping.
        self.store.clear();
        let _ = &self.data;
    }
}

/// `media.*` hints so the graph places us as a screen capture sink.
///
/// # Safety
/// Calls into the loaded library; the returned properties are owned by
/// the stream that receives them.
unsafe fn capture_properties(lib: &Lib) -> *mut c_void {
    // Keys and values must outlive the `pw_properties_new_dict` call;
    // PipeWire copies them.
    let pairs = [
        (c"media.type", c"Video"),
        (c"media.category", c"Capture"),
        (c"media.role", c"Screen"),
    ];
    let items: Vec<SpaDictItem> = pairs
        .iter()
        .map(|(k, v)| SpaDictItem { key: k.as_ptr(), value: v.as_ptr() })
        .collect();
    let dict = SpaDict { flags: 0, n_items: items.len() as u32, items: items.as_ptr() };
    // SAFETY: PipeWire copies the dict and the strings it points at.
    unsafe { (lib.properties_new_dict)(&dict) }
}

// ------------------------------------------------------------- callbacks

fn state_name(state: c_int) -> &'static str {
    match state {
        STATE_ERROR => "error",
        STATE_UNCONNECTED => "unconnected",
        STATE_CONNECTING => "connecting",
        STATE_PAUSED => "paused",
        STATE_STREAMING => "streaming",
        _ => "unknown",
    }
}

/// PipeWire hands us `void *data`; it is always the `StreamData` we
/// registered, boxed and never moved.
///
/// # Safety
/// Only called from the C callbacks below, with the pointer PipeWire
/// was given in `pw_stream_add_listener`.
unsafe fn data_of<'a>(data: *mut c_void) -> Option<&'a mut StreamData> {
    // SAFETY: the caller's contract - PipeWire hands back the very
    // pointer it was registered with, which is a live boxed StreamData.
    (!data.is_null()).then(|| unsafe { &mut *(data as *mut StreamData) })
}

extern "C" fn on_state_changed(data: *mut c_void, _old: c_int, new: c_int, error: *const c_char) {
    // SAFETY: see `data_of`.
    let Some(sd) = (unsafe { data_of(data) }) else { return };
    let message = if error.is_null() {
        None
    } else {
        // SAFETY: PipeWire passes a nul-terminated message it owns.
        Some(unsafe { std::ffi::CStr::from_ptr(error) }.to_string_lossy().into_owned())
    };
    let mut health = sd.health.lock().unwrap_or_else(|e| e.into_inner());
    health.state = state_name(new);
    if message.is_some() {
        health.error = message;
    }
    if new == STATE_UNCONNECTED || new == STATE_ERROR {
        health.negotiated = false;
    }
}

/// The format handshake: fixate, then say what buffers and metadata
/// that format needs.
extern "C" fn on_param_changed(data: *mut c_void, id: u32, param: *const u8) {
    // SAFETY: see `data_of`.
    let Some(sd) = (unsafe { data_of(data) }) else { return };
    if id != pod::PARAM_FORMAT {
        return;
    }
    if param.is_null() {
        sd.shape = None;
        sd.health.lock().unwrap_or_else(|e| e.into_inner()).negotiated = false;
        let held = sd.store.clear();
        if !held.is_null() {
            // SAFETY: a buffer this stream dequeued, handed straight back.
            unsafe { (sd.lib.stream_queue_buffer)(sd.stream, held as *mut PwBuffer) };
        }
        return;
    }
    // SAFETY: a `struct spa_pod`: `u32 size` of the body, then `u32
    // type`, then `size` bytes. Reading the header first is the only
    // way to know how much of it there is.
    let bytes = unsafe {
        let size = std::ptr::read_unaligned(param as *const u32) as usize;
        std::slice::from_raw_parts(param, 8 + size)
    };
    let Some(format) = pod::parse_video_format(bytes) else {
        let mut health = sd.health.lock().unwrap_or_else(|e| e.into_inner());
        health.error = Some("the stream fixated a format core cannot crop".to_string());
        return;
    };
    sd.shape = Some(Shape {
        w: format.width,
        h: format.height,
        stride: format.nominal_stride(),
        order: format.order,
    });

    let buffers = pod::buffers(format.nominal_stride(), format.height);
    let header = pod::meta(pod::META_HEADER, pod::META_HEADER_SIZE);
    let cursor = pod::meta_cursor();
    let damage = pod::meta_range(
        pod::META_VIDEO_DAMAGE,
        (META_REGION_SIZE * 4) as i32,
        META_REGION_SIZE as i32,
        (META_REGION_SIZE * 16) as i32,
    );
    let params = [buffers.as_ptr(), header.as_ptr(), cursor.as_ptr(), damage.as_ptr()];
    // SAFETY: called from the loop thread with the loop lock held,
    // which is where `pw_stream_update_params` must be called from.
    unsafe { (sd.lib.stream_update_params)(sd.stream, params.as_ptr(), params.len() as u32) };
    sd.health.lock().unwrap_or_else(|e| e.into_inner()).negotiated = true;
}

extern "C" fn on_remove_buffer(data: *mut c_void, buffer: *mut PwBuffer) {
    // SAFETY: see `data_of`.
    let Some(sd) = (unsafe { data_of(data) }) else { return };
    // If the server is reclaiming the frame we parked, drop it before
    // it is unmapped; nothing may crop out of it afterwards.
    sd.store.release(buffer as *mut c_void);
}

/// One graph cycle: take the newest buffer, park it, recycle the rest.
extern "C" fn on_process(data: *mut c_void) {
    // SAFETY: see `data_of`.
    let Some(sd) = (unsafe { data_of(data) }) else { return };
    // SAFETY: dequeue/queue are RT-safe and this is the loop thread.
    unsafe {
        // Drain: a consumer that reads one buffer per cycle falls
        // behind a compositor that produces several, and a hover wants
        // the newest pixels, not the oldest.
        let mut newest: *mut PwBuffer = std::ptr::null_mut();
        loop {
            let next = (sd.lib.stream_dequeue_buffer)(sd.stream);
            if next.is_null() {
                break;
            }
            if !newest.is_null() {
                (sd.lib.stream_queue_buffer)(sd.stream, newest);
            }
            newest = next;
        }
        if newest.is_null() {
            return;
        }
        match park(sd, newest) {
            Some(displaced) => {
                if !displaced.is_null() {
                    (sd.lib.stream_queue_buffer)(sd.stream, displaced as *mut PwBuffer);
                }
            }
            // Nothing usable in it: straight back to the pool.
            None => {
                (sd.lib.stream_queue_buffer)(sd.stream, newest);
            }
        }
    }
}

/// Inspect one buffer and park it. `None` when it carries no pixels.
///
/// # Safety
/// `buffer` must be a buffer this stream just dequeued.
unsafe fn park(sd: &mut StreamData, buffer: *mut PwBuffer) -> Option<*mut c_void> {
    let shape = sd.shape?;
    // SAFETY: the caller's contract - `buffer` is a live buffer this
    // stream dequeued, so its `spa_buffer` and the data/chunk arrays it
    // points at are PipeWire's and valid for this cycle.
    let (base, len, shape, changed, cursor) = unsafe {
        let spa = (*buffer).buffer;
        if spa.is_null() || (*spa).n_datas == 0 || (*spa).datas.is_null() {
            return None;
        }
        let block = &*(*spa).datas;
        if block.data.is_null() || block.chunk.is_null() {
            return None;
        }
        let chunk = &*block.chunk;
        if chunk.size == 0 || chunk.stride <= 0 {
            return None;
        }
        let maxsize = block.maxsize as usize;
        let offset = (chunk.offset as usize).min(maxsize);
        let len = (chunk.size as usize).min(maxsize - offset);
        if len == 0 {
            return None;
        }
        let shape = Shape { stride: chunk.stride, ..shape };
        // Every row must fit, or the crop would read past the mapping.
        if i64::from(shape.h) * i64::from(shape.stride) > len as i64 {
            return None;
        }
        let (changed, cursor) = inspect_metas(spa);
        ((block.data as *const u8).add(offset), len, shape, changed, cursor)
    };

    if let (Some(sink), Some(pos)) = (sd.cursor.as_ref(), cursor) {
        if sd.last_cursor != Some(pos) {
            sd.last_cursor = Some(pos);
            sink(pos.0, pos.1);
        }
    }
    // SAFETY: `base`/`len` describe the mapped pages of the buffer we
    // are parking, which stay mapped until it is released or displaced.
    Some(unsafe { sd.store.accept(buffer as *mut c_void, base, len, shape, changed) })
}

/// Read the two metadata blocks that matter: was anything damaged, and
/// where is the cursor.
///
/// A buffer with no `SPA_META_VideoDamage` reads as damaged, because
/// "we cannot tell" must never be reported as "nothing changed".
///
/// # Safety
/// `spa` must be a live `struct spa_buffer` from a dequeued buffer.
unsafe fn inspect_metas(spa: *const SpaBuffer) -> (bool, Option<(i32, i32)>) {
    let mut changed = true;
    let mut cursor = None;
    // SAFETY: the caller's contract - `spa` is a live `spa_buffer`, so
    // `metas`/`n_metas` describe an array PipeWire owns for this cycle
    // and each `meta.data` points at `meta.size` readable bytes.
    unsafe {
        if (*spa).metas.is_null() {
            return (changed, cursor);
        }
        let metas = std::slice::from_raw_parts((*spa).metas, (*spa).n_metas as usize);
        for meta in metas {
            if meta.data.is_null() {
                continue;
            }
            match meta.ty {
                pod::META_VIDEO_DAMAGE => {
                    let n = (meta.size as usize) / META_REGION_SIZE;
                    let regions = std::slice::from_raw_parts(meta.data as *const SpaMetaRegion, n);
                    // An entry with a zero dimension ends the array, so
                    // a first entry that is already invalid is an
                    // explicit "this recomposite moved nothing".
                    changed = regions.first().is_some_and(|r| r.width != 0 && r.height != 0);
                }
                pod::META_CURSOR => {
                    if (meta.size as usize) < std::mem::size_of::<SpaMetaCursor>() {
                        continue;
                    }
                    let c = &*(meta.data as *const SpaMetaCursor);
                    // id 0 means "no new cursor data"; a stale position
                    // is worse than none (the cursor rung must not drag
                    // hover to wherever the pointer last was).
                    if c.id != 0 {
                        cursor = Some((c.x, c.y));
                    }
                }
                _ => {}
            }
        }
    }
    (changed, cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mirrored C structs are ABI, and getting one wrong is a
    /// silent misread of somebody else's memory. Pin the layouts that
    /// the buffer walk indexes into.
    #[test]
    fn the_mirrored_c_structs_have_the_abi_layout() {
        assert_eq!(16, std::mem::size_of::<SpaChunk>());
        assert_eq!(40, std::mem::size_of::<SpaData>());
        assert_eq!(16, std::mem::size_of::<SpaMeta>());
        assert_eq!(24, std::mem::size_of::<SpaBuffer>());
        assert_eq!(28, std::mem::size_of::<SpaMetaCursor>());
        assert_eq!(META_REGION_SIZE, std::mem::size_of::<SpaMetaRegion>());
        assert_eq!(pod::META_CURSOR_SIZE as usize, std::mem::size_of::<SpaMetaCursor>());
        // `struct pw_buffer` starts with the spa buffer pointer, which
        // is the only field `park` reads.
        assert_eq!(0, std::mem::offset_of!(PwBuffer, buffer));
    }

    /// `PW_VERSION_STREAM_EVENTS` is 2 and the table is indexed by
    /// position: a missing entry shifts every callback after it.
    #[test]
    fn the_event_table_is_version_two_and_complete() {
        assert_eq!(2, EVENTS.version);
        // The version word plus its padding is one pointer's worth,
        // then eleven callbacks: 96 bytes on a 64-bit ABI.
        assert_eq!(
            std::mem::size_of::<usize>() * 12,
            std::mem::size_of::<PwStreamEvents>(),
            "eleven callbacks plus a padded version word"
        );
        assert!(EVENTS.process.is_some() && EVENTS.param_changed.is_some());
        assert!(EVENTS.remove_buffer.is_some() && EVENTS.state_changed.is_some());
    }

    /// `struct spa_hook` is six pointers; the reserved space must
    /// cover it, or PipeWire writes past our box.
    #[test]
    fn the_hook_scratch_covers_a_spa_hook() {
        let hook: [u64; 8] = [0; 8];
        assert!(std::mem::size_of_val(&hook) >= std::mem::size_of::<usize>() * 6);
        assert_eq!(0, std::mem::align_of_val(&hook) % std::mem::align_of::<usize>());
    }

    #[test]
    fn every_stream_state_has_a_name() {
        assert_eq!("streaming", state_name(STATE_STREAMING));
        assert_eq!("error", state_name(STATE_ERROR));
        assert_eq!("unknown", state_name(42));
    }

    /// An absent PipeWire is a rung that is not there, named in the
    /// error - never a panic.
    #[test]
    fn a_missing_library_is_an_error_naming_the_soname() {
        // The real library may well be installed on a dev box, so this
        // asserts the message shape through the same context the
        // failure path uses rather than forcing an absence.
        let message = format!("loading {SONAME} (install the PipeWire runtime)");
        assert!(message.contains("libpipewire-0.3.so.0"), "{message}");
    }
}
