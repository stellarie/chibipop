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

    /// Checks that every stored census format decodes into the cache.
    /// AVIF and SVG are the two primary formats in this test set.
    #[test]
    fn every_stored_census_format_decodes_into_the_cache() {
        let (db, _guard) = built("formats");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");
        for (path, w, h) in [
            ("gaiji/one.png", 12u32, 7u32),
            ("gaiji/three.jpg", 23, 11),
            ("gaiji/four.gif", 9, 5),
            ("gaiji/two.svg", 64, 32),
            ("gaiji/five.avif", 480, 120),
        ] {
            let key = MediaKey::new(1, path);
            let pixmap = cache
                .surface(&key, AS_DRAWN.0, AS_DRAWN.1)
                .unwrap_or_else(|e| panic!("{path}: {e}"));
            assert_eq!((w, h), (pixmap.width(), pixmap.height()), "{path}");
        }
    }

    #[test]
    fn an_asset_the_build_stored_no_row_for_is_not_stored() {
        let (db, _guard) = built("not_stored");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");
        for path in ["gaiji/missing.png", "gaiji/broken.png", "gaiji/unused.png"] {
            let key = MediaKey::new(1, path);
            assert_eq!(
                Err(Missing::NotStored),
                cache.surface(&key, AS_DRAWN.0, AS_DRAWN.1).map(|_| ()),
                "{path}",
            );
            // The cache stores the refusal, so the code does not query SQLite for each frame.
            // A refusal stores zero pixels.
            assert!(cache.surface(&key, AS_DRAWN.0, AS_DRAWN.1).is_err(), "{path}");
        }
        assert_eq!(0, cache.footprint(), "a refusal costs no pixels");
    }

    #[test]
    fn two_paths_sharing_one_blob_still_decode_to_their_own_surfaces() {
        // The store removes duplicate blobs, but the painter does not.
        // The cache tints a monochrome asset to the body text color.
        // Two nodes that share one blob can request different pixmaps.
        let (db, _guard) = built("shared_blob");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");
        assert!(cache.surface(&MediaKey::new(1, "gaiji/one.png"), Tint::None, (0, 0, 0)).is_ok());
        assert!(cache.surface(&MediaKey::new(1, "gaiji/copy.png"), Tint::None, (0, 0, 0)).is_ok());
        assert_eq!(2 * 12 * 7 * 4, cache.footprint(), "two paths, two surfaces");
    }

    #[test]
    fn the_budget_evicts_the_oldest_and_never_the_entry_just_admitted() {
        // The budget allows one surface.
        // A second surface must evict the first.
        // A surface larger than the whole budget must stay in the cache,
        // so visible assets remain available.
        let (db, _guard) = built("evicts");
        let mut cache = MediaSurfaces::with_budget(&db, 12 * 7 * 4).expect("the store opens");
        let one = MediaKey::new(1, "gaiji/one.png");
        let copy = MediaKey::new(1, "gaiji/copy.png");

        assert!(cache.surface(&one, AS_DRAWN.0, AS_DRAWN.1).is_ok());
        assert!(cache.surface(&copy, AS_DRAWN.0, AS_DRAWN.1).is_ok());
        assert_eq!(12 * 7 * 4, cache.footprint(), "the budget is honoured");
        assert!(!cache.slots.contains_key(&(one.clone(), None, None)), "the oldest went");
        assert!(cache.slots.contains_key(&(copy, None, None)), "the newest stayed");

        let mut tight = MediaSurfaces::with_budget(&db, 1).expect("the store opens");
        assert!(
            tight.surface(&one, AS_DRAWN.0, AS_DRAWN.1).is_ok(),
            "an oversized asset is still served"
        );
    }

    /// Checks the monochrome tint and the premultiply step.
    /// The alpha of a monochrome asset provides coverage for the text color.
    /// This step keeps black-ink gaiji legible on dark and light themes.
    ///
    /// This test checks decoded pixels from the project builder.
    /// A channel value larger than its alpha breaks tiny-skia alpha composition.
    #[test]
    fn a_monochrome_asset_is_tinted_to_the_text_colour_and_stays_premultiplied() {
        let (db, _guard) = built("tint");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");
        let key = MediaKey::new(1, "gaiji/one.png");

        let plain = cache.surface(&key, Tint::None, (0, 0, 0)).expect("it paints").clone();
        // Body text colors for dark theme and light theme.
        for ink in [(230u8, 230, 230), (24, 24, 24)] {
            let tinted = cache.surface(&key, Tint::Alpha, ink).expect("it paints").clone();
            assert_eq!(plain.width(), tinted.width(), "the same pixels, recoloured");
            assert_eq!(plain.height(), tinted.height());
            let mut opaque = 0;
            for (was, now) in
                plain.data().as_chunks::<4>().0.iter().zip(tinted.data().as_chunks::<4>().0)
            {
                assert_eq!(was[3], now[3], "alpha is the mask and is untouched");
                let a = u32::from(now[3]);
                for (i, channel) in [ink.0, ink.1, ink.2].into_iter().enumerate() {
                    let want = ((u32::from(channel) * a + 127) / 255) as u8;
                    assert_eq!(want, now[i], "channel {i} of a {a}-alpha pixel");
                    assert!(now[i] <= now[3], "premultiplied: {} > {}", now[i], now[3]);
                }
                opaque += usize::from(now[3] > 0);
            }
            assert!(opaque > 0, "the fixture has ink to tint");
        }
        // The cache uses three slots: one untinted asset and two tinted variants.
        // A theme change must not use the previous color.
        assert_eq!(3 * 12 * 7 * 4, cache.footprint());
    }

    /// Checks monochrome tint for vector assets.
    /// A monochrome SVG mask has no pixels until code selects a size.
    /// `SceneImage::tint` selects the size from the resolved box.
    /// The painter rasterizes the SVG to a pixmap at that size.
    /// The text color keeps the mask legible on dark and light panels.
    #[test]
    fn a_monochrome_vector_rasterizes_at_the_size_the_tint_asked_for() {
        let (db, _guard) = built("vector");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");
        let svg = MediaKey::new(1, "gaiji/two.svg");

        // Body text colors for dark and light themes.
        // An untinted mask would be invisible on one theme.
        // A tint makes it visible on both themes.
        for ink in [(210u8, 212u8, 218u8), (40, 42, 48)] {
            let mask = cache.surface(&svg, Tint::Raster(30, 30), ink).expect("it paints").clone();
            assert_eq!((30, 30), (mask.width(), mask.height()), "the tint's own size");
            let mut inked = 0;
            for px in mask.data().as_chunks::<4>().0 {
                let a = u32::from(px[3]);
                for (i, channel) in [ink.0, ink.1, ink.2].into_iter().enumerate() {
                    assert_eq!(((u32::from(channel) * a + 127) / 255) as u8, px[i], "{ink:?}");
                    assert!(px[i] <= px[3], "premultiplied");
                }
                inked += usize::from(px[3] > 0);
            }
            assert!(inked > 0, "a mask with no coverage would tint nothing");
        }
        // Two colors produce two pixmaps.
        // A theme change must not use the previous color.
        assert_eq!(2 * 30 * 30 * 4, cache.footprint(), "one mask per theme");

        // A second size creates an additional mask.
        // Vector pixels depend on the requested size, not the intrinsic file size.
        cache.surface(&svg, Tint::Raster(12, 12), (40, 42, 48)).expect("it paints");
        assert_eq!(2 * 30 * 30 * 4 + 12 * 12 * 4, cache.footprint(), "two sizes, two pixmaps");

        // An untinted vector rasterizes at its intrinsic size.
        // The media table stores this intrinsic size.
        let plain = cache.surface(&svg, Tint::None, (0, 0, 0)).expect("it paints");
        assert_eq!((64, 32), (plain.width(), plain.height()));
    }

    /// Checks that a rebuild is visible only to handles that reopen.
    ///
    /// A rebuild renames a new file over the Dictionary path.
    /// Current read-only connections keep the replaced file inode.
    /// A rebuild also renumbers `dict_id`, which forms part of the cache key.
    /// A popup with an old handle draws gaiji from the previous Dictionary.
    ///
    /// The Worker reopens its handle on `reload` (`worker::ReopenDict`).
    /// This cache is the second handle in the process, so it must also reopen.
    #[test]
    fn a_rebuild_is_invisible_to_a_handle_that_did_not_reopen() {
        let (db, _guard) = built("reopen_after_rebuild");
        let mut stale = MediaSurfaces::open(&db).expect("the store opens");
        let key = MediaKey::new(1, "gaiji/one.png");
        assert!(stale.surface(&key, AS_DRAWN.0, AS_DRAWN.1).is_ok(), "the asset is there");

        // The rebuild uses an archive without media and replaces the same path.
        let terms = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/yomitan/terms.zip");
        chibipop::dict::build::build(&[terms], &[], &db, &|_| {}).expect("the rebuild promotes");

        assert!(
            MediaSurfaces::open(&db)
                .expect("the new store opens")
                .surface(&key, AS_DRAWN.0, AS_DRAWN.1)
                .is_err(),
            "a reopened handle reads the dictionary that is actually there now",
        );
        // This test uses a fresh key, so the result comes from the new file rather than a cache entry.
        let other = MediaKey::new(1, "gaiji/three.jpg");
        assert!(
            stale.surface(&other, AS_DRAWN.0, AS_DRAWN.1).is_ok(),
            "while the handle that did not reopen is still serving the replaced file - \
             which is why the reload has to reopen it",
        );
    }
}
