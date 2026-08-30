//! The popup's decoded-surface cache (ADR-0004's paint side).
//!
//! One `MediaStore` connection of its own plus a bounded map of decoded
//! bitmaps. Its own connection because the worker owns the dictionary on
//! its own thread and a `rusqlite::Connection` is not `Sync`; the painter
//! wants nothing the dictionary carries anyway - not the term index, not
//! the parsed-tree cache.
//!
//! What it buys is that a hover over 字通, which averages more than four
//! image nodes per term row, decodes each distinct gaiji once for the
//! session instead of once per `paint_once`. Every refusal is cached too:
//! an asset this build cannot rasterize, or one the archive never shipped,
//! is a permanent answer, and re-asking SQLite for it per frame would be
//! the same waste with none of the pixels.
//!
//! The Linux twin is `crates/chibipop-linux/src/popup/media.rs`, in the
//! same relation as [`super::render`]'s `paint_once` and that adapter's
//! `paint::panel`. The two differ in exactly one thing, and it is not the
//! bookkeeping: core hands out premultiplied RGBA8, which is tiny-skia's
//! own pixmap layout, so the Linux side stores what it is given and this
//! side swaps two channels into the one format `CreateBitmap` takes.
//!
//! What this module deliberately does *not* hold is an `ID2D1Bitmap`. A
//! device bitmap belongs to a render target and dies with it on device
//! loss; these bytes outlive that, and ticket 12 uploads them.

use chibipop::dict::media::{MediaKey, Missing, Surface};
use chibipop::lookup::sqlite::MediaStore;
use chibipop::ui::layout::{Rgb, Tint};
use std::collections::{HashMap, VecDeque};
use std::path::Path;

/// Decoded pixels the cache holds before it starts evicting, in bytes.
///
/// The census's image nodes are gaiji at `height: 1em`, so a decoded asset
/// is a few kilobytes and this holds thousands of them - every distinct
/// glyph a session of hovering a kanji dictionary touches. It is a budget
/// and not a count because one illustration can be worth a thousand gaiji.
const BUDGET: usize = 8 << 20;

/// One decoded asset in the only pixel layout `ID2D1RenderTarget::
/// CreateBitmap` takes without a WIC converter:
/// `DXGI_FORMAT_B8G8R8A8_UNORM` with `D2D1_ALPHA_MODE_PREMULTIPLIED`,
/// top-down, no row padding.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Bitmap {
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
}

impl Bitmap {
    /// Bytes per row, which is `CreateBitmap`'s `pitch`. No padding: the
    /// buffer is exactly `w * h * 4`.
    pub fn stride(&self) -> u32 {
        self.w * 4
    }
}

/// One cache slot: the asset, and the colour it was tinted with.
///
/// The colour is part of the key because a monochrome asset is a *mask*:
/// two nodes can share one blob and want two different bitmaps, and a theme
/// change wants a third. `None` is the untinted asset, which is what all
/// but 15 of the census's 72 dictionaries ask for.
type Slot = (MediaKey, Option<Rgb>);

/// Decoded assets, keyed by media key.
///
/// Insertion order, not recency, for the same reason as the parsed-tree
/// cache in `lookup::sqlite`: a hover is a sliding window over recent
/// content, so the two orders nearly coincide and FIFO costs one push per
/// miss instead of a touch per hit.
pub struct MediaSurfaces {
    store: MediaStore,
    slots: HashMap<Slot, Result<Bitmap, Missing>>,
    order: VecDeque<Slot>,
    bytes: usize,
    budget: usize,
    notes: Vec<String>,
}

impl MediaSurfaces {
    /// Opens the media store beside the dictionary.
    ///
    /// There is no `invalidate`: a rebuild replaces the database file and
    /// this read-only connection keeps the old one, so a reload replaces
    /// the whole cache rather than clearing it.
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

    /// The bitmap for one asset, tinted as the scene asked.
    ///
    /// `Err` is not a failure to handle - it is the `alt`-text fallback's
    /// cue, and it says which of the three reasons applies so a diagnostic
    /// can be honest.
    ///
    /// The tint is applied once, on the way into the cache, rather than per
    /// frame: `paint_once` runs on every damage and a mask's whole bitmap
    /// would be rewritten each time.
    pub fn bitmap(
        &mut self,
        key: &MediaKey,
        tint: Tint,
        color: Rgb,
    ) -> Result<&Bitmap, Missing> {
        let slot: Slot = (key.clone(), (tint != Tint::None).then_some(color));
        if !self.slots.contains_key(&slot) {
            let mut pixels = self.store.surface(key).map(to_bgra);
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

    /// Diagnostics since the last drain, for the app's log.
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

    fn admit(&mut self, slot: Slot, pixels: Result<Bitmap, Missing>) {
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

/// Premultiplied RGBA8 to premultiplied BGRA8, in place.
///
/// Two channels per pixel, over the buffer core already allocated: no
/// second copy of the asset, and no per-frame work - a bitmap is converted
/// once and cached in the form the device wants.
fn to_bgra(mut surface: Surface) -> Bitmap {
    for px in surface.rgba.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    Bitmap { w: surface.w, h: surface.h, bgra: surface.rgba }
}

/// A mask's pixels, in the colour it stands in for.
///
/// Premultiplied BGRA8 in and out: the asset's own colour is discarded and
/// its alpha becomes the coverage of `color`, which is what a monochrome
/// asset *is* - black ink authored for a light page, and invisible on a
/// dark panel if composited as it was drawn.
///
/// Multiplying rather than filtering, because chibipop has no filter stack:
/// Yomitan's and Hoshi Reader's `brightness(0) invert(1)` shortcut needs
/// one, and it also cannot reach a colour that is neither black nor white -
/// which the theme's body text usually is.
///
/// The Linux twin is `chibipop_linux::media::tint_alpha` and differs in
/// exactly the two channel indices, for the reason this module's header
/// gives: core hands out RGBA and `CreateBitmap` takes BGRA.
fn tint_alpha(bgra: &mut [u8], color: Rgb) {
    let (r, g, b) = (u32::from(color.0), u32::from(color.1), u32::from(color.2));
    for px in bgra.as_chunks_mut::<4>().0 {
        // Premultiplied, so every channel is already scaled by the alpha
        // and the new ones have to be scaled the same way - which is also
        // what keeps `b <= a`, the invariant
        // `D2D1_ALPHA_MODE_PREMULTIPLIED` blends under.
        let a = u32::from(px[3]);
        px[0] = ((b * a + 127) / 255) as u8;
        px[1] = ((g * a + 127) / 255) as u8;
        px[2] = ((r * a + 127) / 255) as u8;
    }
}

/// A slot's retained pixels. A refusal holds none.
fn weight(slot: &Result<Bitmap, Missing>) -> usize {
    slot.as_ref().map_or(0, |b| b.bgra.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chibipop::dict::media::{MediaFormat, Undecodable};
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

    /// The asset as it was drawn, which is what all but 15 of the census's
    /// 72 dictionaries ask for.
    const AS_DRAWN: (Tint, Rgb) = (Tint::None, (0, 0, 0));

    #[test]
    fn a_stored_png_arrives_as_premultiplied_bgra() {
        // The fixture is 12x7 of pure blue at half alpha. Premultiplied it
        // is (0, 0, 128, 128) in RGBA, so BGRA puts the 128 first - and a
        // channel swap that never happened would paint every gaiji in the
        // wrong hue.
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

    #[test]
    fn a_format_this_build_cannot_rasterize_is_a_cached_refusal_and_holds_no_pixels() {
        let (db, _guard) = built("no_decoder");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");

        let avif = MediaKey::new(1, "gaiji/five.avif");
        assert_eq!(
            Err(Missing::Undecodable(Undecodable::NoDecoder(MediaFormat::Avif))),
            cache.bitmap(&avif, AS_DRAWN.0, AS_DRAWN.1).map(|_| ()),
            "the ladder needs to know it is the format and not the asset",
        );
        assert_eq!(0, cache.footprint(), "a refusal costs no pixels");
        assert!(cache.bitmap(&avif, AS_DRAWN.0, AS_DRAWN.1).is_err(), "and it is remembered");
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
        }
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

        assert!(cache.bitmap(&one, AS_DRAWN.0, AS_DRAWN.1).is_ok());
        assert!(cache.bitmap(&copy, AS_DRAWN.0, AS_DRAWN.1).is_ok());
        assert_eq!(12 * 7 * 4, cache.footprint(), "the budget is honoured");
        assert!(!cache.slots.contains_key(&(one.clone(), None)), "the oldest went");
        assert!(cache.slots.contains_key(&(copy, None)), "the newest stayed");

        let mut tight = MediaSurfaces::with_budget(&db, 1).expect("the store opens");
        assert!(
            tight.bitmap(&one, AS_DRAWN.0, AS_DRAWN.1).is_ok(),
            "an oversized asset is still served"
        );
    }

    /// Story 17, at the level the tint is reachable: a monochrome asset's
    /// alpha becomes the coverage of the body text colour, so a gaiji
    /// authored as black ink is legible on a dark theme and on a light one.
    ///
    /// The fixture is half-alpha blue, so the assertion is also the channel
    /// order: a tint that forgot the swap would put the red channel where
    /// `CreateBitmap` reads blue.
    #[test]
    fn a_monochrome_asset_is_tinted_to_the_text_colour_in_bgra_order() {
        let (db, _guard) = built("tint");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");
        let key = MediaKey::new(1, "gaiji/one.png");

        // Distinct in all three channels, so a swapped one is visible.
        let ink = (200u8, 100u8, 50u8);
        let tinted = cache.bitmap(&key, Tint::Alpha, ink).expect("it paints").clone();
        // Alpha 128, so each channel is `channel * 128 / 255` rounded.
        let at = |c: u8| ((u32::from(c) * 128 + 127) / 255) as u8;
        for px in tinted.bgra.as_chunks::<4>().0 {
            assert_eq!(&[at(ink.2), at(ink.1), at(ink.0), 128], px, "b, g, r, a");
            assert!(px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3], "premultiplied");
        }
        // Two slots: the asset as drawn is not what a mask wants, and a
        // theme change must not silently keep the old ink.
        assert!(cache.bitmap(&key, Tint::None, (0, 0, 0)).is_ok());
        assert_eq!(2 * 12 * 7 * 4, cache.footprint());
    }

    /// `Tint::Raster` means the same thing to this cache as `Tint::Alpha`
    /// until an SVG rasterizer exists: `dict::media::decode` answers
    /// `NoDecoder` for every vector, so the `alt` ladder takes over.
    #[test]
    fn a_vector_has_no_pixels_to_tint_in_this_build() {
        let (db, _guard) = built("vector");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");
        let svg = MediaKey::new(1, "gaiji/two.svg");

        assert_eq!(
            Err(Missing::Undecodable(Undecodable::NoDecoder(MediaFormat::Svg))),
            cache.bitmap(&svg, Tint::Raster(30, 30), (230, 230, 230)).map(|_| ()),
        );
        assert_eq!(0, cache.footprint());
    }
}
