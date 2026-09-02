//! Caches decoded popup media for the paint side.
//!
//! This module owns one `MediaStore` connection and a bounded map of decoded bitmaps.
//! The worker owns the dictionary on another thread.
//! A `rusqlite::Connection` is not `Sync`, so the painter needs its own connection.
//! The painter needs no term index or parsed-tree cache.
//!
//! A hover over 字通 averages more than four image nodes per term row.
//! The cache decodes each distinct gaiji once per session instead of once per `paint_once`.
//! The cache also stores every refusal.
//! An asset that this build cannot rasterize, or that the archive lacks, stays a permanent answer.
//! The cache does not ask SQLite again for the same asset on each frame.
//!
//! The Linux version is `crates/chibipop-linux/src/popup/media.rs`.
//! This module has the same role as [`super::render`]'s `paint_once` and that adapter's
//! `paint::panel`.
//! The bins differ only in pixel order.
//! Core supplies premultiplied RGBA8, which matches tiny-skia's pixmap layout.
//! This module swaps two channels into the format that `CreateBitmap` takes.
//!
//! Core owns the shared cache rules.
//! [`chibipop::dict::media::ByteBudget`], [`SURFACE_BUDGET`] and
//! [`chibipop::dict::media::premultiplied`] define the byte budget, FIFO eviction, and rounded
//! premultiply.
//! None of the three carries a pixel format. This fact lets core share them while the platform
//! paints (ARCHITECTURE.md#workspace-and-seams).
//! This module keeps the channel swap, the bitmap layout that `CreateBitmap` needs, and the
//! diagnostics.
//!
//! This module does not hold an `ID2D1Bitmap`.
//! A device bitmap belongs to a render target and dies when the device is lost.
//! These bytes outlive that loss, and the paint pass uploads them.

use chibipop::dict::media::{
    premultiplied, ByteBudget, MediaKey, Missing, Surface, SURFACE_BUDGET,
};
use chibipop::lookup::sqlite::MediaStore;
use chibipop::ui::layout::{Rgb, Tint};
use std::path::Path;

/// Stores decoded pixels in the layout that `ID2D1RenderTarget::
/// CreateBitmap` accepts without a WIC converter:
/// `DXGI_FORMAT_B8G8R8A8_UNORM` with `D2D1_ALPHA_MODE_PREMULTIPLIED`,
/// top-down and without row padding.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Bitmap {
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
}

impl Bitmap {
    /// Returns bytes per row for `CreateBitmap` as its `pitch`.
    /// The buffer has no padding. Its size is `w * h * 4`.
    pub fn stride(&self) -> u32 {
        self.w * 4
    }
}

/// Defines one cache slot with an asset, its tint color, and a vector pixel size.
///
/// The color belongs in the key because a monochrome asset acts as a *mask*.
/// Two nodes can share one blob but need two bitmaps. A theme change can need a third bitmap.
/// `None` means the asset has no tint. All but 15 of the census's 72 dictionaries use that form.
///
/// The raster size belongs in the key for the same reason.
/// `Tint::Raster` exists because a vector has no pixels until a caller picks a size.
/// One SVG at two font sizes therefore needs two masks.
/// `None` means a raster asset or a vector at its intrinsic size.
type Slot = (MediaKey, Option<Rgb>, Option<(u32, u32)>);

/// Stores decoded assets by media key under core's byte budget and insertion-order eviction
/// ([`ByteBudget`]).
pub struct MediaSurfaces {
    store: MediaStore,
    slots: ByteBudget<Slot, Result<Bitmap, Missing>>,
    notes: Vec<String>,
}

impl MediaSurfaces {
    /// Opens the media store beside the dictionary.
    ///
    /// This type does not have an `invalidate` method.
    /// A rebuild replaces the database file while this read-only connection keeps the old file.
    /// A reload must replace the whole cache instead of clearing it.
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

    /// Gets the bitmap for one asset with the tint that the scene requests.
    ///
    /// `Err` is the `alt`-text fallback cue, not an operation failure.
    /// It identifies one of three reasons so diagnostics can state the reason.
    ///
    /// The cache applies the tint once before it stores the bitmap.
    /// `paint_once` runs on every damage, so repeated tint work would rewrite the whole mask.
    /// `Tint::Raster` is also the raster size.
    /// Core quantizes the resolved box, so both bins request the same pixels.
    /// This function reads that size and gives it to the store.
    pub fn bitmap(
        &mut self,
        key: &MediaKey,
        tint: Tint,
        color: Rgb,
    ) -> Result<&Bitmap, Missing> {
        let at = match tint {
            Tint::Raster(w, h) => Some((w, h)),
            Tint::None | Tint::Alpha => None,
        };
        let slot: Slot = (key.clone(), (tint != Tint::None).then_some(color), at);
        if !self.slots.contains_key(&slot) {
            let mut pixels = self.store.surface(key, at).map(to_bgra);
            if let (Ok(bitmap), true) = (&mut pixels, tint != Tint::None) {
                tint_alpha(&mut bitmap.bgra, color);
            }
            self.admit(slot.clone(), pixels);
        }
        match self.slots.get(&slot) {
            Some(Ok(bitmap)) => Ok(bitmap),
            Some(Err(why)) => Err(why.clone()),
            // Unreachable: `admit` never evicts the entry it just added.
            None => Err(Missing::NotStored),
        }
    }

    /// Returns diagnostics recorded after the last drain for the application log.
    ///
    /// A real store fault needs one line.
    /// An AVIF that this build cannot rasterize does not.
    /// A repeated refusal does not need a line because the cache answers from its map after the
    /// first request.
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// Returns the decoded pixels that the cache holds.
    pub fn footprint(&self) -> usize {
        self.slots.footprint()
    }

    fn admit(&mut self, slot: Slot, pixels: Result<Bitmap, Missing>) {
        // Only a real store fault needs a line. A dictionary that ships an AVIF does not.
        // This message would repeat for each key in the session.
        // A refusal stores no pixels. This rule keeps broken assets out of the budget.
        // Core's `admit` evicts entries. It also never drops the entry that it just took.
        if let Err(Missing::Unavailable(why)) = &pixels {
            self.notes.push(why.clone());
        }
        let bytes = pixels.as_ref().map_or(0, |b| b.bgra.len());
        self.slots.admit(slot, pixels, bytes);
    }
}

/// Converts premultiplied RGBA8 to premultiplied BGRA8 in place.
///
/// The function swaps two channels in the buffer that core allocates.
/// It does not copy the asset or do work on each frame.
/// The cache converts each bitmap once and stores it in the format that the device needs.
fn to_bgra(mut surface: Surface) -> Bitmap {
    for px in surface.rgba.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    Bitmap { w: surface.w, h: surface.h, bgra: surface.rgba }
}

/// Recolors a mask's pixels with the color that represents the asset.
///
/// The input and output use premultiplied BGRA8.
/// The asset color is discarded. Its alpha becomes the coverage of `color`.
/// A monochrome asset is a *mask*: it is black ink authored for a light page and invisible on a
/// dark panel when composited as drawn.
///
/// This function multiplies channels. It does not use a filter.
/// chibipop has no filter stack.
/// Yomitan's and Hoshi Reader's `brightness(0) invert(1)` shortcut needs a filter.
/// That shortcut cannot produce a color other than black or white, although the theme's body text
/// usually uses another color.
///
/// The Linux version is `chibipop_linux::media::tint_alpha`.
/// Both functions use core's [`premultiplied`] and differ only in channel order.
/// Core supplies RGBA, and `CreateBitmap` takes BGRA.
fn tint_alpha(bgra: &mut [u8], color: Rgb) {
    for px in bgra.as_chunks_mut::<4>().0 {
        // Premultiplied pixels already scale each channel by alpha.
        // The new channels must use the same scale. `premultiplied` does this and keeps
        // `b <= a`, the invariant that `D2D1_ALPHA_MODE_PREMULTIPLIED` needs.
        let a = px[3];
        px[0] = premultiplied(color.2, a);
        px[1] = premultiplied(color.1, a);
        px[2] = premultiplied(color.0, a);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Removes the file on drop, like the other database tests.
    struct TempDbGuard(PathBuf);

    impl Drop for TempDbGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Builds a database from the committed media fixture archive.
    ///
    /// The real builder, not a hand-written schema, writes the database.
    /// The cache must read that build output, so a fixture that skips the build would not test the
    /// cache input.
    fn built(test_name: &str) -> (PathBuf, TempDbGuard) {
        let dir = std::env::temp_dir().join("chibipop_windows_media_test");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join(format!("t_{}_{test_name}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let guard = TempDbGuard(out.clone());
        let archive = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/media/media.zip");
        chibipop::dict::build::build(&[archive], &[], &out, &|_| {}).expect("the fixture builds");
        (out, guard)
    }

    /// The asset's original pixels. All but 15 of the census's 72 dictionaries use this form.
    const AS_DRAWN: (Tint, Rgb) = (Tint::None, (0, 0, 0));

    #[test]
    fn a_stored_png_arrives_as_premultiplied_bgra() {
        // The fixture is 12x7 of pure blue at half alpha.
        // Premultiply gives (0, 0, 128, 128) in RGBA.
        // BGRA puts 128 first. If the channel swap is absent, every gaiji would have the wrong hue.
        let (db, _guard) = built("bgra");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");
        let bitmap = cache
            .bitmap(&MediaKey::new(1, "gaiji/one.png"), AS_DRAWN.0, AS_DRAWN.1)
            .expect("it paints");

        assert_eq!((12, 7), (bitmap.w, bitmap.h));
        assert_eq!(48, bitmap.stride());
        assert_eq!(12 * 7 * 4, bitmap.bgra.len());
        for px in bitmap.bgra.as_chunks::<4>().0 {
            assert_eq!(&[128, 0, 0, 128], px);
        }
    }

    #[test]
    fn a_second_ask_answers_from_the_cache() {
        let (db, _guard) = built("cache_hit");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");
        let key = MediaKey::new(1, "gaiji/one.png");

        let first = cache.bitmap(&key, AS_DRAWN.0, AS_DRAWN.1).expect("it paints").clone();
        let held = cache.footprint();
        assert_eq!(12 * 7 * 4, held, "the budget counts decoded pixels");
        assert_eq!(&first, cache.bitmap(&key, AS_DRAWN.0, AS_DRAWN.1).expect("still paints"));
        assert_eq!(held, cache.footprint(), "a hit admits nothing");
    }

    /// Every format that the census found now decodes into pixels.
    /// The cache therefore stores a decoded bitmap instead of a refusal.
    /// AVIF and SVG matter most. They account for 201 and 35 of this library's 236 assets.
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
            let bitmap = cache
                .bitmap(&key, AS_DRAWN.0, AS_DRAWN.1)
                .unwrap_or_else(|e| panic!("{path}: {e}"));
            assert_eq!((w, h), (bitmap.w, bitmap.h), "{path}");
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
                cache.bitmap(&key, AS_DRAWN.0, AS_DRAWN.1).map(|_| ()),
                "{path}"
            );
            // The cache stores a permanent answer. It must not query SQLite after the first
            // `paint_once`. A refusal consumes no pixels.
            assert!(cache.bitmap(&key, AS_DRAWN.0, AS_DRAWN.1).is_err(), "{path}");
        }
        assert_eq!(0, cache.footprint(), "a refusal costs no pixels");
    }

    #[test]
    fn the_budget_evicts_the_oldest_and_never_the_entry_just_admitted() {
        // The budget holds one surface.
        // The second admission must evict the first, but an asset larger than the full budget must
        // stay. Without this rule, the caller sees an asset as absent even though the cache just
        // served it.
        let (db, _guard) = built("evicts");
        let mut cache = MediaSurfaces::with_budget(&db, 12 * 7 * 4).expect("the store opens");
        let one = MediaKey::new(1, "gaiji/one.png");
        let copy = MediaKey::new(1, "gaiji/copy.png");

        assert!(cache.bitmap(&one, AS_DRAWN.0, AS_DRAWN.1).is_ok());
        assert!(cache.bitmap(&copy, AS_DRAWN.0, AS_DRAWN.1).is_ok());
        assert_eq!(12 * 7 * 4, cache.footprint(), "the budget is honoured");
        assert!(!cache.slots.contains_key(&(one.clone(), None, None)), "the oldest went");
        assert!(cache.slots.contains_key(&(copy, None, None)), "the newest stayed");

        let mut tight = MediaSurfaces::with_budget(&db, 1).expect("the store opens");
        assert!(
            tight.bitmap(&one, AS_DRAWN.0, AS_DRAWN.1).is_ok(),
            "an oversized asset is still served"
        );
    }

    /// Verifies the monochrome tint at the level that the tint can reach.
    /// A monochrome asset uses its alpha as coverage for the body text color.
    /// A gaiji authored as black ink stays legible on dark and light themes.
    ///
    /// The fixture uses half-alpha blue, so this assertion also checks channel order.
    /// A tint that omits the swap would place red where `CreateBitmap` reads blue.
    #[test]
    fn a_monochrome_asset_is_tinted_to_the_text_colour_in_bgra_order() {
        let (db, _guard) = built("tint");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");
        let key = MediaKey::new(1, "gaiji/one.png");

        // Use distinct values in all three channels so a swap is visible.
        let ink = (200u8, 100u8, 50u8);
        let tinted = cache.bitmap(&key, Tint::Alpha, ink).expect("it paints").clone();
        // Alpha is 128, so each channel is `channel * 128 / 255` rounded.
        let at = |c: u8| ((u32::from(c) * 128 + 127) / 255) as u8;
        for px in tinted.bgra.as_chunks::<4>().0 {
            assert_eq!(&[at(ink.2), at(ink.1), at(ink.0), 128], px, "b, g, r, a");
            assert!(px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3], "premultiplied");
        }
        // The cache needs two slots. The asset as drawn does not meet a mask's needs, and a theme
        // change must not silently retain the old color.
        assert!(cache.bitmap(&key, Tint::None, (0, 0, 0)).is_ok());
        assert_eq!(2 * 12 * 7 * 4, cache.footprint());
    }

    /// Applies the same rule to the assets that use it.
    /// A monochrome SVG is a mask with no pixels until a caller picks a size.
    /// `SceneImage::tint` gets that size from the resolved box, and this function creates the
    /// bitmap.
    /// All 35 of this library's gaiji have this form: `appearance: "monochrome"`, `height: 1em`,
    /// and an SVG glyph outline.
    /// The theme's body text color makes them legible on dark and light panels.
    #[test]
    fn a_monochrome_vector_rasterizes_at_the_size_the_tint_asked_for() {
        let (db, _guard) = built("vector");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");
        let svg = MediaKey::new(1, "gaiji/two.svg");

        // The dark and light theme colors differ in all three channels. A missed swap is therefore
        // visible.
        for ink in [(210u8, 212u8, 218u8), (40, 42, 48)] {
            let mask = cache.bitmap(&svg, Tint::Raster(30, 30), ink).expect("it paints").clone();
            assert_eq!((30, 30), (mask.w, mask.h), "the tint's own size");
            let mut inked = 0;
            for px in mask.bgra.as_chunks::<4>().0 {
                let a = u32::from(px[3]);
                let at = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
                assert_eq!(&[at(ink.2), at(ink.1), at(ink.0), px[3]], px, "b, g, r, a");
                assert!(px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3], "premultiplied");
                inked += usize::from(px[3] > 0);
            }
            assert!(inked > 0, "a mask with no coverage would tint nothing");
        }
        // Two colors need two bitmaps. A theme change must not silently retain the old color.
        assert_eq!(2 * 30 * 30 * 4, cache.footprint(), "one mask per theme");

        // A second size needs a second mask because a vector's pixels depend on the requested size,
        // not its bytes.
        cache.bitmap(&svg, Tint::Raster(12, 12), (40, 42, 48)).expect("it paints");
        assert_eq!(2 * 30 * 30 * 4 + 12 * 12 * 4, cache.footprint(), "two sizes, two bitmaps");

        // An untinted vector still rasterizes at its intrinsic size, the size that the media row
        // records.
        let plain = cache.bitmap(&svg, Tint::None, (0, 0, 0)).expect("it paints");
        assert_eq!((64, 32), (plain.w, plain.h));
    }
}
