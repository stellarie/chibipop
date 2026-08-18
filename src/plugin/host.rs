//! Running one plugin process.

use crate::plugin::manifest::Manifest;
use crate::plugin::proto::{Hello, Ready};
use crate::plugin::version::agree;
use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
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
    writes: Sender<Vec<u8>>,
    lines: Receiver<Result<String, std::io::Error>>,
    ready: Ready,
    next_id: u64,
}

/// Kills its members on close.
struct Job(HANDLE);

// SAFETY: a job handle names a kernel object and is not thread-affine. Every
// call this type makes through it - SetInformationJobObject,
// AssignProcessToJobObject, CloseHandle - is valid from any thread of the
// process holding the handle, so moving it between threads breaks no
// invariant. `Host` has to stay `Send`: a plugin call runs on the worker
// thread, not the one that spawned it.
unsafe impl Send for Job {}

impl Job {
    fn create() -> Result<Job> {
        // SAFETY: a null name asks for an unnamed job and a null security
        // descriptor asks for the default one. That is what `None` and a null
        // `PCWSTR` mean here, and neither is dereferenced. The handle returned
        // is owned by the `Job` built from it on the next line, and is closed
        // exactly once, in `Drop`.
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
        // SAFETY: `job.0` is the handle created above and still open.
        // `JobObjectExtendedLimitInformation` is the class whose payload is
        // exactly `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`, so the pointee type
        // and the byte count agree with what the call reads. `info` is fully
        // initialised, outlives the call, and the call only reads it.
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
        // SAFETY: `self.0` stays open for as long as `self` lives. The process
        // handle is borrowed from `child`, which outlives this call, and the
        // call neither stores nor closes it. A handle from `Command::spawn`
        // carries PROCESS_SET_QUOTA and PROCESS_TERMINATE, the access needed.
        unsafe { AssignProcessToJobObject(self.0, HANDLE(child.as_raw_handle())) }
            .context("AssignProcessToJobObject")
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `CreateJobObjectW` in `create`, is owned
        // by this struct alone, and `Drop` runs exactly once. It is also the
        // only handle, so this close is what KILL_ON_JOB_CLOSE acts on.
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
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("starting plugin \"{}\"", m.name))?;
    if let Err(e) = job.adopt(&child) {
        // The job cannot sweep it.
        if child.kill().is_ok() {
            let _ = child.wait();
        }
        return Err(e);
    }

    let mut stdin = child.stdin.take().context("plugin stdin")?;
    let stdout = child.stdout.take().context("plugin stdout")?;

    let (tx, lines) = mpsc::channel();
    // A read cannot be interrupted.
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let (writes, outbox) = mpsc::channel::<Vec<u8>>();
    // Nor a write to a full pipe.
    std::thread::spawn(move || {
        for msg in outbox {
            if stdin.write_all(&msg).is_err() || stdin.flush().is_err() {
                break;
            }
        }
    });

    let mut h = Host { child, job: Some(job), writes, lines, ready: blank_ready(), next_id: 1 };
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

    pub fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
        deadline: Duration,
    ) -> Result<serde_json::Value> {
        // Stale lines are never ours.
        while self.lines.try_recv().is_ok() {}

        let id = self.next_id;
        self.next_id += 1;
        let line = crate::plugin::proto::request(id, method, params);
        self.writes
            .send(line.into_bytes())
            .context("the plugin's input is closed")?;

        let started = Instant::now();
        loop {
            // One budget, not one per line.
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
        // A failed kill never reaps.
        if self.child.kill().is_ok() {
            let _ = self.child.wait();
        }
        // The close kills the tree.
        drop(self.job.take());
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.shutdown();
    }
}
