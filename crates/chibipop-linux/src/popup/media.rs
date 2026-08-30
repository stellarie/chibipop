//! The popup's decoded-surface cache (ADR-0004's paint side).
//!
//! One `MediaStore` connection of its own plus a bounded map of decoded
//! pixmaps. Its own connection because the worker owns the dictionary on
//! its own thread and a `rusqlite::Connection` is not `Sync`; the painter
//! wants nothing the dictionary carries anyway - not the term index, not
//! the parsed-tree cache.
//!
//! What it buys is that a hover over 字通, which averages more than four
//! image nodes per term row, decodes each distinct gaiji once for the
//! session instead of once per frame. Frame gating already coalesces
//! commits (`super::surface`), so without a cache a scroll would re-decode
//! the same forty PNGs at the refresh rate.
//!
//! Every refusal is cached too. An asset this build cannot rasterize, or
//! one the archive never shipped, is a permanent answer, and re-asking
//! SQLite for it once per frame would be the same waste with none of the
//! pixels.
//!
//! The Windows twin is `crates/chibipop-windows/src/ui/media.rs`. The two
//! differ in exactly one thing, and it is not the bookkeeping: tiny-skia's
//! pixmap is premultiplied RGBA8, which is core's own surface layout, so
//! this side stores what core hands it and Direct2D's side swaps two
//! channels.

use chibipop::dict::media::{MediaKey, Missing};
use chibipop::lookup::sqlite::MediaStore;
use chibipop::ui::layout::{Rgb, Tint};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use tiny_skia::{IntSize, Pixmap};

/// Decoded pixels the cache holds before it starts evicting, in bytes.
///
/// The census's image nodes are gaiji at `height: 1em`, so a decoded asset
/// is a few kilobytes and this holds thousands of them - every distinct
/// glyph a session of hovering a kanji dictionary touches. It is a budget
/// and not a count because one illustration can be worth a thousand gaiji.
const BUDGET: usize = 8 << 20;

/// One cache slot: the asset, the colour it was tinted with, and the pixel
/// size a vector was rasterized at.
///
/// The colour is part of the key because a monochrome asset is a *mask*:
/// two nodes can share one blob and want two different pixmaps, and a theme
/// change wants a third. `None` is the untinted asset, which is what all but
/// 15 of the census's 72 dictionaries ask for.
///
/// The raster size is part of it for the same reason one axis further out:
/// `Tint::Raster` exists because a vector has no pixels until something
/// picks a size, so one SVG at two font sizes is two masks. `None` is a
/// raster asset, or a vector at its own intrinsic size.
type Slot = (MediaKey, Option<Rgb>, Option<(u32, u32)>);

/// Decoded assets, keyed by media key.
///
/// Insertion order, not recency, for the same reason as the parsed-tree
/// cache in `lookup::sqlite`: a hover is a sliding window over recent
/// content, so the two orders nearly coincide and FIFO costs one push per
/// miss instead of a touch per hit.
pub struct MediaSurfaces {
    store: MediaStore,
    slots: HashMap<Slot, Result<Pixmap, Missing>>,
    order: VecDeque<Slot>,
    bytes: usize,
    budget: usize,
    notes: Vec<String>,
}

impl MediaSurfaces {
    /// Opens the media store beside the dictionary.
    ///
    /// There is no `invalidate`: a rebuild renames a new database over the
    /// old path and this read-only connection keeps the old inode, so a
    /// reload replaces the whole cache rather than clearing it.
    pub fn open(db: &Path) -> anyhow::Result<MediaSurfaces> {
        MediaSurfaces::with_budget(db, BUDGET)
    }

    fn with_budget(db: &Path, budget: usize) -> anyhow::Result<MediaSurfaces> {
        Ok(MediaSurfaces {
            store: MediaStore::open(db)?,
            slots: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            budget,
            notes: Vec::new(),
        })
    }

    /// The pixmap for one asset, tinted as the scene asked.
    ///
    /// `Err` is not a failure to handle - it is the `alt`-text fallback's
    /// cue, and it says which of the three reasons applies so a diagnostic
    /// can be honest.
    ///
    /// The tint is applied once, on the way into the cache, rather than per
    /// frame: a scroll re-paints the same asset at the refresh rate and a
    /// mask's whole surface would be rewritten each time. `Tint::Raster`
    /// also *is* the rasterization size - core quantised the resolved box
    /// so both bins ask their decoder for the same pixels - so it is read
    /// out here and handed to the store rather than dropped.
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
            // Unreachable: `admit` never evicts the entry it just added.
            None => Err(Missing::NotStored),
        }
    }

    /// Diagnostics since the last drain, for the daemon's log.
    ///
    /// A real store fault is worth one line; a dictionary shipping an AVIF
    /// this build cannot rasterize is not, and neither is repeated - the
    /// cache answers from its own map after the first ask.
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// Decoded pixels currently held.
    pub fn footprint(&self) -> usize {
        self.bytes
    }

    fn load(&self, key: &MediaKey, at: Option<(u32, u32)>) -> Result<Pixmap, Missing> {
        let surface = self.store.surface(key, at)?;
        let (w, h) = (surface.w, surface.h);
        IntSize::from_wh(w, h)
            .and_then(|size| Pixmap::from_vec(surface.rgba, size))
            // Core validated `w * h * 4` on decode, so the only way here
            // is a dimension tiny-skia itself refuses.
            .ok_or_else(|| {
                Missing::Unavailable(format!("media {key}: tiny-skia refused {w}x{h}"))
            })
    }

    fn admit(&mut self, slot: Slot, pixels: Result<Pixmap, Missing>) {
        // Only a real store fault is worth a line. A dictionary shipping
        // an AVIF is not, and it would repeat per key per session.
        if let Err(Missing::Unavailable(why)) = &pixels {
            self.notes.push(why.clone());
        }
        self.bytes += weight(&pixels);
        self.order.push_back(slot.clone());
        self.slots.insert(slot, pixels);
        // `len() > 1` keeps a single oversized asset: evicting the entry
        // just admitted would report an asset the user can see as absent,
        // and decode it again on the next frame.
        while self.bytes > self.budget && self.order.len() > 1 {
            let Some(evicted) = self.order.pop_front() else { break };
            if let Some(slot) = self.slots.remove(&evicted) {
                self.bytes -= weight(&slot);
            }
        }
    }
}

/// A mask's pixels, in the colour it stands in for.
///
/// Premultiplied RGBA8 in, premultiplied RGBA8 out: the asset's own colour
/// is discarded and its alpha becomes the coverage of `color`, which is
/// what a monochrome asset *is* - black ink authored for a light page, and
/// invisible on a dark panel if composited as it was drawn.
///
/// Multiplying rather than filtering, because chibipop has no filter stack:
/// Yomitan's and Hoshi Reader's `brightness(0) invert(1)` shortcut needs
/// one, and it also cannot reach a colour that is neither black nor white -
/// which the theme's body text usually is.
fn tint_alpha(rgba: &mut [u8], color: Rgb) {
    let (r, g, b) = (u32::from(color.0), u32::from(color.1), u32::from(color.2));
    for px in rgba.as_chunks_mut::<4>().0 {
        // Premultiplied, so every channel is already scaled by the alpha
        // and the new ones have to be scaled the same way - which is also
        // what keeps `r <= a`, the invariant tiny-skia's own pixel type
        // enforces.
        let a = u32::from(px[3]);
        px[0] = ((r * a + 127) / 255) as u8;
        px[1] = ((g * a + 127) / 255) as u8;
        px[2] = ((b * a + 127) / 255) as u8;
    }
}

/// A slot's retained pixels. A refusal holds none.
fn weight(slot: &Result<Pixmap, Missing>) -> usize {
    slot.as_ref().map_or(0, |p| p.data().len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Removes the file on drop, as every other database test here does.
    struct TempDbGuard(PathBuf);

    impl Drop for TempDbGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// A database built from the committed media fixture archive.
    ///
    /// The real builder, not a hand-written schema: the cache's whole job
    /// is to read what a build wrote, so a fixture that skipped the build
    /// would be testing the test.
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

    /// The asset as it was drawn, which is what all but 15 of the census's
    /// 72 dictionaries ask for.
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

        // Premultiplied, and identical on the second ask: nothing re-reads
        // and nothing re-decodes.
        let second = cache.surface(&key, AS_DRAWN.0, AS_DRAWN.1).expect("still paints");
        assert_eq!(first.data(), second.data());
        assert_eq!(held, cache.footprint(), "a hit admits nothing");
    }

    /// Every format the census found now reaches pixels, so the cache's
    /// subject is a decoded surface rather than a refusal. AVIF and SVG are
    /// the two that matter: 201 and 35 of this library's 236 assets.
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
            // Cached: a permanent answer must not go back to SQLite once
            // per frame, and it costs no pixels.
            assert!(cache.surface(&key, AS_DRAWN.0, AS_DRAWN.1).is_err(), "{path}");
        }
        assert_eq!(0, cache.footprint(), "a refusal costs no pixels");
    }

    #[test]
    fn two_paths_sharing_one_blob_still_decode_to_their_own_surfaces() {
        // The dedup is in the store, not in the painter: a monochrome asset
        // is tinted to the body text colour, so two nodes drawing one blob
        // can want two different pixmaps.
        let (db, _guard) = built("shared_blob");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");
        assert!(cache.surface(&MediaKey::new(1, "gaiji/one.png"), Tint::None, (0, 0, 0)).is_ok());
        assert!(cache.surface(&MediaKey::new(1, "gaiji/copy.png"), Tint::None, (0, 0, 0)).is_ok());
        assert_eq!(2 * 12 * 7 * 4, cache.footprint(), "two paths, two surfaces");
    }

    #[test]
    fn the_budget_evicts_the_oldest_and_never_the_entry_just_admitted() {
        // One surface's worth of budget, so admitting the second must drop
        // the first - and admitting one *larger* than the whole budget must
        // still keep it, or the caller is told an asset it can see is
        // absent.
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

    /// Story 17, at the level the tint is reachable: a monochrome asset's
    /// alpha becomes the coverage of the body text colour, so a gaiji
    /// authored as black ink is legible on a dark theme and on a light one.
    ///
    /// Asserted on real decoded pixels from the real builder, because the
    /// premultiplication invariant is the thing that would break: a tinted
    /// channel above its own alpha is a pixel tiny-skia refuses to blend.
    #[test]
    fn a_monochrome_asset_is_tinted_to_the_text_colour_and_stays_premultiplied() {
        let (db, _guard) = built("tint");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");
        let key = MediaKey::new(1, "gaiji/one.png");

        let plain = cache.surface(&key, Tint::None, (0, 0, 0)).expect("it paints").clone();
        // The dark theme's body text, and the light theme's.
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
        // Three slots, not one: the asset as drawn plus one per colour, so
        // a theme change does not silently keep the old ink.
        assert_eq!(3 * 12 * 7 * 4, cache.footprint());
    }

    /// Story 17 on the assets story 17 is about: a monochrome SVG is a mask
    /// with no pixels until something picks a size, `SceneImage::tint`
    /// picks it out of the resolved box, and this is where that number
    /// becomes a pixmap. All 35 of this library's gaiji are exactly this
    /// shape - `appearance: "monochrome"`, `height: 1em`, an SVG glyph
    /// outline - and the theme's body text colour is what makes them
    /// legible on a dark panel and on a light one.
    #[test]
    fn a_monochrome_vector_rasterizes_at_the_size_the_tint_asked_for() {
        let (db, _guard) = built("vector");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");
        let svg = MediaKey::new(1, "gaiji/two.svg");

        // The dark theme's body text, and the light theme's. A mask
        // composited as drawn would be one of them and invisible on the
        // other; the tint is what makes it both.
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
        // Two colours, two pixmaps: a theme change must not silently keep
        // the old ink.
        assert_eq!(2 * 30 * 30 * 4, cache.footprint(), "one mask per theme");

        // A second size is a second mask too, because a vector's pixels are
        // the size somebody asked for - not a property of its bytes.
        cache.surface(&svg, Tint::Raster(12, 12), (40, 42, 48)).expect("it paints");
        assert_eq!(2 * 30 * 30 * 4 + 12 * 12 * 4, cache.footprint(), "two sizes, two pixmaps");

        // And an untinted vector still rasterizes, at its own intrinsic
        // size, which is the size the media row recorded.
        let plain = cache.surface(&svg, Tint::None, (0, 0, 0)).expect("it paints");
        assert_eq!((64, 32), (plain.width(), plain.height()));
    }
}
