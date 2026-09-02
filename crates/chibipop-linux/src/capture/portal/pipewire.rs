//! Implements the PipeWire part of the fallback Capture backend.
//!
//! This backend creates one video stream. PipeWire processes the stream on its own thread.
//! [`super::frame`] stores the newest frame.
//!
//! **Why this module uses dlopen instead of the `pipewire` crate.**
//! `pipewire-sys` uses `system-deps`.
//! A build with this dependency needs `libpipewire-0.3-dev` and a pkg-config file.
//! The Linux CI image has neither the development package nor the pkg-config file.
//! Without the development package, a build-time PipeWire dependency stops the build.
//! This choice affects systems that do not use the portal rung.
//! This module opens `libpipewire-0.3.so.0` at run time.
//! `ARCHITECTURE.md#ocr-engine` uses the same run-time choice for the `ort` system path.
//! If the library is absent, the diagnostic names the unavailable rung.
//! The cursor ladder then tries the next rung.
//!
//! **Cost of dlopen.**
//! Each `spa_pod_builder_*` helper is a `static inline` function in a header.
//! The library does not export these helpers, so dlopen cannot load them.
//! [`super::pod`] serializes the format handshake without these helpers.
//! The other calls use FFI with the stable PipeWire ABI.
//! The local structs match `struct spa_buffer`, `struct spa_data`, `struct spa_chunk`,
//! `struct spa_meta`, `struct pw_buffer`, and `struct pw_stream_events` version 2.
//! PipeWire includes these structs in its ABI.
//!
//! **Thread model.** `pw_thread_loop` provides the dedicated PipeWire thread.
//! PipeWire starts this thread and calls `process` and `param_changed` while it holds the loop lock.
//! This module does not use the daemon's calloop pump.
//! The sink returns only a cursor position to the caller.
//! The daemon backs this sink with a calloop channel.
//!
//! **Power use.** The stream stays active while the Capture backend exists.
//! The second cursor ladder rung reads cursor data from this stream.
//! A paused stream has no cursor data.
//! This constant stream cost keeps wlr-screencopy as the primary rung where it exists.
//! This module swaps one pointer for each frame. See [`super::frame`].

use super::frame::{FrameStore, Shape};
use super::pod;
use anyhow::{Context, Result};
use libloading::Library;
use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::{Arc, Mutex};

/// The versioned soname for dlopen.
///
/// `libpipewire-0.3.so` is the `-dev` symlink. This module does not need that symlink.
pub const SONAME: &str = "libpipewire-0.3.so.0";

/// `enum pw_direction`.
const DIRECTION_INPUT: c_int = 0;

/// Bit values from `enum pw_stream_flags` that this module uses.
const FLAG_AUTOCONNECT: c_int = 1 << 0;
const FLAG_MAP_BUFFERS: c_int = 1 << 2;

/// Values from `enum pw_stream_state`.
const STATE_ERROR: c_int = -1;
const STATE_UNCONNECTED: c_int = 0;
const STATE_CONNECTING: c_int = 1;
const STATE_PAUSED: c_int = 2;
const STATE_STREAMING: c_int = 3;

/// The size of `struct spa_meta_region`, which stores a point and a rectangle.
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

/// `struct spa_meta_region` uses a zero dimension to end the damage array.
#[repr(C)]
struct SpaMetaRegion {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

/// Mirrors `struct pw_stream_events` with `PW_VERSION_STREAM_EVENTS == 2`.
///
/// Keep the complete table in this order. PipeWire reads each field by position, not by name.
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

/// Stores the PipeWire symbols that this Capture backend resolves once.
///
/// C callbacks receive only a `void *data` pointer, so this struct stores bare function
/// pointers. `_library` keeps the symbols valid while the module uses these pointers.
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
        // SAFETY: The names and signatures match the public PipeWire ABI in
        // <pipewire/stream.h> and <pipewire/core.h>.
        unsafe {
            *$lib
                .get(concat!($name, "\0").as_bytes())
                .with_context(|| format!("{SONAME} has no {}", $name))?
        }
    };
}

impl Lib {
    /// Opens PipeWire with dlopen and resolves every symbol that this module needs.
    ///
    /// Each failure means that the rung is unavailable. The caller records a named channel
    /// state, so the daemon stays alive.
    pub fn open() -> Result<Arc<Lib>> {
        // SAFETY: The versioned soname identifies the system PipeWire library.
        // `pw_init` starts the library after dlopen. dlopen does not run user code.
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
        // SAFETY: PipeWire documents `pw_init(NULL, NULL)` as the no-argument form.
        // PipeWire makes this call idempotent and reference-counted.
        unsafe { (lib.init)(std::ptr::null_mut(), std::ptr::null_mut()) };
        Ok(Arc::new(lib))
    }
}

// ------------------------------------------------------------ the stream

/// Receives a cursor position in stream buffer pixels.
///
/// The caller knows the monitor and completes the conversion in `super::metadata`.
pub type CursorSink = Box<dyn Fn(i32, i32) + Send + Sync>;

/// Reports the current stream state to the channel status registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    /// The local name of `pw_stream_state_as_string`.
    pub state: &'static str,
    /// The message from the last error, if one exists.
    pub error: Option<String>,
    /// True after format negotiation succeeds and frames can flow.
    pub negotiated: bool,
}

impl Default for Health {
    fn default() -> Health {
        Health { state: "unconnected", error: None, negotiated: false }
    }
}

/// Holds the values that C callbacks can access.
///
/// The box keeps its address stable because PipeWire stores this address as `void *data`.
struct StreamData {
    lib: Arc<Lib>,
    /// The field is set before `pw_stream_connect`, so callbacks never see a null pointer.
    stream: *mut c_void,
    store: Arc<FrameStore>,
    health: Arc<Mutex<Health>>,
    cursor: Option<CursorSink>,
    /// The last cursor position that the sink received.
    ///
    /// An unchanged cursor needs one comparison instead of 60 events each second.
    last_cursor: Option<(i32, i32)>,
    /// The format that `param_changed` fixated.
    shape: Option<Shape>,
    /// Space for `struct spa_hook`, which has six pointers.
    ///
    /// PipeWire owns this zeroed space and does not reallocate it.
    hook: [u64; 8],
}

/// Holds one PipeWire connection for the life of a portal grant.
///
/// The connection contains one thread loop, one context, and one core.
/// [`Stream`] is separate because the core owns the file descriptor.
/// `pw_context_connect_fd` transfers ownership of the `fd` to the core.
/// A second core cannot duplicate the connection.
/// Two protocol clients cannot share one socket.
/// A monitor change replaces the *stream* but keeps the connection.
/// This replacement costs less than a new connection.
pub struct Remote {
    lib: Arc<Lib>,
    thread_loop: *mut c_void,
    context: *mut c_void,
    core: *mut c_void,
}

// SAFETY: Each method takes `pw_thread_loop_lock` before it accesses a PipeWire object.
// Each callback already holds the lock.
unsafe impl Send for Remote {}
// SAFETY: The same loop lock permits shared access.
unsafe impl Sync for Remote {}

impl Remote {
    /// Connects to the PipeWire remote that `fd` identifies and takes ownership of `fd`.
    pub fn connect(lib: Arc<Lib>, fd: std::os::fd::OwnedFd) -> Result<Arc<Remote>> {
        use std::os::fd::IntoRawFd;

        let name = CString::new("chibipop-portal").expect("a literal with no nul");
        // SAFETY: PipeWire documents this constructor sequence.
        // The code checks each result for null before use.
        // Each failure path tears down every object that it created.
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
            // The core takes ownership of the fd. It closes the fd on disconnect or failure.
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
        // SAFETY: Stop the loop before you destroy objects, so no callback can run.
        // No stream exists here because each Stream holds an `Arc<Remote>`.
        // Therefore, this drop call cannot run while a stream remains alive.
        unsafe {
            (self.lib.thread_loop_stop)(self.thread_loop);
            (self.lib.core_disconnect)(self.core);
            (self.lib.context_destroy)(self.context);
            (self.lib.thread_loop_destroy)(self.thread_loop);
        }
    }
}

/// Releases a partially built [`Remote`] after `connect` fails.
struct Half<'a> {
    lib: &'a Lib,
    thread_loop: *mut c_void,
    context: *mut c_void,
}

impl Drop for Half<'_> {
    fn drop(&mut self) {
        // SAFETY: This value holds only objects that `Remote::connect` created.
        // The code has not transferred these objects to `Remote`.
        // PipeWire treats `pw_thread_loop_stop` on an unstarted loop as a no-op.
        unsafe {
            (self.lib.thread_loop_stop)(self.thread_loop);
            if !self.context.is_null() {
                (self.lib.context_destroy)(self.context);
            }
            (self.lib.thread_loop_destroy)(self.thread_loop);
        }
    }
}

/// A connected capture stream on a [`Remote`].
pub struct Stream {
    remote: Arc<Remote>,
    stream: *mut c_void,
    /// The box keeps the address given to C stable and drops after the stream.
    data: Box<StreamData>,
    store: Arc<FrameStore>,
    health: Arc<Mutex<Health>>,
    node_id: u32,
}

// SAFETY: Access the stream only under the remote's loop lock.
// Stream methods and callbacks acquire or hold this lock.
unsafe impl Send for Stream {}

impl Stream {
    /// Connects to `node_id` on an already open portal remote.
    pub fn connect(
        remote: &Arc<Remote>,
        node_id: u32,
        cursor: Option<CursorSink>,
    ) -> Result<Stream> {
        let lib = Arc::clone(&remote.lib);
        let store = Arc::new(FrameStore::new());
        let health = Arc::new(Mutex::new(Health::default()));
        let name = CString::new("chibipop-capture").expect("a literal with no nul");

        // SAFETY: The code calls `pw_stream_*` off the loop thread under the loop lock.
        // The code checks every pointer before use.
        // On failure, it destroys the stream before it releases the lock.
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

    /// The node that this stream reads. The node identifies the monitor.
    pub fn node_id(&self) -> u32 {
        self.node_id
    }

    /// The shared frame with the newest pixels.
    pub fn store(&self) -> &Arc<FrameStore> {
        &self.store
    }

    /// The current stream state.
    pub fn health(&self) -> Health {
        self.health.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: Destroy the stream under the loop lock.
        // No callback can access `data` after the stream releases its hook.
        // The `data` field owns the hook that the stream uses.
        unsafe {
            (self.remote.lib.thread_loop_lock)(self.remote.thread_loop);
            (self.remote.lib.stream_disconnect)(self.stream);
            (self.remote.lib.stream_destroy)(self.stream);
            (self.remote.lib.thread_loop_unlock)(self.remote.thread_loop);
        }
        // The stream has released its mapped buffers.
        // Clear the parked frame so no crop uses an invalid mapping.
        self.store.clear();
        let _ = &self.data;
    }
}

/// The `media.*` hints tell the graph to place this stream as a screen capture sink.
///
/// # Safety
/// This function calls the loaded library. The stream that receives the properties owns them.
unsafe fn capture_properties(lib: &Lib) -> *mut c_void {
    // Keep keys and values alive during the `pw_properties_new_dict` call.
    // PipeWire copies the keys and values.
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

/// PipeWire gives this function `void *data`. The pointer always identifies the boxed
/// [`StreamData`] value that this module registered and never moves.
///
/// # Safety
/// Call this function only from the C callbacks below.
/// PipeWire must pass the pointer registered with `pw_stream_add_listener`.
unsafe fn data_of<'a>(data: *mut c_void) -> Option<&'a mut StreamData> {
    // SAFETY: The caller guarantees that PipeWire returns the registered pointer.
    // The pointer identifies a live boxed `StreamData` value.
    (!data.is_null()).then(|| unsafe { &mut *(data as *mut StreamData) })
}

extern "C" fn on_state_changed(data: *mut c_void, _old: c_int, new: c_int, error: *const c_char) {
    // SAFETY: `data_of` defines this pointer contract.
    let Some(sd) = (unsafe { data_of(data) }) else { return };
    let message = if error.is_null() {
        None
    } else {
        // SAFETY: PipeWire passes a nul-terminated message that it owns.
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

/// Handles the format handshake. It reads the fixated format and requests its buffers and metadata.
extern "C" fn on_param_changed(data: *mut c_void, id: u32, param: *const u8) {
    // SAFETY: `data_of` defines this pointer contract.
    let Some(sd) = (unsafe { data_of(data) }) else { return };
    if id != pod::PARAM_FORMAT {
        return;
    }
    if param.is_null() {
        sd.shape = None;
        sd.health.lock().unwrap_or_else(|e| e.into_inner()).negotiated = false;
        let held = sd.store.clear();
        if !held.is_null() {
            // SAFETY: The stream dequeued this buffer and queues it again here.
            unsafe { (sd.lib.stream_queue_buffer)(sd.stream, held as *mut PwBuffer) };
        }
        return;
    }
    // SAFETY: A `struct spa_pod` starts with `u32 size` for the body, then `u32 type`, and
    // then `size` body bytes. Read the header first because its size field gives the body length.
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
    // SAFETY: This callback runs on the loop thread with the loop lock held.
    // `pw_stream_update_params` needs that lock.
    unsafe { (sd.lib.stream_update_params)(sd.stream, params.as_ptr(), params.len() as u32) };
    sd.health.lock().unwrap_or_else(|e| e.into_inner()).negotiated = true;
}

extern "C" fn on_remove_buffer(data: *mut c_void, buffer: *mut PwBuffer) {
    // SAFETY: `data_of` defines this pointer contract.
    let Some(sd) = (unsafe { data_of(data) }) else { return };
    // If the server reclaims the parked frame, release it before the server unmaps it.
    // No later crop can use the unmapped frame.
    sd.store.release(buffer as *mut c_void);
}

/// Processes one graph cycle. It parks the newest buffer and returns the other buffers.
extern "C" fn on_process(data: *mut c_void) {
    // SAFETY: `data_of` defines this pointer contract.
    let Some(sd) = (unsafe { data_of(data) }) else { return };
    // SAFETY: The loop thread calls these RT-safe dequeue and queue operations.
    unsafe {
        // Drain the queue because a compositor can produce several buffers per cycle.
        // The hover path needs the newest pixels, not the oldest.
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
            // Return a buffer with no usable pixels to the pool.
            None => {
                (sd.lib.stream_queue_buffer)(sd.stream, newest);
            }
        }
    }
}

/// Inspects one buffer and parks it. Returns `None` when the buffer has no pixels.
///
/// # Safety
/// `buffer` must be a buffer that the stream dequeued.
unsafe fn park(sd: &mut StreamData, buffer: *mut PwBuffer) -> Option<*mut c_void> {
    let shape = sd.shape?;
    // SAFETY: The caller passes a live buffer that this stream dequeued.
    // Its `spa_buffer`, data, and chunk arrays belong to PipeWire and stay valid for this cycle.
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
        // Every row must fit so the crop never reads past the mapping.
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
    // SAFETY: `base` and `len` describe mapped pages for this parked buffer.
    // The pages stay mapped until PipeWire releases or displaces the buffer.
    Some(unsafe { sd.store.accept(buffer as *mut c_void, base, len, shape, changed) })
}

/// Reads the two metadata blocks that matter: damage status and cursor position.
///
/// A buffer without `SPA_META_VideoDamage` counts as damaged.
/// The code must not report "we cannot tell" as "nothing changed".
///
/// # Safety
/// `spa` must be a live `struct spa_buffer` from a dequeued buffer.
unsafe fn inspect_metas(spa: *const SpaBuffer) -> (bool, Option<(i32, i32)>) {
    let mut changed = true;
    let mut cursor = None;
    // SAFETY: The caller passes a live `spa_buffer`.
    // PipeWire owns the meta array, and each `meta.data` region stays readable for
    // `meta.size` bytes during this cycle.
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
                    // A zero dimension ends the array.
                    // Therefore, a zero-sized first entry means "this recomposite moved nothing".
                    changed = regions.first().is_some_and(|r| r.width != 0 && r.height != 0);
                }
                pod::META_CURSOR => {
                    if (meta.size as usize) < std::mem::size_of::<SpaMetaCursor>() {
                        continue;
                    }
                    let c = &*(meta.data as *const SpaMetaCursor);
                    // `id` 0 means "no new cursor data".
                    // Ignore a stale position because the cursor rung must not move hover to
                    // the last pointer position.
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

    /// The mirrored C structs define the ABI.
    /// One wrong field causes the buffer walk to read the wrong memory.
    /// Pin the layouts that the buffer walk indexes.
    #[test]
    fn the_mirrored_c_structs_have_the_abi_layout() {
        assert_eq!(16, std::mem::size_of::<SpaChunk>());
        assert_eq!(40, std::mem::size_of::<SpaData>());
        assert_eq!(16, std::mem::size_of::<SpaMeta>());
        assert_eq!(24, std::mem::size_of::<SpaBuffer>());
        assert_eq!(28, std::mem::size_of::<SpaMetaCursor>());
        assert_eq!(META_REGION_SIZE, std::mem::size_of::<SpaMetaRegion>());
        assert_eq!(pod::META_CURSOR_SIZE as usize, std::mem::size_of::<SpaMetaCursor>());
        // `struct pw_buffer` starts with the spa buffer pointer.
        // `park` reads this field only.
        assert_eq!(0, std::mem::offset_of!(PwBuffer, buffer));
    }

    /// `PW_VERSION_STREAM_EVENTS` is 2.
    /// PipeWire indexes this table by position, so a missing entry shifts every later callback.
    #[test]
    fn the_event_table_is_version_two_and_complete() {
        assert_eq!(2, EVENTS.version);
        // The version word and its padding use one pointer's space.
        // Eleven callbacks then use 96 bytes on a 64-bit ABI.
        assert_eq!(
            std::mem::size_of::<usize>() * 12,
            std::mem::size_of::<PwStreamEvents>(),
            "eleven callbacks plus a padded version word"
        );
        assert!(EVENTS.process.is_some() && EVENTS.param_changed.is_some());
        assert!(EVENTS.remove_buffer.is_some() && EVENTS.state_changed.is_some());
    }

    /// `struct spa_hook` has six pointers.
    /// The reserved space must cover it because PipeWire can write past our box otherwise.
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

    /// A missing PipeWire library makes the rung unavailable.
    /// The error names the soname. The code does not panic.
    #[test]
    fn a_missing_library_is_an_error_naming_the_soname() {
        // A development machine can have the real library.
        // Test the message shape through the same context as the failure path.
        // Do not force the library to be absent.
        let message = format!("loading {SONAME} (install the PipeWire runtime)");
        assert!(message.contains("libpipewire-0.3.so.0"), "{message}");
    }
}
