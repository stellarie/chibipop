//! Runs one plugin process.

use crate::plugin::manifest::Manifest;
use crate::plugin::proto::{Hello, Ready};
use crate::plugin::version::agree;
use anyhow::{bail, Context, Result};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_BASIC_LIMIT_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

pub struct Host {
    child: Child,
    job: Option<Job>,
    outbox: Arc<Outbox>,
    lines: Receiver<Result<String, std::io::Error>>,
    ready: Ready,
    next_id: u64,
    stderr_log: Arc<Mutex<VecDeque<String>>>,
}

/// The maximum number of lines in the ring buffer.
const LOG_CAP: usize = 50;

/// Holds recent stderr lines from the active plugin.
static ENGINE_LOG: OnceLock<Arc<Mutex<VecDeque<String>>>> = OnceLock::new();

/// Returns recent stderr lines from the active plugin.
pub fn engine_log_lines() -> Vec<String> {
    match ENGINE_LOG.get() {
        Some(log) => log.lock().unwrap().iter().cloned().collect(),
        None => Vec::new(),
    }
}

/// Removes the oldest line when the buffer is full.
fn push_log_line(buf: &Mutex<VecDeque<String>>, line: String) {
    let mut b = buf.lock().unwrap();
    if b.len() >= LOG_CAP {
        b.pop_front();
    }
    b.push_back(line);
}

#[derive(Default)]
struct Queued {
    bytes: Option<Vec<u8>>,
    closed: bool,
}

/// Stores at most one request. A new request replaces the queued request.
#[derive(Default)]
struct Outbox {
    queued: Mutex<Queued>,
    ready: Condvar,
}

impl Outbox {
    fn put(&self, bytes: Vec<u8>) -> bool {
        let mut q = self.queued.lock().unwrap();
        if q.closed {
            return false;
        }
        // Replace a stale request that has no reader.
        q.bytes = Some(bytes);
        self.ready.notify_one();
        true
    }

    fn clear(&self) {
        self.queued.lock().unwrap().bytes = None;
    }

    fn close(&self) {
        self.queued.lock().unwrap().closed = true;
        self.ready.notify_one();
    }

    fn take(&self) -> Option<Vec<u8>> {
        let mut q = self.queued.lock().unwrap();
        loop {
            if let Some(bytes) = q.bytes.take() {
                return Some(bytes);
            }
            if q.closed {
                return None;
            }
            q = self.ready.wait(q).unwrap();
        }
    }
}

/// A job that kills its member processes when it closes.
struct Job(HANDLE);

// SAFETY: A job handle names a kernel object and is not thread-affine. Any
// thread in this process can call SetInformationJobObject,
// AssignProcessToJobObject, or CloseHandle with this handle. This handle can
// cross threads and preserve every invariant. `Host` must stay `Send` because
// a worker thread calls the plugin, not the thread that spawned it.
unsafe impl Send for Job {}

impl Job {
    fn create() -> Result<Job> {
        // SAFETY: A null name asks for an unnamed job, and a null security
        // descriptor asks for the default one. `None` and a null `PCWSTR` have
        // these meanings here. This call does not dereference either value. The
        // `Job` on the next line owns the returned handle, and `Drop` closes it
        // exactly once.
        let handle =
            unsafe { CreateJobObjectW(None, PCWSTR::null()) }.context("CreateJobObjectW")?;
        let job = Job(handle);
        let info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
            BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: `job.0` is the handle created above, and it remains open.
        // `JobObjectExtendedLimitInformation` is the class whose payload is
        // exactly `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`. The pointer type and
        // byte count match the payload that the call reads. `info` is fully
        // initialized, outlives the call, and the call only reads it.
        unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .context("SetInformationJobObject")?;
        Ok(job)
    }

    fn adopt(&self, child: &Child) -> Result<()> {
        // SAFETY: `self.0` stays open while `self` exists. The process handle is
        // borrowed from `child`, which outlives this call. This call neither
        // stores nor closes the handle. A handle from `Command::spawn` carries
        // PROCESS_SET_QUOTA and PROCESS_TERMINATE, which this call needs.
        unsafe { AssignProcessToJobObject(self.0, HANDLE(child.as_raw_handle())) }
            .context("AssignProcessToJobObject")
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `CreateJobObjectW` in `create`, and this
        // struct owns it alone. `Drop` runs exactly once. This is the only
        // handle, so its close triggers KILL_ON_JOB_CLOSE.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub fn spawn(m: &Manifest, dir: &Path) -> Result<Host> {
    let job = Job::create()?;
    let mut child = Command::new(&m.command)
        .args(&m.args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting plugin \"{}\"", m.name))?;
    if let Err(e) = job.adopt(&child) {
        // The job cannot kill this process because `adopt` failed.
        if child.kill().is_ok() {
            let _ = child.wait();
        }
        return Err(e);
    }

    let mut stdin = child.stdin.take().context("plugin stdin")?;
    let stdout = child.stdout.take().context("plugin stdout")?;
    let stderr = child.stderr.take().context("plugin stderr")?;

    let (tx, lines) = mpsc::channel();
    // No code can interrupt this read.
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let stderr_log = Arc::clone(
        ENGINE_LOG.get_or_init(|| Arc::new(Mutex::new(VecDeque::with_capacity(LOG_CAP)))));
    let log_mine = Arc::clone(&stderr_log);
    // The thread name appears in panic messages.
    std::thread::Builder::new()
        .name(format!("{}-stderr", m.name))
        .spawn(move || {
            for line in BufReader::new(stderr).lines() {
                // Stop the loop when the stderr read fails.
                let Ok(line) = line else { break };
                eprintln!("{line}");
                let _ = std::io::Write::flush(&mut std::io::stderr());
                push_log_line(&log_mine, line);
            }
        })
        .context("spawning the stderr reader")?;

    let outbox: Arc<Outbox> = Arc::default();
    let mine = Arc::clone(&outbox);
    // This thread writes requests so a full pipe does not stop the caller.
    std::thread::spawn(move || {
        while let Some(msg) = mine.take() {
            if stdin.write_all(&msg).is_err() || stdin.flush().is_err() {
                break;
            }
        }
        mine.close();
    });

    let mut h = Host {
        child, job: Some(job), outbox, lines, ready: blank_ready(), next_id: 1, stderr_log,
    };
    let hello = Hello {
        chibipop: env!("CARGO_PKG_VERSION").to_string(),
        protocol_supported: crate::plugin::manifest::SUPPORTED.to_vec(),
        features: vec![],
        roles_wanted: m.roles.clone(),
    };
    let v = h.call("hello", serde_json::to_value(&hello)?, Duration::from_secs(5))?;
    let ready: Ready = serde_json::from_value(v).context("reading the handshake")?;
    agree(crate::plugin::manifest::SUPPORTED, ready.protocol, m.protocol)?;
    h.ready = ready;
    Ok(h)
}

fn blank_ready() -> Ready {
    Ready {
        protocol: 0,
        name: String::new(),
        version: String::new(),
        roles: vec![],
        features: vec![],
        capabilities: None,
    }
}

impl Host {
    pub fn ready(&self) -> &Ready {
        &self.ready
    }

    /// Returns recent stderr lines from this plugin.
    pub fn recent_log(&self) -> Vec<String> {
        self.stderr_log.lock().unwrap().iter().cloned().collect()
    }

    pub fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
        deadline: Duration,
    ) -> Result<serde_json::Value> {
        let got = self.attempt(method, params, deadline);
        if got.is_err() {
            // Only this request can remain queued.
            self.outbox.clear();
        }
        got
    }

    fn attempt(
        &mut self,
        method: &str,
        params: serde_json::Value,
        deadline: Duration,
    ) -> Result<serde_json::Value> {
        // Stale lines cannot belong to this request.
        while self.lines.try_recv().is_ok() {}

        let id = self.next_id;
        self.next_id += 1;
        let line = crate::plugin::proto::request(id, method, params);
        if !self.outbox.put(line.into_bytes()) {
            bail!("the plugin's input is closed");
        }

        let started = Instant::now();
        loop {
            // Use one deadline for the request, not one deadline per line.
            let left = match deadline.checked_sub(started.elapsed()) {
                Some(d) if !d.is_zero() => d,
                _ => bail!("plugin missed its {} ms deadline", deadline.as_millis()),
            };
            let got = match self.lines.recv_timeout(left) {
                Ok(l) => l.context("reading from the plugin")?,
                Err(RecvTimeoutError::Timeout) => {
                    bail!("plugin missed its {} ms deadline", deadline.as_millis())
                }
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("plugin closed its output, or it exited")
                }
            };
            let v: serde_json::Value =
                serde_json::from_str(&got).context("plugin sent malformed JSON")?;
            if v.get("id").and_then(|i| i.as_u64()) != Some(id) {
                continue;
            }
            if let Some(e) = v.get("error") {
                bail!("plugin reported: {e}");
            }
            return v.get("result").cloned().context("plugin sent no result");
        }
    }

    pub fn shutdown(&mut self) {
        // Wake the idle writer thread so it can exit.
        self.outbox.close();
        // A failed kill does not reap the child.
        if self.child.kill().is_ok() {
            let _ = self.child.wait();
        }
        // The job kills the process tree when its handle closes.
        drop(self.job.take());
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_request_replaces_the_first_rather_than_queueing_behind_it() {
        let outbox = Outbox::default();
        assert!(outbox.put(b"one".to_vec()));
        assert!(outbox.put(b"two".to_vec()));
        assert_eq!(outbox.take(), Some(b"two".to_vec()));
        outbox.close();
        assert_eq!(outbox.take(), None);
    }

    /// The `deaf` plugin must not leave a request buffer queued.
    #[test]
    fn an_abandoned_request_is_dropped_rather_than_left_queued() {
        let outbox = Outbox::default();
        assert!(outbox.put(vec![0u8; 256 * 1024]));
        outbox.clear();
        outbox.close();
        // The queued request would retain 256 KiB.
        assert!(outbox.take().is_none());
    }

    #[test]
    fn a_closed_outbox_refuses_a_request_instead_of_swallowing_it() {
        let outbox = Outbox::default();
        outbox.close();
        assert!(!outbox.put(b"one".to_vec()));
        assert_eq!(outbox.take(), None);
    }

    /// The writer thread must exit when the outbox closes.
    #[test]
    fn a_waiting_writer_wakes_when_the_outbox_closes() {
        let outbox = Arc::new(Outbox::default());
        let mine = Arc::clone(&outbox);
        let writer = std::thread::spawn(move || mine.take());
        outbox.close();
        assert_eq!(writer.join().unwrap(), None);
    }

    #[test]
    fn the_log_ring_keeps_only_the_newest_lines_once_full() {
        let buf = Mutex::new(VecDeque::new());
        for i in 0..LOG_CAP + 5 {
            push_log_line(&buf, i.to_string());
        }
        let got: Vec<String> = buf.lock().unwrap().iter().cloned().collect();
        assert_eq!(got.len(), LOG_CAP);
        assert_eq!(got.first(), Some(&"5".to_string()));
        assert_eq!(got.last(), Some(&(LOG_CAP + 4).to_string()));
    }
}
