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

/// Decoded assets, keyed by media key.
///
/// Insertion order, not recency, for the same reason as the parsed-tree
/// cache in `lookup::sqlite`: a hover is a sliding window over recent
/// content, so the two orders nearly coincide and FIFO costs one push per
/// miss instead of a touch per hit.
pub struct MediaSurfaces {
    store: MediaStore,
    slots: HashMap<MediaKey, Result<Pixmap, Missing>>,
    order: VecDeque<MediaKey>,
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

    /// The pixmap for one asset, decoding it on the first ask.
    ///
    /// `Err` is not a failure to handle - it is the `alt`-text fallback's
    /// cue, and it says which of the three reasons applies so a diagnostic
    /// can be honest.
    pub fn surface(&mut self, key: &MediaKey) -> Result<&Pixmap, Missing> {
        if !self.slots.contains_key(key) {
            let slot = self.load(key);
            self.admit(key.clone(), slot);
        }
        match self.slots.get(key) {
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

    fn load(&self, key: &MediaKey) -> Result<Pixmap, Missing> {
        let surface = self.store.surface(key)?;
        let (w, h) = (surface.w, surface.h);
        IntSize::from_wh(w, h)
            .and_then(|size| Pixmap::from_vec(surface.rgba, size))
            // Core validated `w * h * 4` on decode, so the only way here
            // is a dimension tiny-skia itself refuses.
            .ok_or_else(|| {
                Missing::Unavailable(format!("media {key}: tiny-skia refused {w}x{h}"))
            })
    }

    fn admit(&mut self, key: MediaKey, slot: Result<Pixmap, Missing>) {
        // Only a real store fault is worth a line. A dictionary shipping
        // an AVIF is not, and it would repeat per key per session.
        if let Err(Missing::Unavailable(why)) = &slot {
            self.notes.push(why.clone());
        }
        self.bytes += weight(&slot);
        self.order.push_back(key.clone());
        self.slots.insert(key, slot);
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

/// A slot's retained pixels. A refusal holds none.
fn weight(slot: &Result<Pixmap, Missing>) -> usize {
    slot.as_ref().map_or(0, |p| p.data().len())
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

    #[test]
    fn a_stored_png_decodes_once_and_answers_from_the_cache_after_that() {
        let (db, _guard) = built("cache_hit");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");
        let key = MediaKey::new(1, "gaiji/one.png");

        let first = cache.surface(&key).expect("a stored PNG paints").clone();
        assert_eq!((12, 7), (first.width(), first.height()));
        let held = cache.footprint();
        assert_eq!(12 * 7 * 4, held, "the budget counts decoded pixels");

        // Premultiplied, and identical on the second ask: nothing re-reads
        // and nothing re-decodes.
        let second = cache.surface(&key).expect("still paints");
        assert_eq!(first.data(), second.data());
        assert_eq!(held, cache.footprint(), "a hit admits nothing");
    }

    #[test]
    fn a_format_this_build_cannot_rasterize_is_a_cached_refusal_and_holds_no_pixels() {
        let (db, _guard) = built("no_decoder");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");

        let avif = MediaKey::new(1, "gaiji/five.avif");
        assert_eq!(
            Err(Missing::Undecodable(Undecodable::NoDecoder(MediaFormat::Avif))),
            cache.surface(&avif).map(|_| ()),
            "the ladder needs to know it is the format and not the asset",
        );
        assert_eq!(0, cache.footprint(), "a refusal costs no pixels");
        // Cached: the second ask must not go back to SQLite.
        assert!(cache.surface(&avif).is_err());
    }

    #[test]
    fn an_asset_the_build_stored_no_row_for_is_not_stored() {
        let (db, _guard) = built("not_stored");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");
        for path in ["gaiji/missing.png", "gaiji/broken.png", "gaiji/unused.png"] {
            let key = MediaKey::new(1, path);
            assert_eq!(
                Err(Missing::NotStored),
                cache.surface(&key).map(|_| ()),
                "{path}",
            );
        }
    }

    #[test]
    fn two_paths_sharing_one_blob_still_decode_to_their_own_surfaces() {
        // The dedup is in the store, not in the painter: ticket 12 tints a
        // monochrome asset to the body text colour, so two nodes drawing
        // one blob can want two different pixmaps.
        let (db, _guard) = built("shared_blob");
        let mut cache = MediaSurfaces::open(&db).expect("the store opens");
        assert!(cache.surface(&MediaKey::new(1, "gaiji/one.png")).is_ok());
        assert!(cache.surface(&MediaKey::new(1, "gaiji/copy.png")).is_ok());
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

        assert!(cache.surface(&one).is_ok());
        assert!(cache.surface(&copy).is_ok());
        assert_eq!(12 * 7 * 4, cache.footprint(), "the budget is honoured");
        assert!(!cache.slots.contains_key(&one), "the oldest went");
        assert!(cache.slots.contains_key(&copy), "the newest stayed");

        let mut tight = MediaSurfaces::with_budget(&db, 1).expect("the store opens");
        assert!(tight.surface(&one).is_ok(), "an oversized asset is still served");
    }
}
