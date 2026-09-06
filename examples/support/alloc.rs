// ---------------------------------------------------------------------------
// allocation counter
// ---------------------------------------------------------------------------

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::Relaxed};

pub(crate) static COUNT_ON: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static FREES: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);
static FREED_BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicI64 = AtomicI64::new(0);
static PEAK: AtomicI64 = AtomicI64::new(0);

/// `Counting` wraps `System` and adds counters behind one relaxed load.
///
/// `realloc` delegates to `System::realloc` instead of the trait default.
/// The default allocates, copies, and deallocates.
/// It copies every `Vec` during growth, even when the allocator can extend the block in place.
/// This behavior penalizes the arena because it grows only a few large vectors.
struct Counting;

impl Counting {
    #[inline]
    fn on_alloc(size: usize) {
        ALLOCS.fetch_add(1, Relaxed);
        ALLOC_BYTES.fetch_add(size as u64, Relaxed);
        let live = LIVE.fetch_add(size as i64, Relaxed) + size as i64;
        PEAK.fetch_max(live, Relaxed);
    }

    #[inline]
    fn on_free(size: usize) {
        FREES.fetch_add(1, Relaxed);
        FREED_BYTES.fetch_add(size as u64, Relaxed);
        LIVE.fetch_sub(size as i64, Relaxed);
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc(layout);
        if !p.is_null() && COUNT_ON.load(Relaxed) {
            Self::on_alloc(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = System.alloc_zeroed(layout);
        if !p.is_null() && COUNT_ON.load(Relaxed) {
            Self::on_alloc(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if COUNT_ON.load(Relaxed) {
            Self::on_free(layout.size());
        }
        System.dealloc(ptr, layout);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = System.realloc(ptr, layout, new_size);
        if !p.is_null() && COUNT_ON.load(Relaxed) {
            Self::on_free(layout.size());
            Self::on_alloc(new_size);
        }
        p
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[derive(Clone, Copy, Default)]
pub(crate) struct AllocStat {
    pub(crate) allocs: u64,
    pub(crate) frees: u64,
    pub(crate) alloc_bytes: u64,
    pub(crate) freed_bytes: u64,
    pub(crate) live: i64,
    pub(crate) peak: i64,
}

pub(crate) fn alloc_reset() {
    ALLOCS.store(0, Relaxed);
    FREES.store(0, Relaxed);
    ALLOC_BYTES.store(0, Relaxed);
    FREED_BYTES.store(0, Relaxed);
    LIVE.store(0, Relaxed);
    PEAK.store(0, Relaxed);
}

pub(crate) fn alloc_snapshot() -> AllocStat {
    AllocStat {
        allocs: ALLOCS.load(Relaxed),
        frees: FREES.load(Relaxed),
        alloc_bytes: ALLOC_BYTES.load(Relaxed),
        freed_bytes: FREED_BYTES.load(Relaxed),
        live: LIVE.load(Relaxed),
        peak: PEAK.load(Relaxed),
    }
}
