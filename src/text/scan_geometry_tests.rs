//! Shape contracts for the scan rects that `TextSource` reads and the overlay draws.
//!
//! Issue #92 showed boxes "all over the place" over one short line. The `Scripted`
//! fixture in `source.rs` answers a grab by its frame shape. It cannot place a cursor at
//! many positions on lines of many lengths. This module builds a virtual screen of glyph
//! boxes instead. `PageCapture` records every grab. `PageOcr` returns the glyphs inside
//! the grab with the box that the grab edge leaves. A glyph that the edge cuts by more
//! than half vanishes, as a real engine drops a sliver.
//!
//! Two layers use the page. The exact-vector tests derive every rect by hand from the
//! rules in `layout.rs`. The placement sweep puts the cursor at several positions on
//! every glyph of every page and asserts shape invariants on the scan vector.
//!
//! The fixture runs at upscale 1 with `discard_furigana` off. A kana row that a box edge
//! cuts to a third of its height satisfies `is_ruby_pair` beside a kanji row and would
//! vanish. The ruby rule has its own tests.

use crate::geom::{PhysPoint, PhysRect, ScanKind, ScanRect};
use crate::present::{match_highlight, Card, HIGHLIGHT_PAD};
use crate::text::layout::{hit_scan, CaptureSize, OcrLine, OcrWord, Orientation, Resolved};
use crate::text::{CaptureMask, Frame, OcrEngine, RegionCapture, SettingsSnapshot, TextSource};
use anyhow::Result;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Every glyph is `S x S` pixels.
const S: i32 = 40;
/// The single output. Every page uses it.
const BOUNDS: PhysRect = PhysRect { x: 0, y: 0, w: 1920, h: 1080 };

/// One glyph on the virtual screen.
struct Glyph {
    ch: char,
    rect: PhysRect,
    line: usize,
}

struct Page {
    name: &'static str,
    glyphs: Vec<Glyph>,
    /// The orientation of each line index, for the orientation invariant.
    lines: Vec<Orientation>,
    bounds: PhysRect,
}

impl Page {
    fn new(name: &'static str) -> Self {
        Page { name, glyphs: Vec::new(), lines: Vec::new(), bounds: BOUNDS }
    }

    /// Add one horizontal line. Glyph `i` sits at `x0 + S * i`.
    fn row(&mut self, text: &str, x0: i32, y0: i32) {
        let line = self.lines.len();
        self.lines.push(Orientation::Horizontal);
        for (i, ch) in text.chars().enumerate() {
            let rect = PhysRect { x: x0 + S * i as i32, y: y0, w: S, h: S };
            self.glyphs.push(Glyph { ch, rect, line });
        }
    }

    /// Add one vertical line. Glyph `i` sits at `y0 + S * i`.
    fn column(&mut self, text: &str, x0: i32, y0: i32) {
        let line = self.lines.len();
        self.lines.push(Orientation::Vertical);
        for (i, ch) in text.chars().enumerate() {
            let rect = PhysRect { x: x0, y: y0 + S * i as i32, w: S, h: S };
            self.glyphs.push(Glyph { ch, rect, line });
        }
    }

    /// Add rows of `cols` glyphs. Row `k` sits at `y0 + pitch * k`.
    fn paragraph(&mut self, text: &str, x0: i32, y0: i32, cols: usize, pitch: i32) {
        let chars: Vec<char> = text.chars().collect();
        for (k, chunk) in chars.chunks(cols).enumerate() {
            let row: String = chunk.iter().collect();
            self.row(&row, x0, y0 + pitch * k as i32);
        }
    }
}

fn area(r: PhysRect) -> i64 {
    i64::from(r.w) * i64::from(r.h)
}

type Grabs = Rc<RefCell<Vec<PhysRect>>>;

/// `PageCapture` records every requested region in order. It never fails, also for a
/// region that lies partly outside the output. `region_around` does not clamp, and the
/// test checks that shape, not the backend.
struct PageCapture {
    page: Rc<Page>,
    grabs: Grabs,
    last: Rc<Cell<PhysRect>>,
}

impl RegionCapture for PageCapture {
    fn grab(&mut self, region: PhysRect) -> Result<Frame> {
        self.grabs.borrow_mut().push(region);
        self.last.set(region);
        Ok(Frame {
            buf: vec![0; (region.w * region.h * 4) as usize],
            w: region.w,
            h: region.h,
            source: "page",
            fallback: None,
            unchanged: false,
        })
    }

    fn bounds_containing(&self, _p: PhysPoint) -> PhysRect {
        self.page.bounds
    }
}

/// `PageOcr` answers with the glyphs that the last grab shows.
///
/// A glyph that the grab edge cuts by less than half comes back with its cut box. A
/// glyph that the edge cuts by more than half vanishes. Each page line index gives one
/// OCR line, in page order.
struct PageOcr {
    page: Rc<Page>,
    last: Rc<Cell<PhysRect>>,
}

impl OcrEngine for PageOcr {
    fn recognise(&self, _bgra: &[u8], w: i32, h: i32) -> Result<Vec<OcrLine>> {
        let seen = self.last.get();
        assert_eq!((w, h), (seen.w, seen.h), "the fixture runs at upscale 1");
        let mut lines: Vec<OcrLine> =
            self.page.lines.iter().map(|_| OcrLine { words: Vec::new() }).collect();
        for glyph in &self.page.glyphs {
            let Some(part) = glyph.rect.intersection(seen) else { continue };
            if 2 * area(part) <= area(glyph.rect) {
                continue;
            }
            lines[glyph.line].words.push(OcrWord {
                text: glyph.ch.to_string(),
                rect: part.translated(-seen.x, -seen.y),
            });
        }
        lines.retain(|line| !line.words.is_empty());
        Ok(lines)
    }

    fn set_language(&mut self, _tag: &str) {}

    fn name(&self) -> &str {
        "page"
    }

    fn provides_geometry(&self) -> bool {
        true
    }
}

#[derive(Clone, Copy)]
struct Settings {
    max_passes: u8,
    prefer_vertical: bool,
}

fn fixture(page: &Rc<Page>, settings: Settings) -> (TextSource, Grabs) {
    let grabs: Grabs = Rc::new(RefCell::new(Vec::new()));
    let last = Rc::new(Cell::new(PhysRect { x: 0, y: 0, w: 0, h: 0 }));
    let capture = PageCapture {
        page: Rc::clone(page),
        grabs: Rc::clone(&grabs),
        last: Rc::clone(&last),
    };
    let ocr = PageOcr { page: Rc::clone(page), last };
    let snapshot = SettingsSnapshot {
        max_passes: settings.max_passes,
        upscale: 1,
        prefer_vertical: settings.prefer_vertical,
        capture: CaptureSize::default(),
        scan_alphanumeric: true,
        discard_furigana: false,
    };
    (TextSource::new(Box::new(capture), Box::new(ocr), snapshot), grabs)
}

type Read = (Option<Resolved>, Vec<ScanRect>, Vec<OcrLine>);

fn read(source: &mut TextSource, cursor: PhysPoint) -> Read {
    source.resolve_at_tiled_scanned(cursor, true, CaptureMask::NONE).expect("read")
}

fn kinds(scan: &[ScanRect]) -> Vec<(ScanKind, PhysRect)> {
    scan.iter().map(|s| (s.kind, s.rect)).collect()
}

fn card(match_len: usize) -> Card {
    Card {
        written: None,
        reading: None,
        pos: vec![],
        inflections: vec![],
        freq: None,
        blocks: vec![],
        match_len,
        pitch: Vec::new(),
    }
}

fn expected_pass1(cursor: PhysPoint, prefer_vertical: bool) -> PhysRect {
    let (w, h) = if prefer_vertical { (100, 500) } else { (500, 100) };
    PhysRect { x: cursor.x - w / 2, y: cursor.y - h / 2, w, h }
}

/// Return the one glyph whose box contains `rect`.
fn glyph_containing(page: &Page, rect: PhysRect) -> Option<&Glyph> {
    let mut found = None;
    for glyph in &page.glyphs {
        let g = glyph.rect;
        let inside = rect.x >= g.x
            && rect.y >= g.y
            && rect.x + rect.w <= g.x + g.w
            && rect.y + rect.h <= g.y + g.h;
        if inside {
            assert!(found.is_none(), "{rect:?} lies inside two glyphs");
            found = Some(glyph);
        }
    }
    found
}

fn r(x: i32, y: i32, w: i32, h: i32) -> PhysRect {
    PhysRect { x, y, w, h }
}

fn pt(x: i32, y: i32) -> PhysPoint {
    PhysPoint { x, y }
}

// Pages. Every row sits at y 480 unless stated. The text has no separator unless
// stated, so `runs_out` is true and a probe or a tile can follow.

/// A short line inside the box. The separator stops the lookup.
fn short_stop() -> Rc<Page> {
    let mut page = Page::new("short_stop");
    page.row("日本語を話す。", 800, 480);
    Rc::new(page)
}

/// A short line inside the box. The lookup runs out at the end.
fn short_open() -> Rc<Page> {
    let mut page = Page::new("short_open");
    page.row("日本語を話す", 800, 480);
    Rc::new(page)
}

/// A wrap at the margin with pitch 70.
fn wrap() -> Rc<Page> {
    let mut page = Page::new("wrap");
    page.row("今日は日本語を勉強", 200, 480);
    page.row("しました", 200, 550);
    Rc::new(page)
}

/// Three wrapped rows of 12.
fn paragraph() -> Rc<Page> {
    let mut page = Page::new("paragraph");
    page.paragraph(
        "きょうはにほんごをべんきょうしましたあしたもがんばりたいとおもいますよね",
        200,
        400,
        12,
        70,
    );
    Rc::new(page)
}

/// A line wider than the box. It ends at x 1300 with a separator.
fn long_stop() -> Rc<Page> {
    let mut page = Page::new("long_stop");
    page.row("あいうえおかきくけこさしすせそたちつてとなにぬねのはひふへ。", 100, 480);
    Rc::new(page)
}

/// A line that ends at x 1900, near the output edge.
fn long_edge() -> Rc<Page> {
    let mut page = Page::new("long_edge");
    page.row("あいうえおかきくけこさしすせそたちつてとなにぬねのはひふへほ", 700, 480);
    Rc::new(page)
}

/// A line that the box clips. It ends at x 1200.
fn clipped() -> Rc<Page> {
    let mut page = Page::new("clipped");
    page.row("あいうえおかきくけこさしすせそたちつてと", 400, 480);
    Rc::new(page)
}

/// One column that ends at y 540.
fn column() -> Rc<Page> {
    let mut page = Page::new("column");
    page.column("日本語を話す", 900, 300);
    Rc::new(page)
}

/// A vertical wrap to the column on the left.
fn two_columns() -> Rc<Page> {
    let mut page = Page::new("two_columns");
    page.column("今日は日本語を勉強", 900, 200);
    page.column("しました", 830, 200);
    Rc::new(page)
}

fn pages() -> Vec<Rc<Page>> {
    vec![
        short_stop(),
        short_open(),
        wrap(),
        paragraph(),
        long_stop(),
        long_edge(),
        clipped(),
        column(),
        two_columns(),
    ]
}

// Exact vectors. Every rect below comes from the rules in `layout.rs` by hand.

#[test]
fn a_line_with_a_full_stop_shows_only_the_box_and_the_anchor() {
    let page = short_stop();
    let settings = Settings { max_passes: 1, prefer_vertical: false };
    let (mut source, grabs) = fixture(&page, settings);
    let (resolved, scan, _) = read(&mut source, pt(900, 500));

    assert_eq!(
        kinds(&scan),
        [(ScanKind::Pass1, r(650, 450, 500, 100)), (ScanKind::Anchor, r(880, 480, 40, 40))]
    );
    assert_eq!(*grabs.borrow(), [r(650, 450, 500, 100)]);
    let resolved = resolved.expect("hit");
    assert_eq!(resolved.span.text, "日本語を話す。");
    assert_eq!(resolved.span.cursor_byte_offset, 6);
    assert_eq!(resolved.span.geom.len(), 7);
    for (i, entry) in resolved.span.geom.iter().enumerate() {
        assert_eq!(entry.rect, r(800 + 40 * i as i32, 480, 40, 40));
    }
    assert_eq!(resolved.span.anchor, r(880, 480, 40, 40));
    assert_eq!(resolved.orientation, Orientation::Horizontal);
    // 語を話 plus the pad.
    assert_eq!(match_highlight(&resolved.span, Some(&card(3))), Some(r(877, 477, 126, 46)));
}

/// The probe band is 40 thick: 30 above the row center and 240 below it, at y 470.
/// The probes reach from the margin to the line end plus half a glyph, at x 1060.
/// The first probe is 1000 long. The second starts 500 later and takes the rest.
#[test]
fn a_short_line_without_punctuation_pays_two_probes_from_the_margin() {
    let page = short_open();
    let settings = Settings { max_passes: 1, prefer_vertical: false };
    let (mut source, grabs) = fixture(&page, settings);
    let (resolved, scan, _) = read(&mut source, pt(900, 500));

    assert_eq!(
        kinds(&scan),
        [
            (ScanKind::Pass1, r(650, 450, 500, 100)),
            (ScanKind::Tile, r(0, 470, 1000, 270)),
            (ScanKind::Tile, r(500, 470, 560, 270)),
            (ScanKind::Anchor, r(880, 480, 40, 40)),
        ]
    );
    assert_eq!(
        *grabs.borrow(),
        [r(650, 450, 500, 100), r(0, 470, 1000, 270), r(500, 470, 560, 270)]
    );
    // The probes find no continuation. Pass 1 stands.
    let resolved = resolved.expect("hit");
    assert_eq!(resolved.span.text, "日本語を話す");
    assert_eq!(resolved.span.geom.len(), 6);
}

/// Pass 1 saw は cut to `(290,480,30,40)` at its leading edge. The probe replaces the
/// whole span with full boxes.
#[test]
fn a_wrap_at_the_line_end_is_read_by_one_probe_from_the_margin() {
    let page = wrap();
    let settings = Settings { max_passes: 1, prefer_vertical: false };
    let (mut source, grabs) = fixture(&page, settings);
    let (resolved, scan, _) = read(&mut source, pt(540, 500));

    assert_eq!(
        kinds(&scan),
        [
            (ScanKind::Pass1, r(290, 450, 500, 100)),
            (ScanKind::Tile, r(0, 470, 580, 270)),
            (ScanKind::Anchor, r(520, 480, 40, 40)),
        ]
    );
    assert_eq!(*grabs.borrow(), [r(290, 450, 500, 100), r(0, 470, 580, 270)]);
    let resolved = resolved.expect("hit");
    assert_eq!(resolved.span.text, "今日は日本語を勉強しました");
    assert_eq!(resolved.span.cursor_byte_offset, 24);
    assert_eq!(resolved.span.geom.len(), 13);
    assert_eq!(resolved.span.geom[8].rect, r(520, 480, 40, 40));
    assert_eq!(resolved.span.geom[9].rect, r(200, 550, 40, 40));
    assert_eq!(match_highlight(&resolved.span, Some(&card(1))), Some(r(517, 477, 46, 46)));
}

/// け is cut to 30 px at x 720 and ends at the box edge, so no wrap probe runs. The
/// tile restarts at け's own edge. The band is `max(3 * 40, 100) = 120` tall and
/// centered on the anchor. The second tile reads nothing and stops the read.
/// The stitched text has no geometry. The Anchor is the only per-glyph box that the
/// overlay keeps.
#[test]
fn a_line_the_box_clips_reads_forward_tiles_on_the_same_row() {
    let page = clipped();
    let settings = Settings { max_passes: 3, prefer_vertical: false };
    let (mut source, grabs) = fixture(&page, settings);
    let (resolved, scan, _) = read(&mut source, pt(500, 500));

    assert_eq!(
        kinds(&scan),
        [
            (ScanKind::Pass1, r(250, 450, 500, 100)),
            (ScanKind::Tile, r(720, 440, 500, 120)),
            (ScanKind::Tile, r(1220, 440, 500, 120)),
            (ScanKind::Anchor, r(480, 480, 40, 40)),
        ]
    );
    assert_eq!(
        *grabs.borrow(),
        [r(250, 450, 500, 100), r(720, 440, 500, 120), r(1220, 440, 500, 120)]
    );
    let resolved = resolved.expect("hit");
    assert_eq!(resolved.span.text, "うえおかきくけこさしすせそたちつてと");
    assert_eq!(resolved.span.cursor_byte_offset, 0);
    assert!(resolved.span.geom.is_empty());
    assert_eq!(match_highlight(&resolved.span, Some(&card(2))), None);
}

/// A vertical band puts `below` (240) on the left and `above` (30) on the right:
/// `x = 920 - 240`, `w = 270`. The probe starts at the output top edge and ends at
/// `540 + 20`. This is the tall box from the issue screenshot. It is correct for a
/// real column.
#[test]
fn a_column_with_prefer_vertical_pays_one_probe_from_the_top_edge() {
    let page = column();
    let settings = Settings { max_passes: 1, prefer_vertical: true };
    let (mut source, grabs) = fixture(&page, settings);
    let (resolved, scan, _) = read(&mut source, pt(920, 400));

    assert_eq!(
        kinds(&scan),
        [
            (ScanKind::Pass1, r(870, 150, 100, 500)),
            (ScanKind::Tile, r(680, 0, 270, 560)),
            (ScanKind::Anchor, r(900, 380, 40, 40)),
        ]
    );
    assert_eq!(*grabs.borrow(), [r(870, 150, 100, 500), r(680, 0, 270, 560)]);
    let resolved = resolved.expect("hit");
    assert_eq!(resolved.span.text, "日本語を話す");
    assert_eq!(resolved.span.cursor_byte_offset, 6);
    assert_eq!(resolved.orientation, Orientation::Vertical);
    assert_eq!(resolved.span.geom.len(), 6);
}

/// Three glyphs are visible. The box cuts 本 and を to 30 px. Their cut boxes keep
/// their text, so the line reads as a column. The line ends at the box edge, so no
/// probe runs. The sweep's invariants cover the cut boxes.
#[test]
fn a_column_in_a_horizontal_box_shows_only_the_box_and_the_anchor() {
    let page = column();
    let settings = Settings { max_passes: 1, prefer_vertical: false };
    let (mut source, grabs) = fixture(&page, settings);
    let (resolved, scan, _) = read(&mut source, pt(920, 400));

    assert_eq!(
        kinds(&scan),
        [(ScanKind::Pass1, r(670, 350, 500, 100)), (ScanKind::Anchor, r(900, 380, 40, 40))]
    );
    assert_eq!(*grabs.borrow(), [r(670, 350, 500, 100)]);
    assert_eq!(resolved.expect("hit").orientation, Orientation::Vertical);
}

#[test]
fn a_vertical_wrap_joins_the_column_to_the_left() {
    let page = two_columns();
    let settings = Settings { max_passes: 1, prefer_vertical: true };
    let (mut source, grabs) = fixture(&page, settings);
    let (resolved, scan, _) = read(&mut source, pt(920, 540));

    assert_eq!(
        kinds(&scan),
        [
            (ScanKind::Pass1, r(870, 290, 100, 500)),
            (ScanKind::Tile, r(680, 0, 270, 580)),
            (ScanKind::Anchor, r(900, 520, 40, 40)),
        ]
    );
    assert_eq!(*grabs.borrow(), [r(870, 290, 100, 500), r(680, 0, 270, 580)]);
    let resolved = resolved.expect("hit");
    assert_eq!(resolved.span.text, "今日は日本語を勉強しました");
    assert_eq!(resolved.span.cursor_byte_offset, 24);
    assert_eq!(resolved.span.geom.len(), 13);
    assert_eq!(resolved.span.geom[9].rect, r(830, 200, 40, 40));
    assert_eq!(match_highlight(&resolved.span, Some(&card(1))), Some(r(897, 517, 46, 46)));
}

/// Pass 1 reaches x 2130, past the output. `region_end >= bounds_end` disables the
/// clipped-line gate. The probes end at `min(1900 + 20, 1920)`, step by 500, and take
/// `min(remaining, 1000)` each.
#[test]
fn a_line_ending_at_the_screen_edge_pays_three_overlapping_probes() {
    let page = long_edge();
    let settings = Settings { max_passes: 1, prefer_vertical: false };
    let (mut source, grabs) = fixture(&page, settings);
    let (resolved, scan, _) = read(&mut source, pt(1880, 500));

    assert_eq!(
        kinds(&scan),
        [
            (ScanKind::Pass1, r(1630, 450, 500, 100)),
            (ScanKind::Tile, r(0, 470, 1000, 270)),
            (ScanKind::Tile, r(500, 470, 1000, 270)),
            (ScanKind::Tile, r(1000, 470, 920, 270)),
            (ScanKind::Anchor, r(1860, 480, 40, 40)),
        ]
    );
    assert_eq!(
        *grabs.borrow(),
        [
            r(1630, 450, 500, 100),
            r(0, 470, 1000, 270),
            r(500, 470, 1000, 270),
            r(1000, 470, 920, 270),
        ]
    );
    // Pass 1 shows ね cut to 30 px plus six full glyphs. No continuation exists, so
    // pass 1 stands.
    let resolved = resolved.expect("hit");
    assert_eq!(resolved.span.text, "ねのはひふへほ");
    assert_eq!(resolved.span.cursor_byte_offset, 18);
}

/// The box top cuts 5 px off the row, so the anchor is the cut box. The outline shows
/// what the box let OCR see. `hit_scan` accepts the glyph at distance 16, within
/// `35 / 2`.
#[test]
fn a_cursor_just_below_the_row_anchors_on_the_glyph_the_box_shows() {
    let page = short_stop();
    let settings = Settings { max_passes: 1, prefer_vertical: false };
    let (mut source, _) = fixture(&page, settings);
    let (resolved, scan, _) = read(&mut source, pt(900, 535));

    assert_eq!(
        kinds(&scan),
        [(ScanKind::Pass1, r(650, 485, 500, 100)), (ScanKind::Anchor, r(880, 485, 40, 35))]
    );
    assert_eq!(resolved.expect("hit").span.anchor, r(880, 485, 40, 35));
}

#[test]
fn a_cursor_past_half_a_glyph_below_the_row_hits_nothing_and_draws_nothing() {
    let page = short_stop();
    let settings = Settings { max_passes: 1, prefer_vertical: false };
    let (mut source, grabs) = fixture(&page, settings);
    let (resolved, scan, _) = read(&mut source, pt(900, 545));

    assert!(resolved.is_none());
    assert!(scan.is_empty());
    assert_eq!(*grabs.borrow(), [r(650, 495, 500, 100)]);
}

// The placement sweep.

fn matrix() -> [Settings; 4] {
    [
        Settings { max_passes: 1, prefer_vertical: false },
        Settings { max_passes: 1, prefer_vertical: true },
        Settings { max_passes: 3, prefer_vertical: false },
        Settings { max_passes: 3, prefer_vertical: true },
    ]
}

/// Five cursors per glyph plus two cursors on empty screen.
fn placements(page: &Page) -> Vec<PhysPoint> {
    let mut out = Vec::with_capacity(page.glyphs.len() * 5 + 2);
    for glyph in &page.glyphs {
        let g = glyph.rect;
        let c = g.center();
        out.push(c);
        out.push(pt(g.x + 1, g.y + 1));
        out.push(pt(g.x + S - 1, g.y + S - 1));
        // Below the glyph, within the half-height slack.
        out.push(pt(c.x, g.y + 55));
        // Left of the glyph.
        out.push(pt(g.x - 15, c.y));
    }
    out.push(pt(50, 50));
    out.push(pt(1800, 1000));
    out
}

/// Check the shape invariants for one placement.
///
/// - A miss draws nothing, and pass 1 still grabs once.
/// - The scan vector is `Pass1`, zero or more `Tile`s, then one `Anchor`.
/// - The outline draws exactly what the pipeline grabbed, in order.
/// - Every tile lies inside the output, is at most a probe long, and faces the
///   anchor on the cross axis. No tile repeats.
/// - The anchor is the hovered glyph's box, or the part of it that the box shows.
/// - Every geometry entry is one glyph's box, in text order. A stitched span has no
///   geometry, and a forward tile of band thickness `max(3 * anchor, 100)` read it.
/// - The match highlight covers the matched glyphs and no other glyph beyond the pad.
///   A match across two lines covers the paragraph width by design (`union_chars`),
///   so the overlap check skips it.
/// - A line of two or more words keeps the page line's orientation. One word reads
///   as a row.
fn check_placement(page: &Rc<Page>, settings: Settings, cursor: PhysPoint) {
    let (mut source, grabs) = fixture(page, settings);
    let (resolved, scan, lines) = read(&mut source, cursor);
    let context = format!(
        "{} passes={} vertical={} cursor={cursor:?} scan={:?}",
        page.name,
        settings.max_passes,
        settings.prefer_vertical,
        kinds(&scan)
    );
    let grabs = grabs.borrow();

    let Some(resolved) = resolved else {
        assert!(scan.is_empty(), "{context}");
        assert_eq!(grabs.len(), 1, "{context}");
        return;
    };

    assert!(scan.len() >= 2, "{context}");
    let pass1 = expected_pass1(cursor, settings.prefer_vertical);
    assert_eq!(scan[0], ScanRect { rect: pass1, kind: ScanKind::Pass1 }, "{context}");
    let last = scan[scan.len() - 1];
    assert_eq!(last.kind, ScanKind::Anchor, "{context}");
    let anchor = last.rect;
    let tiles: Vec<PhysRect> = scan[1..scan.len() - 1]
        .iter()
        .map(|s| {
            assert_eq!(s.kind, ScanKind::Tile, "{context}");
            s.rect
        })
        .collect();

    let mut expected_grabs = vec![pass1];
    expected_grabs.extend(&tiles);
    assert_eq!(*grabs, expected_grabs, "{context}");

    let orientation = resolved.orientation;
    let cross = orientation.cross(anchor.center());
    for (i, tile) in tiles.iter().enumerate() {
        assert!(tile.w > 0 && tile.h > 0, "{context}");
        assert_eq!(tile.intersection(page.bounds), Some(*tile), "{context}");
        assert!(orientation.len(*tile) <= 1000, "{context}");
        let start = orientation.cross(pt(tile.x, tile.y));
        assert!((start..start + orientation.thick(*tile)).contains(&cross), "{context}");
        assert!(!tiles[..i].contains(tile), "{context}");
    }

    let span = &resolved.span;
    assert_eq!(anchor, span.anchor, "{context}");
    let hovered = glyph_containing(page, anchor).unwrap_or_else(|| panic!("{context}"));
    assert!(2 * area(anchor) > area(hovered.rect), "{context}");
    assert!(anchor.edge_distance_to(cursor) <= f64::from(anchor.h) / 2.0, "{context}");
    assert_eq!(
        span.text[span.cursor_byte_offset..].chars().next(),
        Some(hovered.ch),
        "{context}"
    );

    if span.geom.is_empty() {
        assert!(settings.max_passes > 1, "{context}");
        let band = (orientation.thick(anchor) * 3).max(100);
        let forward = tiles
            .iter()
            .any(|t| orientation.len(*t) <= 500 && orientation.thick(*t) == band);
        assert!(forward, "{context}");
    } else {
        let chars: Vec<char> = span.text.chars().collect();
        for (i, entry) in span.geom.iter().enumerate() {
            assert_eq!(entry.char_count, 1, "{context}");
            let glyph = glyph_containing(page, entry.rect)
                .unwrap_or_else(|| panic!("{context} geom[{i}]={:?}", entry.rect));
            assert!(2 * area(entry.rect) > area(glyph.rect), "{context}");
            assert_eq!(chars.get(i), Some(&glyph.ch), "{context}");
        }
    }

    let from = span.text[..span.cursor_byte_offset].chars().count();
    let after = span.text[span.cursor_byte_offset..].chars().count();
    let len = after.min(3);
    let highlight = match_highlight(span, Some(&card(len)));
    if span.geom.is_empty() {
        assert_eq!(highlight, None, "{context}");
    } else {
        let m = highlight.unwrap_or_else(|| panic!("{context}"));
        let matched = &span.geom[from..from + len];
        for entry in matched {
            assert_eq!(m.intersection(entry.rect), Some(entry.rect), "{context}");
        }
        let line_of = |rect| glyph_containing(page, rect).map(|g| g.line);
        let first_line = line_of(matched[0].rect);
        if matched.iter().all(|e| line_of(e.rect) == first_line) {
            for (i, entry) in span.geom.iter().enumerate() {
                if (from..from + len).contains(&i) {
                    continue;
                }
                let clear = m
                    .intersection(entry.rect)
                    .is_none_or(|o| o.w <= HIGHLIGHT_PAD || o.h <= HIGHLIGHT_PAD);
                assert!(clear, "{context} geom[{i}]={:?} match={m:?}", entry.rect);
            }
        }
    }

    let (li, _) = hit_scan(&lines, cursor, true).unwrap_or_else(|| panic!("{context}"));
    let expected = if lines[li].words.len() >= 2 {
        page.lines[hovered.line]
    } else {
        Orientation::Horizontal
    };
    assert_eq!(orientation, expected, "{context}");
}

#[test]
fn every_cursor_placement_keeps_every_box_on_the_hovered_line() {
    for page in pages() {
        for settings in matrix() {
            for cursor in placements(&page) {
                check_placement(&page, settings, cursor);
            }
        }
    }
}
