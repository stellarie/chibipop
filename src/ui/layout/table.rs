//! One table: built from a subtree, placed on a grid, sized, collapsed.
//!
//! **One reason to change:** HTML's and CSS's table rules as this build
//! answers them - cell placement, span clamping, track sizing, and
//! `border-collapse`.
//!
//! All four steps are here, including the two that are methods on other
//! modules' types ([`Paragraphs`] builds the [`Grid`], [`Pass`] measures
//! it), because a table rule is never only one of them: a `colSpan` change
//! touches the placement that reads it, the track sizing that spreads it
//! and the collapse that decides which rules the cell swallowed. Splitting
//! them by owning type would put one change in three files.

use crate::dict::gloss::{GlossDoc, NodeId, NodePath, Scalar, Tag};
use super::chrome::LINE_GAP;
use super::flow::Ctx;
use super::gloss::Paragraphs;
use super::measure::{MeasureError, TextMeasure};
use super::pass::{BoxHeader, Pass, Piece};
use super::scene::{box_elem, ElemBox, ElemKind, GlossOrigin, HitTarget, SceneElem, SceneRect};
use super::style::{Block, BorderStyle, BoxStyle, Edges, Inline, YOMITAN_BASE_PX};

/// Yomitan's own table cell border,
/// as the spec's defaults table
/// states it: `1em / 14`, one pixel
/// at the base font size, so a cell
/// rule scales with the panel instead
/// of vanishing on a dense screen
/// (`.gloss-sc-th, .gloss-sc-td {
/// border-width: calc(1em /
/// var(--font-size-no-units)) }`).
///
/// A division rather than a
/// reciprocal constant, so that a
/// panel drawn at Yomitan's own base
/// size gets exactly the one pixel
/// the rule promises
/// ([`YOMITAN_BASE_PX`]).
pub(super) fn cell_border(em: f32) -> f32 {
    em / YOMITAN_BASE_PX
}

/// Yomitan's own table cell padding,
/// as a fraction of the em it sits
/// in: `padding: 0.25em`, from the
/// same rule.
pub(super) const CELL_PADDING: f32 = 0.25;

/// HTML's own cap on a cell's span.
///
/// The schema puts no bound on
/// `colSpan` and a grid costs memory
/// per column, so an absurd one is a
/// bill rather than a wide cell.
/// 1000 is the number HTML itself
/// clamps `colspan` to.
pub(super) const MAX_SPAN: usize = 1000;

/// One table cell, before the grid
/// measures it.
pub(super) struct Cell {
    /// Its content. A cell is a block
    /// container, so it holds
    /// paragraphs - and, where a
    /// dictionary nested one, a table.
    pub(super) body: Vec<Piece>,
    /// Its box: Yomitan's own grid
    /// defaults with the dictionary's
    /// declarations over them. The
    /// grid resolves the collapse into
    /// it once the rule width is known
    /// ([`collapsed`]).
    pub(super) style: BoxStyle,
    /// The style its own element
    /// draws in. It carries no text,
    /// so only the cull slack rides on
    /// this.
    pub(super) base: Inline,
    /// `(row, column)`, filled in by
    /// [`place`].
    pub(super) at: (usize, usize),
    /// `(rowSpan, colSpan)`, at least
    /// one each and already clipped to
    /// the grid by [`place`].
    pub(super) span: (usize, usize),
    pub(super) path: Option<NodePath>,
}

impl Cell {
    /// Where in its dictionary it came
    /// from.
    pub(super) fn origin(&self, grid: &Grid) -> GlossOrigin {
        GlossOrigin { dict_id: grid.dict_id, entry_id: grid.entry_id, path: self.path }
    }
}

/// One table, before it is measured.
///
/// Structure only: which cell sits in
/// which slot needs no measurement,
/// so it is decided while the tree is
/// walked. What the grid pass adds is
/// the column widths, the row
/// heights, and where the whole thing
/// lands.
pub(super) struct Grid {
    /// Gap owed above it.
    pub(super) top_gap: f32,
    /// Every cell, in document order,
    /// each carrying the slot it was
    /// placed in.
    pub(super) cells: Vec<Cell>,
    pub(super) rows: usize,
    pub(super) cols: usize,
    /// The `table`'s own box, indent
    /// and alignment.
    pub(super) block: Block,
    /// The style its own element
    /// draws in.
    pub(super) base: Inline,
    pub(super) path: Option<NodePath>,
    pub(super) dict_id: i64,
    pub(super) entry_id: i64,
}

impl Grid {
    /// Where in its dictionary it came
    /// from.
    pub(super) fn origin(&self) -> GlossOrigin {
        GlossOrigin { dict_id: self.dict_id, entry_id: self.entry_id, path: self.path }
    }

    /// What every block container
    /// shares, as [`Pass::open_box`]
    /// takes it. A table is one: it
    /// carries a margin, a border and a
    /// padding around its grid exactly
    /// as a `div` does around its
    /// paragraphs.
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
/// HTML's own placement, minus the
/// parts a dictionary never writes: a
/// row's cells take the leftmost
/// column no `rowSpan` from above has
/// already claimed, and each then
/// claims `colSpan` columns across
/// and `rowSpan` rows down. A span
/// reaching past the last row ends at
/// it, so every cell's slot is inside
/// the grid it will be measured
/// against.
///
/// The grid is as wide as the
/// furthest column any row reached,
/// so a row holding more cells than
/// its neighbours widens the table
/// and the short rows simply end
/// early. That is the "renders what
/// it can" a malformed table is owed:
/// no cell is dropped and no index
/// can run off the end.
pub(super) fn place(rows: Vec<Vec<Cell>>) -> (Vec<Cell>, usize, usize) {
    let count = rows.len();
    // How far down each column is
    // already claimed, as the row
    // index one past the last claimed.
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
/// The schema writes these as
/// numbers, which is also the only
/// form the Anki HTML renderer reads
/// off them. Anything else, and any
/// value below one, is one cell:
/// HTML's own parser treats a missing
/// or zero span that way, and a cell
/// occupying no slot has nowhere to
/// draw.
pub(super) fn span_of(doc: &GlossDoc, id: NodeId, key: &str) -> usize {
    match doc.attr_of(id, key) {
        Some(Scalar::Num(n)) if n >= 1.0 => (n as usize).min(MAX_SPAN),
        _ => 1,
    }
}

/// The `len` tracks from `at`, plus
/// the rules between them.
///
/// What a cell spanning several
/// columns is offered, and what a cell
/// spanning several rows fills. Summed
/// from the track sizes rather than
/// differenced from two accumulated
/// edges, and that is load-bearing:
/// `(x0 + w + rule) - x0 - rule` is a
/// float ulp short of `w`, and a cell
/// one ulp narrower than its own text
/// wraps a line it should not.
pub(super) fn extent(tracks: &[f32], at: usize, len: usize, rule: f32) -> f32 {
    tracks[at..at + len].iter().sum::<f32>() + (len as f32 - 1.0) * rule
}

/// Grows `tracks` until every span
/// fits.
///
/// The one rule the columns and the
/// rows share, so it is written once:
/// a track takes the widest ask among
/// the cells that live only in it,
/// and a cell across several tracks
/// then adds only its *shortfall* -
/// what it needs beyond the tracks it
/// already covers, the `rule` between
/// them included - spread evenly over
/// them.
///
/// Spanning cells are visited once,
/// in document order, against the
/// tracks as the cells before them
/// left them. There is no fixpoint
/// and no second round: two passes
/// over the cells, and the answer.
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

/// One cell's box, with the shared
/// edges resolved.
///
/// `border-collapse: collapse` draws
/// the rule between two cells once,
/// so each cell owns the rules on its
/// left and its top and only the
/// cells against the grid's right and
/// bottom edges owe the closing ones.
/// Two neighbours therefore abut
/// exactly, with one `rule` between
/// them rather than two beside each
/// other - and a spanning cell simply
/// does not draw the rules it
/// swallowed.
///
/// An edge whose own `border-style`
/// is `none` still draws nothing: the
/// grid keeps the slot it reserved
/// and leaves it empty, exactly as a
/// browser does.
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

/// Moves a laid-out run of elements,
/// and the targets they earned, down
/// by `dy`.
///
/// A cell is laid out before its
/// row's height is known, because the
/// row is as tall as its tallest
/// cell, so the grid places every
/// cell at the table's own top and
/// drops it into its row afterwards.
///
/// Only origins move. A line box is
/// the measurer's answer and a
/// reading's position is
/// run-relative, so nothing a bin
/// re-measures changes - which is the
/// rule [`measure_readings`] sets and
/// the reason a row's extra height
/// never reaches the text inside it.
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

/// The table half of the gloss walk: a `table` subtree into a [`Grid`].
///
/// Structure only - which cell sits in which slot needs no measurement -
/// plus the Yomitan cell defaults, because a cell's box is a default
/// before it is a declaration.
impl Paragraphs<'_> {
    /// One `table`: its rows, as a
    /// grid.
    ///
    /// The table is also the block
    /// container Yomitan wraps one in
    /// (`.gloss-sc-table-container {
    /// display: block }`): it closes
    /// the paragraph before it and
    /// opens none of its own, because
    /// every word in a table belongs
    /// to a cell.
    ///
    /// A `table` inside a cell is a
    /// table inside a cell: a cell's
    /// content is a list of
    /// [`Piece`]s, and [`Pass::block`]
    /// lays a nested grid out through
    /// the same two methods the outer
    /// one used.
    pub(super) fn table(&mut self, id: NodeId, ctx: Ctx) {
        // A marker owed to an item
        // whose only content is a table
        // has no line inside the table
        // to hang beside, so it takes
        // the line above - which is
        // where a browser puts a
        // `::marker` beside a block too.
        // Inline, deliberately: a
        // hanging marker needs a line
        // box to share a baseline with,
        // and the paragraph this opens
        // would have none of its own.
        // A gutter with no line beside
        // it would drop the bullet.
        self.mark_inline();
        self.flush();
        let inline = self.styled(id, ctx.inline);
        let block = self.boxed(id, ctx.block, inline);
        let inner = Ctx {
            inline,
            // A cell pays the box and the
            // list indent once, through
            // the table that holds it -
            // the same reason
            // [`Block::inherited`] hands
            // a child an empty box.
            block: Block { indent: 0.0, ..block.inherited() },
            link: self.link_of(id, ctx.link),
            path: ctx.path,
        };
        let mut rows = Vec::new();
        self.rows_of(id, inner, false, &mut rows);
        let (cells, rows, cols) = place(rows);
        // A table of nothing draws
        // nothing, rather than an empty
        // rule.
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
        }));
    }

    /// Every row under a `table` or a
    /// row group, in document order.
    ///
    /// `thead`, `tbody` and `tfoot` are
    /// scaffolding rather than
    /// structure, and the parser
    /// already classes all three
    /// `Kind::Table`, so they
    /// contribute their rows and
    /// nothing else. A `thead` also
    /// carries Yomitan's header styling
    /// to the cells inside it, which is
    /// how a `td` in a header row comes
    /// out bold and tinted. `tfoot` is
    /// out of this ticket's scope and
    /// contributes its rows plainly,
    /// which loses no text and invents
    /// no styling for a shape the
    /// census counts zero of.
    ///
    /// Content a dictionary wrote
    /// outside any cell becomes **one**
    /// anonymous cell in **one**
    /// anonymous row, so a malformed
    /// table loses none of it and gains
    /// no structure either. That count
    /// is CSS 2.1 section 17.2.1's own
    /// repair and not a simplification
    /// of it: rule 2.1 wraps a child
    /// that is no proper table child
    /// "and all consecutive siblings of
    /// C that are not proper table
    /// children" in one anonymous row,
    /// and rule 2.3 wraps a run of
    /// non-cell children of that row in
    /// one anonymous cell. A `td`
    /// written straight under a `table`
    /// is no proper table child either,
    /// so it joins the same anonymous
    /// row as the runs around it and
    /// keeps its own slot in it.
    ///
    /// One cell per child instead is
    /// what turned 旺文社漢字典 第四版's
    /// radical index 90 degrees: that
    /// index is a `table` of 19
    /// `span`s, one per stroke-count
    /// group, and it came out as one
    /// row of 19 columns about 6 px
    /// wide each. A browser reads the
    /// dictionary's own stylesheet
    /// there and draws 19 rows of two
    /// columns; chibipop resolves no
    /// `display` at all, deliberately
    /// (`Tag::is_block`, and
    /// `display: grid` is the corpus's
    /// commonest declaration), so the
    /// anonymous-box repair is the
    /// reading it can hold - and it is
    /// the reading a browser falls back
    /// to with the stylesheet gone.
    /// Neither reading produces 19
    /// columns.
    ///
    /// An anonymous cell holding only
    /// the whitespace between two
    /// written cells is dropped, and
    /// with it the column it would
    /// otherwise have invented.
    pub(super) fn rows_of(&mut self, id: NodeId, ctx: Ctx, head: bool, out: &mut Vec<Vec<Cell>>) {
        let doc = self.doc;
        let mut stray: Vec<Cell> = Vec::new();
        // The run of consecutive
        // children the next anonymous
        // cell will hold, each with the
        // index its own address needs.
        // One buffer per table rather
        // than one per cell: a run is
        // closed at a boundary and
        // started again empty.
        let mut loose: Vec<(usize, NodeId)> = Vec::new();
        for (i, child) in doc.children(id).enumerate() {
            let tag = doc.node(child).tag;
            let row_level = matches!(tag, Tag::Tr | Tag::Thead | Tag::Tbody | Tag::Tfoot);
            // Every other tag joins the
            // run. A boundary - a
            // written cell, a row-level
            // tag, or the end below -
            // closes it into the one
            // anonymous cell it is owed.
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
                // A `td` or a `th`, and
                // nothing else: the guard
                // above took every other
                // tag into the run.
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
    /// A row's own box is not drawn.
    /// Under `border-collapse` a row's
    /// border collapses into its
    /// cells', which is where this grid
    /// resolves it, and Yomitan
    /// declares no `tr` rule at all -
    /// so the one real dictionary that
    /// writes a `tr` border writes a
    /// width with no style, which draws
    /// nothing in a browser either.
    pub(super) fn row(&mut self, id: NodeId, ctx: Ctx, head: bool) -> Vec<Cell> {
        let doc = self.doc;
        let inline = self.styled(id, ctx.inline);
        let block = self.boxed(id, ctx.block, inline);
        let inner = Ctx {
            inline,
            block: block.inherited(),
            link: self.link_of(id, ctx.link),
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

    /// The one anonymous cell a run of
    /// content written outside any cell
    /// collapses into, if it renders
    /// anything.
    ///
    /// An explicit empty `<td>` is a
    /// blank in a paradigm and keeps
    /// its border; an anonymous cell
    /// holding nothing is the
    /// whitespace a dictionary left
    /// between two written cells, and
    /// a border round it would invent a
    /// column.
    ///
    /// It draws no border and pays no
    /// padding either, because it is no
    /// `td`. Yomitan hangs its cell
    /// defaults on
    /// `.gloss-sc-th, .gloss-sc-td` and
    /// a `span` matches neither class,
    /// and CSS 2.1 section 17.2.1 gives
    /// an anonymous box the initial
    /// value of every property it does
    /// not inherit. Charging
    /// [`Paragraphs::cell_defaults`]
    /// here drew 19 boxes a browser
    /// leaves undrawn over 旺文社漢字典's
    /// radical index, and took 0.25em
    /// of padding per side out of
    /// columns narrower than that -
    /// which is what drove the wrap
    /// width onto its own 1 px floor
    /// and put every glyph of a group
    /// on a line of its own.
    ///
    /// The run's own declarations are
    /// not read into the cell's box
    /// either: they belong to the
    /// nodes, which
    /// [`Paragraphs::node`] resolves
    /// again inside the cell, and
    /// taking them twice would pay a
    /// margin twice.
    pub(super) fn implied(&mut self, ctx: Ctx, run: &[(usize, NodeId)]) -> Option<Cell> {
        if run.is_empty() {
            return None;
        }
        // An anonymous box has no
        // element, so the cell is
        // addressed by the table or the
        // row that generated it.
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
    /// The cell opens a paragraph of
    /// its own rather than joining the
    /// one before it, which `td` and
    /// `th` do not do anywhere else in
    /// this walk: they are in neither
    /// [`Tag::is_block`] nor
    /// [`Tag::is_inline`] precisely
    /// because a cell is a grid problem
    /// and not a line-break one. That
    /// paragraph carries the cell's own
    /// address, so a hit inside a
    /// conjugation table resolves to
    /// that cell's subtree.
    pub(super) fn cell(&mut self, id: NodeId, ctx: Ctx, head: bool) -> Cell {
        let doc = self.doc;
        let inline = self.styled(id, ctx.inline);
        // Yomitan tints a `th` and
        // everything inside a `thead`.
        // The bold half of the same rule
        // is [`tag_style`]'s, so a `td`
        // in a header row inherits its
        // weight from the `thead` and no
        // second rule belongs here.
        let tint = head || doc.node(id).tag == Tag::Th;
        let defaults = self.cell_defaults(ctx.block, inline, tint);
        let block = self.declared(id, defaults, inline.size);
        let inner = Ctx {
            inline,
            block: block.inherited(),
            link: self.link_of(id, ctx.link),
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

    /// One cell's paragraphs, collected
    /// off to one side: the grid
    /// measures each cell separately,
    /// so they cannot go into the
    /// stream the panel is stacking.
    ///
    /// A cell's first paragraph starts
    /// at the cell's own top; the ones
    /// under it are spaced as the panel
    /// spaces every other stacked
    /// paragraph.
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

    /// Yomitan's own cell box, before
    /// the dictionary speaks.
    ///
    /// The spec's defaults table,
    /// verbatim: a solid border of
    /// [`cell_border`] on every edge in
    /// the panel's rule colour,
    /// [`CELL_PADDING`] of padding, and
    /// a tinted background on a header
    /// cell. `vertical-align: top` is
    /// the other half of that table and
    /// needs no field - it is what
    /// [`Pass::table`] does by placing
    /// a short cell at its row's top.
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

/// The grid half of the geometry pass: a [`Grid`] into cells with rects.
///
/// The block half is in [`pass`](super::pass), and this is a second
/// `impl` block on its [`Pass`] rather than a type of its own for the
/// reason that type exists: every buffer a cell's paragraph reuses is
/// already in it.
impl<'a> Pass<'a> {
    /// One table, as a grid.
    ///
    /// Four steps, in order, and none
    /// of them iterates: one rule
    /// width for the whole grid, the
    /// column widths from the cells'
    /// content ([`Pass::columns`]),
    /// every cell laid out at its
    /// column with its y deferred, and
    /// then the row heights, which is
    /// the first moment a cell's own
    /// top is known.
    ///
    /// The table element itself leads
    /// the cells in draw order, so a
    /// declared background sits under
    /// them; its geometry is filled in
    /// last, because a shrink-to-fit
    /// table is exactly as wide as the
    /// grid inside it.
    ///
    /// The box around that grid is a
    /// `div`'s box and is opened and
    /// closed by the same two helpers
    /// ([`Pass::open_box`],
    /// [`Pass::close_box`]): a table
    /// differs only in what goes
    /// inside, and in taking the width
    /// its grid wants rather than the
    /// width it was offered.
    pub(super) fn table(
        &mut self,
        m: &mut dyn TextMeasure,
        grid: &'a Grid,
        at: (f32, f32),
        avail_w: f32,
    ) -> Result<(f32, f32), MeasureError> {
        let lead = self.open_box(ElemKind::Table, grid.header(), at, avail_w);
        let (x0, y0) = lead.pen;

        // One rule width for the whole
        // grid, the widest any cell
        // asked for. That *is*
        // `border-collapse`'s own
        // conflict rule - the wider
        // border wins the shared edge -
        // and one width per grid is
        // what makes the column
        // arithmetic a sum instead of a
        // negotiation.
        let rule = grid.cells.iter().fold(0.0f32, |w, cell| {
            let e = cell.style.border_used();
            w.max(e.top).max(e.right).max(e.bottom).max(e.left)
        });
        // A grid of `cols` columns has
        // `cols + 1` rules down it.
        let rules = rule * (grid.cols as f32 + 1.0);
        // Everything the columns have
        // between them. Never negative:
        // a table with more rules than
        // room collapses its columns
        // rather than reserving width
        // it does not have.
        let share = (lead.avail - rules).max(0.0);
        let inner = self.columns(m, grid, rule, share)?;

        // Column edges: the *outside*
        // of the rule before each
        // column, and one past the
        // last. Accumulated once and
        // read twice, so two
        // neighbours share one number
        // and abut exactly - an edge
        // differenced back out of a
        // running sum does not, and
        // `(x + rule) - rule` is not
        // `x`.
        let mut ex = Vec::with_capacity(grid.cols + 1);
        let mut x = x0;
        for w in &inner {
            ex.push(x);
            x += rule + w;
        }
        ex.push(x);

        // Every cell at its column,
        // with its y deferred to zero:
        // a row is as tall as its
        // tallest cell, so no cell's
        // own top is known until every
        // cell in that row has been
        // measured.
        let mut tall = Vec::with_capacity(grid.cells.len());
        let mut placed = Vec::with_capacity(grid.cells.len());
        for cell in &grid.cells {
            let (c, across) = (cell.at.1, cell.span.1);
            let pad = cell.style.padding;
            // A cell spanning columns
            // gets them and the rules it
            // swallowed, less its own
            // padding. Summed from the
            // widths rather than taken
            // off `ex`, because a wrap
            // width one ulp short of its
            // own text breaks a line
            // that fits (see [`extent`]).
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
            // The content, dropped into
            // the row the grid grew for
            // it. Nothing stretches: a
            // cell shorter than its row
            // sits at the row's top,
            // which is Yomitan's own
            // `vertical-align: top`, and
            // every line box is still
            // the one the measurer
            // reported.
            shift(
                &mut self.out[from + 1..to],
                &mut self.hits[hit_from..hit_to],
                ey[r] + rule + pad.top,
            );
            let style =
                collapsed(cell.style, rule, c + across == grid.cols, r + down == grid.rows);
            let used = style.border_used();
            // A cell that draws a rule
            // starts on its outside and
            // one that does not starts
            // on its inside, so the box
            // holds exactly the rules
            // this cell owns and its
            // neighbour's box begins
            // where this one ends.
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
        // Shrink-to-fit, as a table
        // with no declared width is in
        // a browser: the box is the
        // grid's own width and not the
        // container's. Conditional,
        // where a `div`'s is not: only
        // a block that declared a box
        // becomes a [`Boxed`], while
        // every table is a [`Grid`]
        // whether it declared one or
        // not.
        let block_box = lead.style.exists().then(|| ElemBox {
            rect: lead.border_box(lead.fitted_w(grid_w), grid_h),
            style: lead.style,
        });
        // The element's own extent is
        // the grid and not that box: a
        // table's margin and padding
        // are outside the cells a hit
        // test and a cull read.
        let rect = SceneRect { x: x0, y: y0, w: grid_w, h: grid_h };
        let advance = self.close_box(&lead, rect, block_box, grid_h);
        Ok((lead.demand(grid_w), advance))
    }

    /// Every column's width, from the
    /// content of the cells in it.
    ///
    /// Fixed sizing in one pass, and
    /// deliberately not CSS
    /// auto-layout:
    ///
    /// 1. Each cell is laid out once
    ///    at `share` - the whole width
    ///    the columns have between
    ///    them - and what it measures
    ///    to plus its own horizontal
    ///    padding is what it asks for.
    /// 2. A column takes the widest
    ///    ask among the single-column
    ///    cells in it, and a cell
    ///    spanning several adds only
    ///    its shortfall, spread evenly
    ///    ([`distribute`]).
    /// 3. If the columns together
    ///    still want more than
    ///    `share`, every one is
    ///    multiplied by the single
    ///    factor that makes them fit
    ///    exactly.
    ///
    /// Step 3 is what keeps a table
    /// inside the panel: the columns
    /// shrink and their content
    /// rewraps, so a wide table can
    /// never ask the panel to widen
    /// for it. There is no second
    /// round and no negotiation - the
    /// only thing measured again after
    /// this is each cell at its final
    /// width, which is the height
    /// pass.
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
            // An empty cell asks for
            // nothing and is not worth a
            // measurement - which is most
            // of a Jitendex forms table.
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
