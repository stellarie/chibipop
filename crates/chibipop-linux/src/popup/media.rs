//! This module caches decoded surfaces for popup paint.
//!
//! It keeps one `MediaStore` connection and a bounded map of decoded pixmaps.
//! It needs its own connection because the Worker owns the Dictionary on another thread,
//! and `rusqlite::Connection` is not `Sync`.
//! The painter needs neither the term index nor the parsed-tree cache.
//!
//! The cache prevents repeated decodes.
//! A hover over 字通 can contain more than four image nodes per term row.
//! The cache decodes each distinct gaiji once per session instead of once per frame.
//! The frame gate coalesces commits (`super::surface`).
//! Without the cache, a scroll would decode the same forty PNGs at the refresh rate.
//!
//! The cache stores every refusal.
//! An asset that this build cannot rasterize, or that the archive does not contain,
//! gives the same answer on every request.
//! A new SQLite query for each frame would waste work and return no pixels.
//!
//! The Windows equivalent is `crates/chibipop-windows/src/ui/media.rs`.
//! The files differ only in pixel format.
//! tiny-skia uses premultiplied RGBA8 pixmaps, which match Core's surface layout.
//! Linux therefore stores the Core layout directly.
//! Direct2D on Windows swaps two channels.
//!
//! Core owns cache state.
//! The byte budget, FIFO eviction, and rounded premultiply calculations live in
//! [`chibipop::dict::media::ByteBudget`], [`SURFACE_BUDGET`], and
//! [`chibipop::dict::media::premultiplied`].
//! None of these items uses a specific pixel format.
//! Both platforms share them, and each platform keeps its own paint code
//! (ARCHITECTURE.md#workspace-and-seams).
//! This module owns decode, channel order, and diagnostics.

use chibipop::dict::media::{premultiplied, ByteBudget, MediaKey, Missing, SURFACE_BUDGET};
use chibipop::lookup::sqlite::MediaStore;
use chibipop::ui::layout::{Rgb, Tint};
use std::path::Path;
use tiny_skia::{IntSize, Pixmap};

/// One cache slot contains an asset, a tint color, and a raster pixel size for vectors.
///
/// The color forms part of the key because a monochrome asset is a mask.
/// Two nodes can share one blob but need different pixmaps.
/// A theme change needs another pixmap.
/// `None` means an untinted asset.
/// Most Dictionaries in the census use untinted assets.
///
/// The raster size forms part of the key for vectors.
/// `Tint::Raster` exists because a vector has no pixels until code selects a size.
/// One SVG at two font sizes therefore needs two masks.
/// `None` means a raster asset or a vector at its intrinsic size.
type Slot = (MediaKey, Option<Rgb>, Option<(u32, u32)>);

/// Stores decoded assets under Core's byte budget, keyed by media key.
/// [`ByteBudget`] evicts entries in insertion order to bound memory use.
pub struct MediaSurfaces {
    store: MediaStore,
    slots: ByteBudget<Slot, Result<Pixmap, Missing>>,
    notes: Vec<String>,
}

impl MediaSurfaces {
    /// Opens the media store beside the Dictionary.
    ///
    /// No `invalidate` function exists.
    /// A rebuild renames a new database over the old path.
    /// This read-only connection keeps the old inode.
    /// A reload therefore replaces the whole cache instead of a clear operation.
    pub fn open(db: &Path) -> anyhow::Result<MediaSurfaces> {
        MediaSurfaces::with_budget(db, SURFACE_BUDGET)
    }

    fn with_budget(db: &Path, budget: usize) -> anyhow::Result<MediaSurfaces> {
        Ok(MediaSurfaces {
            store: MediaStore::open(db)?,
            slots: ByteBudget::new(budget),
            notes: Vec::new(),
        })
    }

    /// Gets the pixmap for one asset with the requested scene tint.
    ///
    /// `Err` is an expected result, not an unhandled failure. It tells the caller to draw
    /// the `alt` text and gives the diagnostic log its specific reason.
    ///
    /// This function applies the tint when it admits an entry.
    /// It does not apply the tint on each frame.
    /// A scroll repaints the asset at the refresh rate.
    /// Without a cache, each repaint would rewrite the mask surface.
    /// `Tint::Raster` also gives the raster size.
    /// Core quantizes the resolved box, so both platform bins request the same pixels.
    /// This function reads that size and passes it to the store.
    pub fn surface(
        &mut self,
        key: &MediaKey,
        tint: Tint,
        color: Rgb,
    ) -> Result<&Pixmap, Missing> {
        let at = match tint {
            Tint::Raster(w, h) => Some((w, h)),
            Tint::None | Tint::Alpha => None,
        };
        let slot: Slot = (key.clone(), (tint != Tint::None).then_some(color), at);
        if !self.slots.contains_key(&slot) {
            let mut pixels = self.load(key, at);
            if let (Ok(pixmap), true) = (&mut pixels, tint != Tint::None) {
                tint_alpha(pixmap.data_mut(), color);
            }
            self.admit(slot.clone(), pixels);
        }
        match self.slots.get(&slot) {
            Some(Ok(pixmap)) => Ok(pixmap),
            Some(Err(why)) => Err(why.clone()),
            // This arm is unreachable because `admit` never evicts the newest entry.
            None => Err(Missing::NotStored),
        }
    }

    /// Returns diagnostics collected since the last drain for the daemon log.
    ///
    /// A database store fault generates one log line.
    /// An unsupported image format such as AVIF does not log repeated errors.
    /// The cache answers repeated requests from its map after the first query.
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// Returns the count of decoded pixels currently stored in memory.
    pub fn footprint(&self) -> usize {
        self.slots.footprint()
    }

    fn load(&self, key: &MediaKey, at: Option<(u32, u32)>) -> Result<Pixmap, Missing> {
        let surface = self.store.surface(key, at)?;
        let (w, h) = (surface.w, surface.h);
        IntSize::from_wh(w, h)
            .and_then(|size| Pixmap::from_vec(surface.rgba, size))
            // Core checks `w * h * 4` during decode.
            // This branch runs only if tiny-skia refuses a dimension.
            .ok_or_else(|| {
                Missing::Unavailable(format!("media {key}: tiny-skia refused {w}x{h}"))
            })
    }

    fn admit(&mut self, slot: Slot, pixels: Result<Pixmap, Missing>) {
        // A store fault generates one line.
        // An unsupported format such as AVIF does not generate one line for each key.
        if let Err(Missing::Unavailable(why)) = &pixels {
            self.notes.push(why.clone());
        }
        // A refusal keeps zero bytes, so broken assets do not consume the budget.
        // Core's `admit` handles eviction and never drops the newest entry.
        let bytes = pixels.as_ref().map_or(0, |p| p.data().len());
        self.slots.admit(slot, pixels, bytes);
    }
}

/// Fills mask pixels with the replacement color.
///
/// Input and output use premultiplied RGBA8.
/// This function discards the asset color and uses alpha as coverage for `color`.
/// A monochrome asset is authored as black ink for a light background.
/// Without a tint, that asset is invisible on a dark panel.
///
/// This code multiplies channels because chibipop has no filter stack.
/// Yomitan and Hoshi Reader use CSS `brightness(0) invert(1)`.
/// That CSS approach needs a filter stack and cannot produce arbitrary colors.
fn tint_alpha(rgba: &mut [u8], color: Rgb) {
    for px in rgba.as_chunks_mut::<4>().0 {
        // Each channel is premultiplied by alpha.
        // This code must scale the new color channels by the same alpha value.
        // `premultiplied` scales each channel and enforces the invariant `r <= a`.
        // Memory uses RGBA order to match Core and tiny-skia.
        let a = px[3];
        px[0] = premultiplied(color.0, a);
        px[1] = premultiplied(color.1, a);
        px[2] = premultiplied(color.2, a);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Removes the temporary database file when the guard drops.
    /// Other database tests use the same guard pattern.
    struct TempDbGuard(PathBuf);

    impl Drop for TempDbGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Builds a database from the committed media fixture archive.
    ///
    /// This helper uses the project builder instead of a static schema.
    /// The cache must read the exact output of a build.
    /// A test with hand-written tables would not check actual behavior.
    fn built(test_name: &str) -> (PathBuf, TempDbGuard) {
        let dir = std::env::temp_dir().join("chibipop_linux_media_test");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join(format!("t_{}_{test_name}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let guard = TempDbGuard(out.clone());
        let archive = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/media/media.zip");
        chibipop::dict::build::build(&[archive], &[], &out, &|_| {}).expect("the fixture builds");
        (out, guard)
    }

    /// The untinted asset matches most Dictionaries in the census.
    const AS_DRAWN: (Tint, Rgb) = (Tint::None, (0, 0, 0));

    #[test]
    fn a_stored_png_decodes_once_and_answers_from_the_cache_after_that() {
        let (db, _guard) = built("cache_hit");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");
        let key = MediaKey::new(1, "gaiji/one.png");

        let first =
            cache.surface(&key, AS_DRAWN.0, AS_DRAWN.1).expect("a stored PNG paints").clone();
        assert_eq!((12, 7), (first.width(), first.height()));
        let held = cache.footprint();
        assert_eq!(12 * 7 * 4, held, "the budget counts decoded pixels");

        // The pixmap is premultiplied and identical on the second request.
        // The cache does not reread the file or decode it again.
        let second = cache.surface(&key, AS_DRAWN.0, AS_DRAWN.1).expect("still paints");
        assert_eq!(first.data(), second.data());
        assert_eq!(held, cache.footprint(), "a hit admits nothing");
    }
}
