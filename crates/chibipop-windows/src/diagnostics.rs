use anyhow::{anyhow, Context, Result};
use std::str;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{
    CloseHandle, DuplicateHandle, HANDLE, DUPLICATE_SAME_ACCESS,
};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::Console::{
    GetConsoleMode, GetStdHandle, SetStdHandle, WriteConsoleW, STD_ERROR_HANDLE,
    STD_HANDLE, STD_OUTPUT_HANDLE,
};
use windows::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
use windows::Win32::System::Threading::GetCurrentProcess;

const LOG_CAPACITY: usize = 256 * 1024;
const READ_CAPACITY: usize = 8 * 1024;
const POLL_DELAY: Duration = Duration::from_millis(10);
const STOP_TIMEOUT: Duration = Duration::from_millis(200);

static CAPTURE: OnceLock<Mutex<Option<CaptureState>>> = OnceLock::new();
static LOGS: OnceLock<LogStore> = OnceLock::new();
static NEXT_CAPTURE_ID: AtomicU64 = AtomicU64::new(1);

/// A recent log snapshot.
pub(crate) struct LogSnapshot {
    pub(crate) revision: u64,
    pub(crate) text: String,
}

/// Owns active log capture.
pub struct CaptureGuard {
    active: bool,
    id: u64,
}

struct CaptureState {
    id: u64,
    stdout: PreparedStream,
    stderr: PreparedStream,
}

/// Starts live log capture.
pub fn start_capture() -> Result<CaptureGuard> {
    let result = start_capture_inner();
    if let Err(error) = &result {
        append_log(&format!("chibipop: live log capture unavailable: {error:#}\n"));
    }
    result
}

fn start_capture_inner() -> Result<CaptureGuard> {
    let mut active = capture_state()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if active.is_some() {
        return Err(anyhow!("diagnostic capture is already active"));
    }
    let id = NEXT_CAPTURE_ID.fetch_add(1, Ordering::Relaxed);
    let mut stdout = PreparedStream::new(STD_OUTPUT_HANDLE, "stdout")?;
    let mut stderr = PreparedStream::new(STD_ERROR_HANDLE, "stderr")?;

    set_standard_handle(stdout.kind, stdout.writer_handle())
        .context("redirecting standard output")?;
    stdout.redirected = true;
    if let Err(error) = set_standard_handle(stderr.kind, stderr.writer_handle()) {
        let mut state = CaptureState { id, stdout, stderr };
        state.shutdown_before(Instant::now() + STOP_TIMEOUT);
        if state.is_redirected() {
            *active = Some(state);
        }
        return Err(error).context("redirecting standard error");
    }

    stderr.redirected = true;
    *active = Some(CaptureState {
        id,
        stdout,
        stderr,
    });
    Ok(CaptureGuard { active: true, id })
}

/// Stops capture for restart.
/// Recent logs remain readable.
/// False retains stream owners.
pub fn shutdown_capture() -> bool {
    shutdown_matching(None)
}

/// Copies the recent log.
pub(crate) fn snapshot() -> LogSnapshot {
    logs().snapshot()
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        if self.active {
            shutdown_matching(Some(self.id));
            self.active = false;
        }
    }
}

impl CaptureState {
    fn shutdown_before(&mut self, deadline: Instant) {
        self.stdout.stop_before(deadline);
        self.stderr.stop_before(deadline);
        if !self.stdout.redirected {
            self.stdout.finish_before(deadline);
        }
        if !self.stderr.redirected {
            self.stderr.finish_before(deadline);
        }
    }

    fn is_redirected(&self) -> bool {
        self.stdout.redirected || self.stderr.redirected
    }
}

struct PreparedStream {
    kind: STD_HANDLE,
    original: isize,
    writer: Option<OwnedHandle>,
    worker: Option<JoinHandle<()>>,
    done: mpsc::Receiver<()>,
    redirected: bool,
    stop: Arc<Mutex<Option<Instant>>>,
}

impl PreparedStream {
    fn new(kind: STD_HANDLE, name: &str) -> Result<Self> {
        let original = get_standard_handle(kind)?;
        let sink = ForwardSink::new(duplicate_handle(original)?);
        let (reader, writer) = create_pipe()?;
        let (done_tx, done) = mpsc::channel();
        let stop = Arc::new(Mutex::new(None));
        let worker_stop = Arc::clone(&stop);
        let thread_name = format!("chibipop-log-{name}");
        let worker = thread::Builder::new()
            .name(thread_name)
            .spawn(move || reader_loop(reader, sink, worker_stop, done_tx, logs()))
            .context("starting a diagnostic reader")?;
        Ok(Self {
            kind,
            original: handle_raw(original),
            writer: Some(writer),
            worker: Some(worker),
            done,
            redirected: false,
            stop,
        })
    }

    fn writer_handle(&self) -> HANDLE {
        self.writer.as_ref().expect("writer exists").0
    }

    fn restore(&mut self) -> bool {
        if self.redirected {
            if let Err(error) = set_standard_handle(self.kind, handle_from_raw(self.original)) {
                append_log(&format!("chibipop: restoring {:?} failed; capture retained: {error:#}\n",
                    self.kind));
                return false;
            }
            self.redirected = false;
        }
        true
    }

    fn stop_before(&mut self, deadline: Instant) {
        if self.restore() {
            *self.stop.lock().unwrap_or_else(|error| error.into_inner()) = Some(deadline);
            self.writer.take();
        }
    }

    fn finish_before(&mut self, deadline: Instant) {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            self.worker.take();
            return;
        };
        if self.worker.is_none() {
            return;
        }
        if self.done.recv_timeout(remaining).is_err() {
            append_log("chibipop: log shutdown deadline reached; forwarding may be incomplete\n");
        }
        self.worker.take();
    }
}

impl Drop for PreparedStream {
    fn drop(&mut self) {
        if self.redirected && !self.restore() {
            if let Some(writer) = self.writer.take() {
                std::mem::forget(writer);
            }
        }
    }
}

struct OwnedHandle(HANDLE);

// SAFETY: Kernel handles can move between threads. This type closes its sole
// owned handle once, and callers cannot access the inner handle after drop.
unsafe impl Send for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: This value owns a valid kernel handle. No other owner closes it.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

enum ForwardSink {
    Console(OwnedHandle),
    Raw(OwnedHandle),
}

impl ForwardSink {
    fn new(handle: OwnedHandle) -> Self {
        let mut mode = Default::default();
        // SAFETY: `handle` remains valid for this probe. `mode` is writable storage.
        if unsafe { GetConsoleMode(handle.0, &mut mode) }.is_ok() {
            Self::Console(handle)
        } else {
            Self::Raw(handle)
        }
    }

    fn forward(&self, bytes: &[u8], text: &str) -> Result<()> {
        match self {
            Self::Raw(handle) => write_file_all(handle.0, bytes),
            Self::Console(handle) => write_console_all(handle.0, text),
        }
    }
}

fn reader_loop(
    reader: OwnedHandle,
    sink: ForwardSink,
    stop: Arc<Mutex<Option<Instant>>>,
    done: mpsc::Sender<()>,
    log: &LogStore,
) {
    let mut decoder = Utf8Decoder::default();
    let mut buffer = [0u8; READ_CAPACITY];
    let mut forwarding = true;
    loop {
        let deadline = *stop.lock().unwrap_or_else(|error| error.into_inner());
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            log.push("chibipop: log drain deadline reached; queued output may be incomplete\n");
            break;
        }
        let mut available = 0u32;
        // SAFETY: `reader` is valid. The call only writes `available`.
        let peeked = unsafe {
            PeekNamedPipe(reader.0, None, 0, None, Some(&mut available), None)
        };
        if let Err(error) = peeked {
            if error.code() != windows::Win32::Foundation::ERROR_BROKEN_PIPE.to_hresult() {
                log.push(&format!("chibipop: reading live logs failed: {error}\n"));
            }
            break;
        }
        if available == 0 {
            if deadline.is_some() {
                break;
            }
            thread::sleep(POLL_DELAY);
            continue;
        }

        let requested = usize::try_from(available)
            .unwrap_or(READ_CAPACITY)
            .min(READ_CAPACITY);
        let mut read = 0u32;
        // SAFETY: `buffer` has `requested` writable bytes. `read` is writable storage.
        let result = unsafe {
            ReadFile(
                reader.0,
                Some(&mut buffer[..requested]),
                Some(&mut read),
                None,
            )
        };
        if let Err(error) = result {
            log.push(&format!("chibipop: reading live logs failed: {error}\n"));
            break;
        }
        if read == 0 {
            break;
        }
        let count = usize::try_from(read).unwrap_or(requested).min(requested);
        let bytes = &buffer[..count];
        let decoded = decoder.push(bytes);
        if !decoded.is_empty() {
            log.push(&decoded);
        }
        if forwarding {
            if let Err(error) = sink.forward(bytes, &decoded) {
                log.push(&format!("chibipop: forwarding live logs failed: {error:#}\n"));
                forwarding = false;
            }
        }
    }

    let tail = decoder.finish();
    if !tail.is_empty() {
        log.push(&tail);
        if forwarding {
            if let Err(error) = sink.forward(&[], &tail) {
                log.push(&format!("chibipop: forwarding live logs failed: {error:#}\n"));
            }
        }
    }
    let _ = done.send(());
}

fn get_standard_handle(kind: STD_HANDLE) -> Result<HANDLE> {
    // SAFETY: `kind` is one of the two standard output handle identifiers.
    let handle = unsafe { GetStdHandle(kind) }.context("reading a standard handle")?;
    if handle.is_invalid() {
        Err(anyhow!("the standard handle is invalid"))
    } else {
        Ok(handle)
    }
}

fn set_standard_handle(kind: STD_HANDLE, handle: HANDLE) -> Result<()> {
    #[cfg(test)]
    if FAIL_STD_SET.with(|failure| failure.get() == Some(kind)) {
        return Err(anyhow!("injected standard-handle restoration failure"));
    }
    // SAFETY: `handle` stays valid while installed in the process handle table.
    unsafe { SetStdHandle(kind, handle) }.context("setting a standard handle")
}

fn duplicate_handle(handle: HANDLE) -> Result<OwnedHandle> {
    let mut duplicate = HANDLE::default();
    // SAFETY: The current process owns `handle`. `duplicate` is writable storage.
    unsafe {
        let process = GetCurrentProcess();
        DuplicateHandle(
            process,
            handle,
            process,
            &mut duplicate,
            0,
            false,
            DUPLICATE_SAME_ACCESS,
        )
    }
    .context("duplicating a standard handle")?;
    Ok(OwnedHandle(duplicate))
}

fn create_pipe() -> Result<(OwnedHandle, OwnedHandle)> {
    create_pipe_with_capacity(0)
}

fn create_pipe_with_capacity(capacity: u32) -> Result<(OwnedHandle, OwnedHandle)> {
    let mut reader = HANDLE::default();
    let mut writer = HANDLE::default();
    // SAFETY: Both handle outputs are writable. Null attributes disable inheritance.
    unsafe { CreatePipe(&mut reader, &mut writer, None, capacity) }
        .context("creating a diagnostic pipe")?;
    Ok((OwnedHandle(reader), OwnedHandle(writer)))
}

fn write_file_all(handle: HANDLE, mut bytes: &[u8]) -> Result<()> {
    while !bytes.is_empty() {
        let mut written = 0u32;
        // SAFETY: `handle` stays valid. `bytes` is readable, and `written` is writable.
        unsafe { WriteFile(handle, Some(bytes), Some(&mut written), None) }
            .context("writing the original stream")?;
        if written == 0 {
            return Err(anyhow!("the original stream wrote zero bytes"));
        }
        let count = usize::try_from(written).unwrap_or(bytes.len()).min(bytes.len());
        bytes = &bytes[count..];
    }
    Ok(())
}

fn write_console_all(handle: HANDLE, text: &str) -> Result<()> {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let mut remaining = wide.as_slice();
    while !remaining.is_empty() {
        let mut written = 0u32;
        // SAFETY: `handle` is a console. `remaining` is readable UTF-16 storage.
        unsafe { WriteConsoleW(handle, remaining, Some(&mut written), None) }
            .context("writing the original console")?;
        if written == 0 {
            return Err(anyhow!("the original console wrote zero characters"));
        }
        let count = usize::try_from(written)
            .unwrap_or(remaining.len())
            .min(remaining.len());
        remaining = &remaining[count..];
    }
    Ok(())
}

#[derive(Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut output = String::new();
        loop {
            match str::from_utf8(&self.pending) {
                Ok(valid) => {
                    output.push_str(valid);
                    self.pending.clear();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    output.push_str(str::from_utf8(&self.pending[..valid]).expect("valid prefix"));
                    if let Some(invalid) = error.error_len() {
                        output.push('\u{fffd}');
                        self.pending.drain(..valid.saturating_add(invalid));
                    } else {
                        self.pending.drain(..valid);
                        break;
                    }
                }
            }
        }
        output
    }

    fn finish(mut self) -> String {
        if self.pending.is_empty() {
            String::new()
        } else {
            self.pending.clear();
            "\u{fffd}".into()
        }
    }
}

struct LogStore {
    buffer: Mutex<LogBuffer>,
    revision: AtomicU64,
}

impl LogStore {
    fn new(capacity: usize) -> Self {
        Self {
            buffer: Mutex::new(LogBuffer::new(capacity)),
            revision: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> LogSnapshot {
        let buffer = self.buffer.lock().unwrap_or_else(|error| error.into_inner());
        let revision = self.revision.load(Ordering::Acquire);
        LogSnapshot {
            revision,
            text: buffer.text.clone(),
        }
    }

    fn push(&self, text: &str) {
        let mut buffer = self.buffer.lock().unwrap_or_else(|error| error.into_inner());
        buffer.push(text);
        self.revision.fetch_add(1, Ordering::Release);
    }
}

struct LogBuffer {
    text: String,
    capacity: usize,
}

impl LogBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            text: String::new(),
            capacity,
        }
    }

    fn push(&mut self, text: &str) {
        if self.capacity == 0 {
            self.text.clear();
            return;
        }
        if text.len() >= self.capacity {
            let mut start = text.len() - self.capacity;
            while !text.is_char_boundary(start) {
                start += 1;
            }
            self.text.clear();
            self.text.push_str(&text[start..]);
            return;
        }
        self.text.push_str(text);
        if self.text.len() <= self.capacity {
            return;
        }
        let excess = self.text.len().saturating_sub(self.capacity);
        let mut cut = excess;
        while !self.text.is_char_boundary(cut) {
            cut = cut.saturating_add(1);
        }
        self.text.drain(..cut);
    }
}

fn logs() -> &'static LogStore {
    LOGS.get_or_init(|| LogStore::new(LOG_CAPACITY))
}

fn capture_state() -> &'static Mutex<Option<CaptureState>> {
    CAPTURE.get_or_init(|| Mutex::new(None))
}

fn shutdown_matching(id: Option<u64>) -> bool {
    let deadline = Instant::now() + STOP_TIMEOUT;
    let mut active = loop {
        match capture_state().try_lock() {
            Ok(active) => break active,
            Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    append_log("chibipop: log shutdown deadline reached; capture is still busy\n");
                    return false;
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
    };
    let matches = active
        .as_ref()
        .is_some_and(|state| id.is_none_or(|id| state.id == id));
    if matches {
        if let Some(mut state) = active.take() {
            state.shutdown_before(deadline);
            if state.is_redirected() {
                *active = Some(state);
            }
        }
    }
    active.is_none()
}

fn append_log(text: &str) {
    logs().push(text);
}

fn handle_raw(handle: HANDLE) -> isize {
    handle.0 as isize
}

fn handle_from_raw(raw: isize) -> HANDLE {
    HANDLE(raw as *mut std::ffi::c_void)
}

#[cfg(test)]
thread_local! {
    static FAIL_STD_SET: std::cell::Cell<Option<STD_HANDLE>> = const {
        std::cell::Cell::new(None)
    };
}
