//! This module holds the shm buffers that receive the copies
//! (ARCHITECTURE.md#capture-and-masking: shm everywhere, CPU crop, no
//! GPU plumbing inside a capture backend).
//!
//! Each pool gets one file in `XDG_RUNTIME_DIR`. The code unlinks the
//! file the instant it exists. The fd keeps the file alive. The
//! process can end in any way, and the file leaves nothing behind.
//! This is the same discipline as the control socket.
//!
//! The compositor maps the file writable and copies into it. This
//! code reads the bytes back with `read_at`, and does not map the
//! file. A mapping costs the same single kernel copy. The `read_at`
//! call also keeps this crate free of `unsafe`.

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_shm::{Format, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::{Dispatch, QueueHandle};

/// The buffer that a pool offers now.
struct Current {
    buffer: WlBuffer,
    h: i32,
    stride: i32,
    format: Format,
    w: i32,
}

/// One shm pool that can grow, and the buffer inside it.
pub struct Pool {
    file: File,
    pool: WlShmPool,
    size: i32,
    current: Option<Current>,
}

impl Pool {
    /// Creates a pool of `size` bytes in an unlinked file.
    pub fn new<D>(shm: &WlShm, qh: &QueueHandle<D>, tag: &str, size: i32) -> Result<Pool>
    where
        D: Dispatch<WlShmPool, ()> + 'static,
    {
        let size = size.max(4);
        let file = scratch_file(tag)?;
        file.set_len(size as u64).context("sizing the capture shm file")?;
        let pool = shm.create_pool(file.as_fd(), size, qh, ());
        Ok(Pool { file, pool, size, current: None })
    }

    /// Returns a buffer with exactly these parameters. The pool
    /// reuses the mapping while the shape of the previous grab still
    /// fits.
    ///
    /// The pool keeps ownership of the returned proxy. A copy in
    /// flight must keep its buffer. Therefore the pool swaps the
    /// buffer only for a *new* shape.
    pub fn buffer<D>(
        &mut self,
        qh: &QueueHandle<D>,
        w: i32,
        h: i32,
        stride: i32,
        format: Format,
    ) -> Result<WlBuffer>
    where
        D: Dispatch<WlBuffer, ()> + 'static,
    {
        // The format decides the bytes per pixel, not the pool. The
        // stride of a 24-bit buffer is three times its width, not four.
        anyhow::ensure!(
            w > 0 && h > 0 && stride > 0,
            "the compositor described a {w}x{h} buffer with stride {stride}"
        );
        if let Some(c) = &self.current {
            if c.w == w && c.h == h && c.stride == stride && c.format == format {
                return Ok(c.buffer.clone());
            }
        }
        let needed = stride
            .checked_mul(h)
            .with_context(|| format!("a {w}x{h} buffer of stride {stride} does not fit in memory"))?;
        if needed > self.size {
            self.file.set_len(needed as u64).context("growing the capture shm file")?;
            self.pool.resize(needed);
            self.size = needed;
        }
        if let Some(old) = self.current.take() {
            old.buffer.destroy();
        }
        let buffer = self.pool.create_buffer(0, w, h, stride, format, qh, ());
        self.current = Some(Current { buffer: buffer.clone(), w, h, stride, format });
        Ok(buffer)
    }

    /// Reads the first `len` bytes that the compositor wrote.
    pub fn read(&self, len: usize, into: &mut Vec<u8>) -> Result<()> {
        anyhow::ensure!(len <= self.size as usize, "a {len}-byte read past a {}-byte pool", self.size);
        into.clear();
        into.resize(len, 0);
        self.file.read_exact_at(into, 0).context("reading the copied pixels")
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        if let Some(c) = self.current.take() {
            c.buffer.destroy();
        }
        self.pool.destroy();
    }
}

/// Creates an unlinked file in the runtime dir. If there is no
/// runtime dir, this function uses the temp dir.
fn scratch_file(tag: &str) -> Result<File> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let path = dir.join(format!("chibipop-capture-{}-{tag}", std::process::id()));
    // This code must never adopt a stale file from a killed process.
    let _ = std::fs::remove_file(&path);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| format!("creating the capture shm file {}", path.display()))?;
    std::fs::remove_file(&path)
        .with_context(|| format!("unlinking the capture shm file {}", path.display()))?;
    Ok(file)
}
