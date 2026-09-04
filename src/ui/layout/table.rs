//! One table: the layout builds it from a subtree, places it on a grid, and sizes
//! and collapses it.
//!
//! **One reason to change:** the table rules of HTML and CSS. This file implements
//! cell placement, span clamping, track sizing, and `border-collapse`.
//!
//! All four steps live in this file. Two of the four steps are methods on types
//! that other modules own. [`Paragraphs`] builds the [`Grid`]. [`Pass`] measures it.
//!
//! A change to one table rule never stays inside one step. A change to `colSpan`
//! touches the placement, the track sizing, and the collapse. The placement reads
//! the span. The track sizing spreads it across the tracks it reaches. The collapse
//! decides which rules a cell does not draw. A split by owning type would put one
//! change across three files.

use crate::dict::gloss::{GlossDoc, NodeId, NodePath, Scalar, Tag};
use super::chrome::LINE_GAP;
use super::flow::Ctx;
use super::gloss::Paragraphs;
use super::measure::{MeasureError, TextMeasure};
use super::pass::{BoxHeader, Pass, Piece};
use super::scene::{box_elem, ElemBox, ElemKind, GlossOrigin, HitTarget, SceneElem, SceneRect};
use super::style::{Block, BorderStyle, BoxStyle, Edges, Inline, YOMITAN_BASE_PX};

/// The width of a table cell border, matched to Yomitan's own default
/// stylesheet: `.gloss-sc-th, .gloss-sc-td { border-width: calc(1em /
/// var(--font-size-no-units)) }`.
///
/// The spec's defaults table states this rule as `1em / 14`. That is one
/// pixel at the base font size. The rule scales with the panel's font
/// size, so it stays visible on a dense screen.
///
/// The function divides the em by the constant. It does not multiply by
/// a reciprocal constant instead. A panel drawn at Yomitan's own base
/// size then gets exactly the one pixel the rule promises. See
/// [`YOMITAN_BASE_PX`].
pub(super) fn cell_border(em: f32) -> f32 {
    em / YOMITAN_BASE_PX
}

/// The padding of a table cell, as a fraction of its own em. Yomitan's
/// stylesheet sets `padding: 0.25em` in the same default rule as the
/// border width.
pub(super) const CELL_PADDING: f32 = 0.25;

/// The limit on a cell's span, matched to the limit in the HTML spec.
///
/// The dictionary schema sets no limit on `colSpan`. Each column in the
/// grid costs memory. Without a limit, an absurd `colSpan` value would
/// cost memory rather than draw a wide cell. The HTML spec itself clamps
/// `colspan` to 1000, so this constant uses the same number.
pub(super) const MAX_SPAN: usize = 1000;

/// One table cell, before the grid measures it.
pub(super) struct Cell {
    /// The content of the cell. A cell is a block container, so it holds
    /// paragraphs. A cell can also hold a table, if a dictionary nested
    /// one inside it.
    pub(super) body: Vec<Piece>,
    /// The box of the cell. This box starts from Yomitan's own grid
    /// defaults. The dictionary's own declarations override these
    /// defaults. The grid resolves the border collapse into this box
    /// once it knows the rule width. See [`collapsed`].
    pub(super) style: BoxStyle,
    /// The style of the cell's own element. The cell holds no text of
    /// its own, so this style affects only the cull slack.
    pub(super) base: Inline,
    /// `(row, column)`, set by [`place`].
    pub(super) at: (usize, usize),
    /// `(rowSpan, colSpan)`. Each value is at least one. [`place`]
    /// already clipped both values to fit inside the grid.
    pub(super) span: (usize, usize),
    pub(super) path: Option<NodePath>,
}

impl Cell {
    /// The location of the cell in its dictionary.
    pub(super) fn origin(&self, grid: &Grid) -> GlossOrigin {
        GlossOrigin {
            dict_id: grid.dict_id,
            entry_id: grid.entry_id,
            entry: grid.entry,
            path: self.path,
        }
    }
}

/// One table, before the layout measures it.
///
/// This struct holds structure only. The slot of each cell needs no
/// measurement, so the tree walk decides it. The grid pass then adds the
/// column widths, the row heights, and the position of the whole table.
pub(super) struct Grid {
    /// The gap owed above the table.
    pub(super) top_gap: f32,
    /// Every cell, in document order. Each cell carries the slot that
    /// [`place`] gave it.
    pub(super) cells: Vec<Cell>,
    pub(super) rows: usize,
    pub(super) cols: usize,
    /// The table's own box: its indent and its alignment.
    pub(super) block: Block,
    /// The style of the table's own element.
    pub(super) base: Inline,
    pub(super) path: Option<NodePath>,
    pub(super) dict_id: i64,
    pub(super) entry_id: i64,
    pub(super) entry: u32,
}

impl Grid {
    /// The location of the table in its dictionary.
    pub(super) fn origin(&self) -> GlossOrigin {
        GlossOrigin {
            dict_id: self.dict_id,
            entry_id: self.entry_id,
            entry: self.entry,
            path: self.path,
        }
    }

    /// The fields that every block container shares, in the form that
    /// [`Pass::open_box`] takes. A table is one such container. A table
    /// carries a margin, a border, and a padding around its grid, in the
    /// same way that a `div` carries them around its paragraphs.
    pub(super) fn header(&self) -> BoxHeader {
        BoxHeader {
            top_gap: self.top_gap,
            block: self.block,
            base: self.base,
            origin: self.origin(),
        }
    }
}

/// Puts every cell in a slot.
///
/// This function follows HTML's own placement rule, minus the parts
/// that a dictionary never writes. A row's cell takes the leftmost
/// column that no earlier `rowSpan` claimed. Each cell then claims
/// `colSpan` columns across and `rowSpan` rows down. A span that
/// reaches past the last row stops at that row. Every cell's slot
/// therefore stays inside the grid that will measure it.
///
/// The grid width equals the furthest column that any row reached. A
/// row with more cells than its neighbor rows widens the whole table.
/// The shorter rows end early. A malformed table is owed this
/// behavior: it always renders what it can. No cell is dropped, and no
/// index can run past the end of the grid.
pub(super) fn place(rows: Vec<Vec<Cell>>) -> (Vec<Cell>, usize, usize) {
    let count = rows.len();
    // For each column, the row index just past the last row that a
    // `rowSpan` already claimed.
    let mut claimed: Vec<usize> = Vec::new();
    let mut out = Vec::new();
    for (r, row) in rows.into_iter().enumerate() {
        let mut c = 0usize;
        for mut cell in row {
            while claimed.get(c).is_some_and(|&to| to > r) {
                c += 1;
            }
            let down = cell.span.0.min(count - r);
            let across = cell.span.1;
            cell.at = (r, c);
            cell.span = (down, across);
            if claimed.len() < c + across {
                claimed.resize(c + across, 0);
            }
            for to in &mut claimed[c..c + across] {
                *to = r + down;
            }
            c += across;
            out.push(cell);
        }
    }
    let cols = claimed.len();
    (out, count, cols)
}

/// A cell's `rowSpan` or `colSpan`.
///
/// The schema writes these values as numbers. The Anki HTML renderer
/// also reads only this numeric form. Any other value counts as one
/// cell, and so does any value below one. HTML's own parser treats a
/// missing or zero span the same way. A cell that occupies no slot has
/// nowhere to draw.
pub(super) fn span_of(doc: &GlossDoc, id: NodeId, key: &str) -> usize {
    match doc.attr_of(id, key) {
        Some(Scalar::Num(n)) if n >= 1.0 => (n as usize).min(MAX_SPAN),
        _ => 1,
    }
}

/// The sum of `len` tracks starting at `at`, plus the rules between
/// them.
///
/// This sum is the width offered to a cell that spans several columns.
/// It is also the height that fills a cell that spans several rows.
///
/// The function sums the track sizes. It does not subtract two
/// accumulated edge positions, and this choice matters. The expression
/// `(x0 + w + rule) - x0 - rule` is one float ulp short of `w`. A cell
/// that is one ulp narrower than its own text wraps a line that would
/// otherwise fit.
pub(super) fn extent(tracks: &[f32], at: usize, len: usize, rule: f32) -> f32 {
    tracks[at..at + len].iter().sum::<f32>() + (len as f32 - 1.0) * rule
}

/// Grows `tracks` until every span fits.
///
/// Columns and rows share one sizing rule, so the code writes it once
/// in this function. A track takes the widest request among the cells
/// that live in only that track. A cell that spans several tracks adds
/// only its shortfall. The shortfall is what the cell still needs
/// beyond the tracks it already covers, including the `rule` between
/// them. The function spreads this shortfall evenly across the tracks
/// the cell spans.
///
/// The function visits each spanning cell once, in document order,
/// against the tracks as earlier cells left them. There is no
/// fixpoint, and there is no second round. Two passes over the cells
/// give the answer.
pub(super) fn distribute(
    tracks: &mut [f32],
    spans: impl Iterator<Item = (usize, usize, f32)> + Clone,
    rule: f32,
) {
    for (at, len, want) in spans.clone() {
        if len == 1 {
            tracks[at] = tracks[at].max(want);
        }
    }
    for (at, len, want) in spans {
        if len < 2 {
            continue;
        }
        let n = len as f32;
        let have = extent(tracks, at, len, rule);
        if want > have {
            let add = (want - have) / n;
            for track in &mut tracks[at..at + len] {
                *track += add;
            }
        }
    }
}

/// One cell's box, with its shared edges resolved.
///
/// `border-collapse: collapse` draws the rule between two cells only
/// once. Each cell owns the rules on its own left edge and its own top
/// edge. Only the cells against the grid's right edge or bottom edge
/// own the closing rules. Two neighbor cells therefore abut exactly,
/// with one `rule` between them, not two rules side by side. A cell
/// that spans several tracks does not draw the rules inside its own
/// span.
///
/// An edge with `border-style: none` still draws nothing. The grid
/// keeps the slot it reserved for that edge, and leaves it empty, in
/// the same way that a browser does.
pub(super) fn collapsed(
    mut style: BoxStyle,
    rule: f32,
    last_col: bool,
    last_row: bool,
) -> BoxStyle {
    let own = style.border_used();
    let edge = |own: f32, draws: bool| if own > 0.0 && draws { rule } else { 0.0 };
    style.border = Edges {
        top: edge(own.top, true),
        right: edge(own.right, last_col),
        bottom: edge(own.bottom, last_row),
        left: edge(own.left, true),
    };
    style
}

/// Moves a laid-out run of elements, and the hit targets they earned,
/// down by `dy`.
///
/// The grid lays out a cell before it knows the height of the cell's
/// row. A row is as tall as its tallest cell. The grid places every
/// cell at the table's own top first. This function then drops each
/// cell into its row.
///
/// Only origins move. A line box is the measurer's own answer. A
/// reading's position is relative to its run. Nothing that a bin
/// re-measures changes when this function runs. [`measure_readings`]
/// sets this rule. This rule is also why a row's extra height never
/// reaches the text inside a cell.
///
/// [`measure_readings`]: super::ruby::measure_readings
pub(super) fn shift(elems: &mut [SceneElem], hits: &mut [HitTarget], dy: f32) {
    for elem in elems {
        elem.pen.1 += dy;
        elem.rect.y += dy;
        if let Some(b) = &mut elem.block_box {
            b.rect.y += dy;
        }
        for b in &mut elem.inline_boxes {
            b.rect.y += dy;
        }
    }
    for hit in hits {
        hit.y += dy;
    }
}

/// The table half of the gloss walk: it turns a `table` subtree into a
/// [`Grid`].
///
/// This code builds structure only. The slot of each cell needs no
/// measurement. It also applies the Yomitan cell defaults, because a
/// cell's box starts as a default before it becomes a declaration.
impl Paragraphs<'_> {
    /// One `table`: its rows, as a grid.
    ///
    /// The table is also the block container that Yomitan wraps one in
    /// (`.gloss-sc-table-container { display: block }`). It closes the
    /// paragraph before it, and it opens no paragraph of its own. Every
    /// word in a table belongs to a cell instead.
    ///
    /// A `table` nested inside a cell is still a table inside a cell.
    /// A cell's content is a list of [`Piece`]s. [`Pass::block`] lays out
    /// a nested grid through the same two methods that laid out the
    /// outer grid.
    pub(super) fn table(&mut self, id: NodeId, ctx: Ctx) {
        // A list item can owe a marker. The item's only content can
        // be a table. The table then has no line for the marker to
        // hang beside. The marker takes the line above the table
        // instead. A browser does the same: it puts a `::marker`
        // beside a block too.
        //
        // The code marks this case inline on purpose. A marker that
        // hangs in the gutter needs a line box to share a baseline
        // with. Without this call, the paragraph that opens here
        // would have no line box of its own. A gutter with no line
        // beside it would drop the bullet.
        self.mark_inline();
        self.flush();
        let (inline, link) = self.enter(id, ctx);
        let block = self.boxed(id, ctx.block, inline);
        let inner = Ctx {
            inline,
            // A cell pays the box and the list indent only once,
            // through the table that holds the cell. This is the
            // same reason that [`Block::inherited`] hands a child an
            // empty box.
            block: Block { indent: 0.0, ..block.inherited() },
            link,
            path: ctx.path,
        };
        let mut rows = Vec::new();
        self.rows_of(id, inner, false, &mut rows);
        let (cells, rows, cols) = place(rows);
        // A table with no cells draws nothing. It does not draw an
        // empty rule.
        if cells.is_empty() {
            return;
        }
        self.out.push(Piece::Table(Grid {
            top_gap: 0.0,
            cells,
            rows,
            cols,
            block,
            base: inline,
            path: ctx.path,
            dict_id: 0,
            entry_id: 0,
            entry: 0,
        }));
    }

    /// Every row under a `table` or a row group, in document order.
    ///
    /// The tags `thead`, `tbody`, and `tfoot` group rows. They add no
    /// grid structure of their own. The parser already classes all
    /// three tags as `Kind::Table`, so each tag contributes only its
    /// rows to the grid.
    ///
    /// A `thead` tag also carries Yomitan's header styling to the cells
    /// inside it. This is why a `td` in a header row renders bold and
    /// tinted. A `tfoot` tag is out of scope for this styling. It
    /// contributes its rows with no special styling. This choice loses
    /// no text. It also invents no styling for a shape that the census
    /// reports zero times.
    ///
    /// Content that a dictionary writes outside any cell becomes one
    /// anonymous cell inside one anonymous row. A malformed table
    /// therefore loses none of this content, and it gains no extra
    /// structure either.
    ///
    /// This repair is not a simplification. It matches CSS 2.1 section
    /// 17.2.1 exactly. Rule 2.1 of that section wraps a child that is
    /// not a proper table child. The spec's own words are "and all
    /// consecutive siblings of C that are not proper table children".
    /// This wrapped content goes in one anonymous row. Rule 2.3 then
    /// wraps a run of non-cell children of that row in one anonymous
    /// cell.
    ///
    /// A `td` written straight under a `table`, with no `tr` around
    /// it, is also not a proper table child. This `td` therefore joins
    /// the same anonymous row as the loose content around it. But the
    /// `td` keeps its own slot in that row.
    ///
    /// A different rule, one cell for each child, once turned a real
    /// example 90 degrees. 旺文社漢字典 第四版's radical index is a `table`
    /// of 19 `span` tags, one for each stroke-count group. The one
    /// cell per child rule turned this into one row of 19 columns,
    /// each about 6 px wide.
    ///
    /// A browser reads the dictionary's own stylesheet for this table
    /// and draws 19 rows of two columns. chibipop resolves no
    /// `display` property at all. This choice is deliberate. See
    /// `Tag::is_block`. The census also shows that `display: grid` is
    /// the most common declaration in the corpus.
    ///
    /// Because chibipop ignores `display`, the anonymous-box repair is
    /// the only reading it can produce. A browser without a stylesheet
    /// also produces this same reading. Neither reading produces 19
    /// columns.
    ///
    /// An anonymous cell that holds only whitespace between two
    /// written cells is dropped. This also drops the column that the
    /// cell would otherwise invent.
    pub(super) fn rows_of(&mut self, id: NodeId, ctx: Ctx, head: bool, out: &mut Vec<Vec<Cell>>) {
        let doc = self.doc;
        let mut stray: Vec<Cell> = Vec::new();
        // The run of consecutive children that the next anonymous cell
        // will hold. Each child keeps the index that its own address
        // needs. The code uses one buffer for each table, not one for
        // each cell. A boundary closes a run, and a new empty run then
        // starts.
        let mut loose: Vec<(usize, NodeId)> = Vec::new();
        for (i, child) in doc.children(id).enumerate() {
            let tag = doc.node(child).tag;
            let row_level = matches!(tag, Tag::Tr | Tag::Thead | Tag::Tbody | Tag::Tfoot);
            // Every other tag joins the run. A boundary closes the run
            // into the one anonymous cell it is owed. A written cell, a
            // row-level tag, or the end of the loop can each be that
            // boundary.
            if !row_level && !matches!(tag, Tag::Td | Tag::Th) {
                loose.push((i, child));
                continue;
            }
            stray.extend(self.implied(ctx, &loose));
            loose.clear();
            if row_level && !stray.is_empty() {
                out.push(std::mem::take(&mut stray));
            }
            let at = ctx.at(i);
            match tag {
                Tag::Tr => {
                    let row = self.row(child, at, head);
                    if !row.is_empty() {
                        out.push(row);
                    }
                }
                Tag::Thead | Tag::Tbody | Tag::Tfoot => {
                    let inline = self.styled(child, ctx.inline);
                    let block = self.boxed(child, ctx.block, inline);
                    let inner = Ctx { inline, block: block.inherited(), ..at };
                    self.rows_of(child, inner, head || tag == Tag::Thead, out);
                }
                // A `td` or a `th`, and nothing else. The guard above
                // already sent every other tag into the run.
                _ => stray.push(self.cell(child, at, head)),
            }
        }
        stray.extend(self.implied(ctx, &loose));
        if !stray.is_empty() {
            out.push(stray);
        }
    }

    /// One `tr`: its cells, in order.
    ///
    /// The code does not draw a row's own box. Under `border-collapse`,
    /// a row's border collapses into its cells, and this grid resolves
    /// the collapse there. Yomitan also declares no `tr` rule at all.
    /// The one real dictionary that writes a `tr` border writes a width
    /// with no style. A browser also draws nothing for that rule.
    pub(super) fn row(&mut self, id: NodeId, ctx: Ctx, head: bool) -> Vec<Cell> {
        let doc = self.doc;
        let (inline, link) = self.enter(id, ctx);
        let block = self.boxed(id, ctx.block, inline);
        let inner = Ctx {
            inline,
            block: block.inherited(),
            link,
            path: ctx.path,
        };
        let mut out = Vec::new();
        let mut loose: Vec<(usize, NodeId)> = Vec::new();
        for (i, child) in doc.children(id).enumerate() {
            if !matches!(doc.node(child).tag, Tag::Td | Tag::Th) {
                loose.push((i, child));
                continue;
            }
            out.extend(self.implied(inner, &loose));
            loose.clear();
            out.push(self.cell(child, inner.at(i), head));
        }
        out.extend(self.implied(inner, &loose));
        out
    }

    /// The one anonymous cell that a run of content written outside any
    /// cell collapses into, if the run renders anything.
    ///
    /// An explicit empty `<td>` is a blank cell in a paradigm, and it
    /// keeps its border. An anonymous cell that holds nothing is only
    /// the whitespace that a dictionary left between two written cells.
    /// A border around it would invent a column that was never there.
    ///
    /// This anonymous cell draws no border and pays no padding, because
    /// it is not a `td`. Yomitan hangs its cell defaults on
    /// `.gloss-sc-th, .gloss-sc-td`, and a `span` matches neither class.
    /// CSS 2.1 section 17.2.1 also gives an anonymous box the initial
    /// value of every property that it does not inherit.
    ///
    /// An earlier version charged [`Paragraphs::cell_defaults`] here.
    /// That version drew 19 boxes that a browser leaves undrawn over
    /// 旺文社漢字典's radical index. It also took 0.25em of padding per
    /// side out of columns narrower than that padding. This padding
    /// loss drove the wrap width down to its own 1 px floor, and it put
    /// every glyph of a group on a line of its own.
    ///
    /// The code also does not read the run's own declarations into the
    /// cell's box. Those declarations belong to the nodes.
    /// [`Paragraphs::node`] resolves them again inside the cell.
    /// Reading them twice here would charge one margin twice.
    pub(super) fn implied(&mut self, ctx: Ctx, run: &[(usize, NodeId)]) -> Option<Cell> {
        if run.is_empty() {
            return None;
        }
        // An anonymous box has no element of its own. The table or the
        // row that generated it addresses the cell instead.
        let body = self.enclose(ctx.path, ctx.block.inherited(), |p| {
            for &(i, child) in run {
                p.node(child, ctx.at(i), false);
            }
        });
        (!body.is_empty()).then(|| Cell {
            body,
            style: BoxStyle::default(),
            base: ctx.inline,
            at: (0, 0),
            span: (1, 1),
            path: ctx.path,
        })
    }

    /// One `td` or `th`.
    ///
    /// The cell opens a new paragraph of its own. It does not join the
    /// paragraph before it. `td` and `th` do this nowhere else in this
    /// walk: they are in neither `Tag::is_block` nor `Tag::is_inline`,
    /// because a cell is a grid problem and not a line-break problem.
    /// This new paragraph carries the cell's own address, so a hit
    /// inside a conjugation table resolves to that cell's subtree.
    pub(super) fn cell(&mut self, id: NodeId, ctx: Ctx, head: bool) -> Cell {
        let doc = self.doc;
        let (inline, link) = self.enter(id, ctx);
        // Yomitan tints a `th` tag and everything inside a `thead` tag.
        // `tag_style` owns the bold half of the same rule, so a `td` in
        // a header row inherits its weight from the `thead`. No second
        // rule for weight belongs here.
        let tint = head || doc.node(id).tag == Tag::Th;
        let defaults = self.cell_defaults(ctx.block, inline, tint);
        let block = self.declared(id, defaults, inline.size);
        let inner = Ctx {
            inline,
            block: block.inherited(),
            link,
            path: ctx.path,
        };
        let body = self.enclose(ctx.path, block.inherited(), |p| p.children(id, inner));
        Cell {
            body,
            style: block.style,
            base: inline,
            at: (0, 0),
            span: (span_of(doc, id, "rowSpan"), span_of(doc, id, "colSpan")),
            path: ctx.path,
        }
    }

    /// One cell's paragraphs, collected off to one side.
    ///
    /// The grid measures each cell on its own, so a cell's paragraphs
    /// cannot go into the stream that the panel is stacking. A cell's
    /// first paragraph starts at the cell's own top. The panel spaces
    /// the paragraphs under it the same way it spaces every other
    /// stacked paragraph.
    fn enclose(
        &mut self,
        path: Option<NodePath>,
        block: Block,
        walk: impl FnOnce(&mut Self),
    ) -> Vec<Piece> {
        let outer = std::mem::take(&mut self.out);
        self.open(path, block);
        walk(self);
        self.flush();
        let mut body = std::mem::replace(&mut self.out, outer);
        for (i, piece) in body.iter_mut().enumerate() {
            piece.top_gap(if i == 0 { 0.0 } else { LINE_GAP });
        }
        body
    }

    /// Yomitan's own cell box, before the dictionary declares anything.
    ///
    /// This box matches the spec's defaults table exactly: a solid
    /// border of [`cell_border`] on every edge, in the panel's own rule
    /// color, [`CELL_PADDING`] of padding, and a tinted background on a
    /// header cell.
    ///
    /// `vertical-align: top` is the other half of that defaults table,
    /// and it needs no field here. [`Pass::table`] applies it by
    /// placing a short cell at the top of its row.
    pub(super) fn cell_defaults(&self, parent: Block, inline: Inline, tint: bool) -> Block {
        let em = inline.size;
        Block {
            style: BoxStyle {
                border: Edges::all(cell_border(em)),
                border_style: Edges::all(BorderStyle::Solid),
                border_color: self.rule,
                padding: Edges::all(em * CELL_PADDING),
                background: tint.then_some(self.tint),
                ..BoxStyle::default()
            },
            ..parent
        }
    }
}

/// The grid half of the geometry pass: it turns a [`Grid`] into cells
/// with rects.
///
/// The block half lives in [`pass`](super::pass). This code is a second
/// `impl` block on [`Pass`], not a type of its own, for the same reason
/// that [`Pass`] exists: every buffer that a cell's paragraph reuses is
/// already inside it.
impl<'a> Pass<'a> {
    /// One table, as a grid.
    ///
    /// This function runs four steps in order, and no step repeats.
    /// First, it finds one rule width for the whole grid. Second, it
    /// finds the column widths from the content of the cells (see
    /// [`Pass::columns`]). Third, it lays out every cell at its own
    /// column, with its y position deferred. Fourth, it finds the row
    /// heights. This fourth step is the first moment that a cell's own
    /// top position is known.
    ///
    /// The table element itself leads the cells in draw order, so a
    /// declared background sits under them. The function fills in the
    /// table element's own geometry last, because a shrink-to-fit table
    /// is exactly as wide as the grid inside it.
    ///
    /// The box around the grid is a `div`'s box. The same two helpers
    /// open and close it ([`Pass::open_box`], [`Pass::close_box`]). A
    /// table differs only in what goes inside the box, and in taking
    /// the width that its grid wants rather than the width it was
    /// offered.
    pub(super) fn table(
        &mut self,
        m: &mut dyn TextMeasure,
        grid: &'a Grid,
        at: (f32, f32),
        avail_w: f32,
    ) -> Result<(f32, f32), MeasureError> {
        let lead = self.open_box(ElemKind::Table, grid.header(), at, avail_w);
        let (x0, y0) = lead.pen;

        // One rule width for the whole grid: the widest width that any
        // cell asked for. This is exactly `border-collapse`'s own
        // conflict rule, where the wider border wins the shared edge.
        // One width for each grid also turns the column arithmetic into
        // a sum instead of a negotiation.
        let rule = grid.cells.iter().fold(0.0f32, |w, cell| {
            let e = cell.style.border_used();
            w.max(e.top).max(e.right).max(e.bottom).max(e.left)
        });
        // A grid of `cols` columns has `cols + 1` rules down it.
        let rules = rule * (grid.cols as f32 + 1.0);
        // Everything that the columns have between them. This value is
        // never negative. A table with more rules than room collapses
        // its columns instead of reserving width that it does not
        // have.
        let share = (lead.avail - rules).max(0.0);
        let inner = self.columns(m, grid, rule, share)?;

        // Column edges: the outside of the rule before each column,
        // plus one edge past the last column. The code accumulates
        // each edge once and reads it twice, so two neighbor columns
        // share one number and abut exactly. An edge that the code
        // differences back out of a running sum does not abut exactly,
        // because `(x + rule) - rule` is not always `x`.
        let mut ex = Vec::with_capacity(grid.cols + 1);
        let mut x = x0;
        for w in &inner {
            ex.push(x);
            x += rule + w;
        }
        ex.push(x);

        // The code places every cell at its own column, with its y
        // position deferred to zero. A row is as tall as its tallest
        // cell, so no cell's own top position is known until the code
        // measures every cell in that row.
        let mut tall = Vec::with_capacity(grid.cells.len());
        let mut placed = Vec::with_capacity(grid.cells.len());
        for cell in &grid.cells {
            let (c, across) = (cell.at.1, cell.span.1);
            let pad = cell.style.padding;
            // A cell that spans several columns gets those columns and
            // the rules inside its own span, minus its own padding.
            // The code sums this value from the column widths. It does
            // not take the value off `ex`, because a wrap width one
            // ulp short of the cell's own text breaks a line that fits.
            // See [`extent`].
            let w = (extent(&inner, c, across, rule) - pad.horizontal()).max(1.0);
            let (elems, hits) = (self.out.len(), self.hits.len());
            self.out.push(box_elem(ElemKind::Cell, cell.base, grid.block.align, cell.origin(grid)));
            let advance = self.block(m, &cell.body, (ex[c] + rule + pad.left, 0.0), w)?.1;
            placed.push((elems, self.out.len(), hits, self.hits.len()));
            tall.push(pad.vertical() + advance);
        }

        let mut rows = vec![0.0f32; grid.rows];
        distribute(
            &mut rows,
            grid.cells.iter().zip(&tall).map(|(c, &h)| (c.at.0, c.span.0, h)),
            rule,
        );
        let mut ey = Vec::with_capacity(grid.rows + 1);
        let mut y = y0;
        for h in &rows {
            ey.push(y);
            y += rule + h;
        }
        ey.push(y);

        for (i, cell) in grid.cells.iter().enumerate() {
            let (r, c) = cell.at;
            let (down, across) = cell.span;
            let pad = cell.style.padding;
            let (from, to, hit_from, hit_to) = placed[i];
            // This call drops the cell's content into the row that the
            // grid grew for it. Nothing stretches. A cell shorter than
            // its row sits at the top of the row, which matches
            // Yomitan's own `vertical-align: top`. Every line box stays
            // exactly the one that the measurer reported.
            shift(
                &mut self.out[from + 1..to],
                &mut self.hits[hit_from..hit_to],
                ey[r] + rule + pad.top,
            );
            let style =
                collapsed(cell.style, rule, c + across == grid.cols, r + down == grid.rows);
            let used = style.border_used();
            // A cell that draws a rule starts at the outside of that
            // rule. A cell that does not draw a rule starts at its own
            // inside edge instead. The box then holds exactly the
            // rules that this cell owns, and its neighbor's box begins
            // where this box ends.
            let edge = |at: f32, own: f32| if own > 0.0 { at } else { at + rule };
            let (left, top) = (edge(ex[c], used.left), edge(ey[r], used.top));
            let rect = SceneRect {
                x: left,
                y: top,
                w: ex[c + across] + used.right - left,
                h: ey[r + down] + used.bottom - top,
            };
            let elem = &mut self.out[from];
            elem.wrap_w = (extent(&inner, c, across, rule) - pad.horizontal()).max(1.0);
            elem.pen = (ex[c] + rule + pad.left, ey[r] + rule + pad.top);
            elem.rect = rect;
            elem.block_box = Some(ElemBox { rect, style });
        }

        let grid_w = ex[grid.cols] + rule - x0;
        let grid_h = ey[grid.rows] + rule - y0;
        // This box is shrink-to-fit, in the same way that a browser
        // sizes a table with no declared width. The box takes the
        // grid's own width, not the container's width. This box is
        // also conditional, unlike a `div`'s box: only a block that
        // declared a box becomes a [`Boxed`], while every table is a
        // [`Grid`] whether the dictionary declared one or not.
        let block_box = lead.style.exists().then(|| ElemBox {
            rect: lead.border_box(lead.fitted_w(grid_w), grid_h),
            style: lead.style,
        });
        // The element's own extent is the grid, not that box. A
        // table's margin and padding sit outside the cells that a hit
        // test or a cull check reads.
        let rect = SceneRect { x: x0, y: y0, w: grid_w, h: grid_h };
        let advance = self.close_box(&lead, rect, block_box, grid_h);
        Ok((lead.demand(grid_w), advance))
    }

    /// Every column's width, from the content of the cells in it.
    ///
    /// This function sizes the columns in one fixed pass. It does not
    /// use CSS auto-layout, on purpose:
    ///
    /// 1. The function lays out each cell once, at `share`, the whole
    ///    width that the columns have between them. What the cell
    ///    measures to, plus its own horizontal padding, is what the
    ///    cell asks for.
    /// 2. A column takes the widest request among the single-column
    ///    cells inside it. A cell that spans several columns adds only
    ///    its shortfall, spread evenly (see [`distribute`]).
    /// 3. If the columns together still want more than `share`, the
    ///    function multiplies every column by the one factor that
    ///    makes them fit exactly.
    ///
    /// Step 3 keeps a table inside the panel. The columns shrink and
    /// their content rewraps, so a wide table can never ask the panel
    /// to widen for it. There is no second round and no negotiation.
    /// The only thing that the code measures again after this step is
    /// each cell at its final width, during the height pass.
    pub(super) fn columns(
        &mut self,
        m: &mut dyn TextMeasure,
        grid: &'a Grid,
        rule: f32,
        share: f32,
    ) -> Result<Vec<f32>, MeasureError> {
        let mut want = Vec::with_capacity(grid.cells.len());
        for cell in &grid.cells {
            let pad = cell.style.padding.horizontal();
            // An empty cell asks for nothing, so it is not worth a
            // measurement. Most cells in a Jitendex forms table are
            // empty.
            let ink = if cell.body.is_empty() {
                0.0
            } else {
                self.trial(m, &cell.body, (share - pad).max(1.0))?
            };
            want.push(pad + ink);
        }
        let mut inner = vec![0.0f32; grid.cols];
        distribute(
            &mut inner,
            grid.cells.iter().zip(&want).map(|(c, &w)| (c.at.1, c.span.1, w)),
            rule,
        );
        let total: f32 = inner.iter().sum();
        if total > share {
            let scale = share / total;
            for col in &mut inner {
                *col *= scale;
            }
        }
        Ok(inner)
    }
}
