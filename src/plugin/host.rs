//! Running one plugin process.

use crate::plugin::manifest::Manifest;
use crate::plugin::proto::{Hello, Ready};
use crate::plugin::version::agree;
use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

pub struct Host {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<Result<String, std::io::Error>>,
    ready: Ready,
    next_id: u64,
}

pub fn spawn(m: &Manifest, dir: &Path) -> Result<Host> {
    let mut child = Command::new(&m.command)
        .args(&m.args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("starting plugin \"{}\"", m.name))?;

    let stdin = child.stdin.take().context("plugin stdin")?;
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

    let mut h = Host { child, stdin, lines, ready: blank_ready(), next_id: 1 };
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
        let id = self.next_id;
        self.next_id += 1;
        let line = crate::plugin::proto::request(id, method, params);
        self.stdin.write_all(line.as_bytes()).context("writing to the plugin")?;
        self.stdin.flush().context("flushing to the plugin")?;

        loop {
            let got = match self.lines.recv_timeout(deadline) {
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
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.shutdown();
    }
}
