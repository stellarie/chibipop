//! Serves the daemon control pipe.
//!
//! A settings process sends one `reload` line.
//! The configuration file holds the only truth.

use crate::app::WM_APP_CONTROL;
use crate::lock::config_key;
use anyhow::{Context, Result};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE, LPARAM, WPARAM,
};
use windows::Win32::Storage::FileSystem::{
    ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PeekNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW;

/// The request size limit, in bytes.
const REQUEST_LIMIT: usize = 256;

/// The reply size limit, in bytes.
const REPLY_LIMIT: usize = 480;

/// The size of each pipe buffer, in bytes.
const PIPE_BUFFER: u32 = 512;

/// The time a client has to send a line.
const READ_DEADLINE: Duration = Duration::from_secs(2);

/// The time the main thread has to reply.
const REPLY_DEADLINE: Duration = Duration::from_secs(5);

/// The time a client has to read a reply.
const DRAIN_DEADLINE: Duration = Duration::from_secs(2);

/// The gap between pipe polls.
const POLL_GAP: Duration = Duration::from_millis(10);

/// The failed connects in a row that stop serving.
const CONNECT_FAILURE_LIMIT: u32 = 20;

/// One request line and its reply channel.
pub struct ControlRequest {
    pub line: String,
    pub reply: mpsc::Sender<String>,
}

/// Makes the pipe name for one config file.
pub fn pipe_name(config_path: &Path) -> String {
    format!(r"\\.\pipe\chibipop-control-{}", config_key(config_path))
}

/// Owns the server end of the pipe.
struct PipeHandle(HANDLE);

// SAFETY: A kernel handle is valid on every thread. The thread that
// receives this value is the only user. This struct closes the handle
// once, in `Drop`.
unsafe impl Send for PipeHandle {}

impl Drop for PipeHandle {
    fn drop(&mut self) {
        // SAFETY: `spawn_listener` took `self.0` from `CreateNamedPipeW`.
        // This struct owns it alone, and `Drop` closes it once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Creates the pipe and serves it on a thread.
/// An error means the daemon has no listener.
pub fn spawn_listener(
    config_path: &Path,
    main_tid: u32,
    tx: mpsc::Sender<ControlRequest>,
) -> Result<()> {
    let name: Vec<u16> = pipe_name(config_path).encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `name` is a NUL-terminated UTF-16 buffer. It stays alive for
    // this call, and `CreateNamedPipeW` copies it. The null security
    // attributes select the default. The mode allows one instance, and
    // `FILE_FLAG_FIRST_PIPE_INSTANCE` fails when another process owns the name.
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            PIPE_BUFFER,
            PIPE_BUFFER,
            0,
            None,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error()).context("creating the control pipe");
    }
    let pipe = PipeHandle(handle);
    thread::Builder::new()
        .name("control-pipe".into())
        .spawn(move || serve(pipe, main_tid, tx))
        .context("starting the control pipe thread")?;
    Ok(())
}

/// Answers one client at a time.
fn serve(pipe: PipeHandle, main_tid: u32, tx: mpsc::Sender<ControlRequest>) {
    let mut failures = 0;
    loop {
        // SAFETY: `pipe.0` is a live pipe handle that this thread owns.
        // A null overlapped pointer selects a blocking connect.
        let connected = unsafe { ConnectNamedPipe(pipe.0, None) };
        let connected = match connected {
            Ok(()) => true,
            Err(error) => error.code() == ERROR_PIPE_CONNECTED.to_hresult(),
        };
        if !connected {
            failures += 1;
            if failures >= CONNECT_FAILURE_LIMIT {
                eprintln!("chibipop: the control pipe stopped after repeated errors");
                return;
            }
            disconnect(&pipe);
            thread::sleep(POLL_GAP * 5);
            continue;
        }
        failures = 0;
        let reply = match read_request(&pipe) {
            Some(line) => forward(line, main_tid, &tx),
            None => Some("ERR bad request".to_string()),
        };
        let Some(reply) = reply else { return };
        write_reply(&pipe, &reply);
        wait_for_client_close(&pipe);
        disconnect(&pipe);
    }
}

/// Reads one line, or returns `None`.
fn read_request(pipe: &PipeHandle) -> Option<String> {
    let deadline = Instant::now() + READ_DEADLINE;
    let mut buffer = [0u8; REQUEST_LIMIT];
    let mut length = 0;
    loop {
        if let Some(end) = buffer[..length].iter().position(|byte| *byte == b'\n') {
            let line = String::from_utf8_lossy(&buffer[..end]);
            return Some(line.trim_end_matches('\r').to_string());
        }
        if length == REQUEST_LIMIT || Instant::now() >= deadline {
            return None;
        }
        let mut available = 0u32;
        // SAFETY: `pipe.0` is a live pipe handle. `available` is a writable
        // `u32` for this call. The null pointers skip the other outputs.
        unsafe { PeekNamedPipe(pipe.0, None, 0, None, Some(&mut available), None) }.ok()?;
        if available == 0 {
            thread::sleep(POLL_GAP);
            continue;
        }
        let wanted = usize::try_from(available).unwrap_or(REQUEST_LIMIT).min(REQUEST_LIMIT - length);
        let mut read = 0u32;
        // SAFETY: `pipe.0` is a live pipe handle. The slice is writable and
        // stays inside `buffer`. At least `wanted` bytes wait in the pipe, so
        // this blocking read returns at once.
        unsafe { ReadFile(pipe.0, Some(&mut buffer[length..length + wanted]), Some(&mut read), None) }
            .ok()?;
        length += usize::try_from(read).unwrap_or(0);
    }
}

/// Posts the request and waits for the reply.
/// Returns `None` when the receiver is gone.
fn forward(line: String, main_tid: u32, tx: &mpsc::Sender<ControlRequest>) -> Option<String> {
    let (reply, answer) = mpsc::channel();
    tx.send(ControlRequest { line, reply }).ok()?;
    // SAFETY: This call wakes the main loop. A thread without a queue makes
    // it fail, and the request then waits for the next wake.
    unsafe {
        let _ = PostThreadMessageW(main_tid, WM_APP_CONTROL, WPARAM(0), LPARAM(0));
    }
    Some(answer.recv_timeout(REPLY_DEADLINE).unwrap_or_else(|_| "ERR timeout".to_string()))
}

/// Writes one reply line, 480 bytes at most.
fn write_reply(pipe: &PipeHandle, reply: &str) {
    let mut line = reply.replace(['\r', '\n'], " ");
    let mut end = line.len().min(REPLY_LIMIT);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line.truncate(end);
    line.push('\n');
    let mut written = 0u32;
    // SAFETY: `pipe.0` is a live pipe handle. The slice is readable for the
    // call. The line is shorter than the pipe buffer, so the write does not
    // wait for the client.
    let _ = unsafe { WriteFile(pipe.0, Some(line.as_bytes()), Some(&mut written), None) };
}

/// Waits for the client to close the pipe.
fn wait_for_client_close(pipe: &PipeHandle) {
    let deadline = Instant::now() + DRAIN_DEADLINE;
    while Instant::now() < deadline {
        let mut available = 0u32;
        // SAFETY: `pipe.0` is a live pipe handle. `available` is a writable
        // `u32` for this call. The null pointers skip the other outputs.
        let peeked = unsafe { PeekNamedPipe(pipe.0, None, 0, None, Some(&mut available), None) };
        if peeked.is_err() {
            return;
        }
        thread::sleep(POLL_GAP);
    }
}

/// Frees the pipe for the next client.
fn disconnect(pipe: &PipeHandle) {
    // SAFETY: `pipe.0` is a live pipe handle that this thread owns.
    unsafe {
        let _ = DisconnectNamedPipe(pipe.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both crates assert this one literal.
    #[test]
    fn pipe_name_golden() {
        assert_eq!(
            pipe_name(Path::new(r"C:\Users\x\chibipop.toml")),
            r"\\.\pipe\chibipop-control-b3e76833bdc645c8"
        );
    }
}
