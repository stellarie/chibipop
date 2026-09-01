//! The block geometry pass: how pieces stack, and how a box closes around
//! them.
//!
//! **One reason to change:** the block box model - what a container puts
//! between the width it was offered and the width its content is measured
//! at, and what it reports back up.
//!
//! [`Pass`] is the pass's whole state, held together because a gloss
//! paragraph is laid out from the panel's own element stream *and* from
//! inside a table cell: the code doing it has to be callable twice, and a
//! free function would have carried eight arguments, six of which never
//! change over a whole panel.
//!
//! The grid half of the same pass is in [`table`](super::table), with the
//! table structure it measures.

use crate::dict::gloss::NodePath;
use crate::ui::theme::Theme;
use super::flow::Flow;
use super::image::{measure_images, place_images};
use super::marker::{measure_markers, place_markers};
use super::measure::{LineBox, MeasureError, MeasureRun, Measured, StyledSpan, TextMeasure};
use super::pill::{measure_pills, place_pills};
use super::ruby::{measure_readings, place_ruby};
use super::scene::{
    box_elem, ElemBox, ElemKind, ElemSpan, GlossOrigin, HitTarget, SceneElem, SceneRect,
};
use super::style::{shift_on, Block, BoxStyle, Edges, Inline};
use super::table::Grid;

/// One popup's layout, in progress.
///
/// Two things at once, and
/// deliberately: the scene being
/// filled, and every buffer filling
/// it reuses. A gloss paragraph is
/// laid out from the panel's own
/// element stream *and* from inside a
/// table cell, so the code that does
/// it has to be callable twice - and
/// a free function would have carried
/// eight arguments, six of which
/// never change over a whole panel.
///
/// `'a` is the element stream's: the
/// spans handed to the seam borrow a
/// paragraph's own text, so the
/// scratch run cannot outlive the
/// paragraphs it was filled from.
pub(super) struct Pass<'a> {
    pub(super) font: &'a str,
    pub(super) theme: &'a Theme,
    /// One paragraph's measured lines
    /// and span boxes.
    pub(super) measured: Measured,
    /// One paragraph's spans, as the
    /// seam takes them.
    pub(super) run: Vec<StyledSpan<'a>>,
    /// The per-line boxes one run of
    /// spans covers - a link's, for
    /// its hit targets, and a pill's,
    /// for its box.
    pub(super) cover: Vec<Cover>,
    /// The scene so far, in draw
    /// order.
    pub(super) out: Vec<SceneElem>,
    /// The targets it has earned.
    pub(super) hits: Vec<HitTarget>,
}

/// The block half of the geometry pass: a paragraph, a box, and a run of
/// pieces stacked inside one.
///
/// The grid half is the second `impl` block, in
/// [`table`](super::table).
impl<'a> Pass<'a> {
    /// One gloss paragraph, measured,
    /// placed and pushed.
    ///
    /// `at` is the paragraph's own
    /// top-left, before its `top_gap`,
    /// and `avail_w` is what the
    /// container around it offers -
    /// already narrowed by any box
    /// that container is. Two answers
    /// come back: the width the
    /// paragraph would like, its ink
    /// plus the gutter its list
    /// reserved, which is what a table
    /// column's demand is built from -
    /// and the y the walk advances by,
    /// its gap included.
    pub(super) fn gloss(
        &mut self,
        m: &mut dyn TextMeasure,
        flow: &'a Flow,
        at: (f32, f32),
        avail_w: f32,
    ) -> Result<(f32, f32), MeasureError> {
        let (font, theme) = (self.font, self.theme);
        // A paragraph carries no box.
        // A block's box wraps *every*
        // paragraph the block emits
        // rather than the first one, so
        // it is a container of its own
        // ([`Boxed`], placed by
        // [`Pass::boxed`]) and the
        // paragraphs inside it are
        // measured at the width that
        // container has already
        // narrowed.
        //
        // What is left for a paragraph
        // to pay is the list indent:
        // the list's own
        // `--list-padding1`, inherited
        // rather than declared
        // ([`Block::indent`]). It
        // shifts the pen and narrows
        // the wrap, because every line
        // of a wrapped item - the first
        // and each continuation -
        // starts at the item's own
        // indent.
        debug_assert!(
            !flow.block.style.exists(),
            "a block's box belongs to its container, never to one of its paragraphs"
        );
        let indent = flow.block.indent;
        let wrap_w = (avail_w - indent).max(1.0);

        // The whole paragraph in one
        // request: its spans wrap
        // together, so a bold word and
        // a normal one beside it in the
        // source share a line and the
        // paragraph rewraps as one unit
        // (ADR-0013).
        self.run.clear();
        self.run.extend(flow.styled_spans(font));
        // The images first, because an
        // image occupies inline space
        // and grows the line it sits
        // on, and the only thing that
        // can do either is a span the
        // measurer is charged for.
        measure_images(m, font, flow, wrap_w, &mut self.run)?;
        // The readings next, by the same
        // rule: what a reading measures
        // to is what sizes the filler
        // span that buys it its slot, so
        // the paragraph below is
        // measured with the taller lines
        // already asked for, and `met.h`
        // counts the readings without
        // anything after the fact
        // touching it. A bin re-measuring
        // these same spans gets the same
        // lines.
        //
        // After the images and not
        // before, because a reading over
        // a gaiji adds its slot on top of
        // the rise that image already
        // asked for, and reads that rise
        // back off the run.
        let read = measure_readings(m, font, flow, wrap_w, &mut self.run)?;
        // Then the inline boxes, for the
        // third time on the same rule: an
        // inline box's own margin, border
        // and padding occupy inline space,
        // and the only thing that can is
        // a span the measurer is charged
        // for. Its spacers touch no
        // reading's and no image's, so
        // the three passes are
        // independent - each writes only
        // the sizes of spans it put in
        // the paragraph itself.
        measure_pills(m, flow, wrap_w, &mut self.run)?;
        m.measure(MeasureRun { spans: &self.run, max_w: wrap_w }, &mut self.measured)?;
        // The markers last, and after
        // the paragraph rather than
        // before it, which is the
        // difference between a marker
        // and a reading: a reading
        // grows the line it sits over
        // and so has to be priced into
        // the run, while a marker hangs
        // in a gutter the list's indent
        // already reserved and changes
        // nothing about the wrap. Its
        // own run, its own width.
        let marks = measure_markers(m, font, flow, wrap_w)?;
        let met = self.measured.metrics;
        let top = at.1 + flow.top_gap;
        let h = met.h;
        let pen = (at.0 + indent, top);
        // One offset per line, so a
        // centred paragraph that
        // wrapped centres each of its
        // lines and not just its ink
        // box.
        let slack = flow.block.align.slack_before();
        let line_at = |line: LineBox| pen.0 + (wrap_w - line.w).max(0.0) * slack;
        // Each reading over the base
        // it belongs to, in the slot
        // the lines already carry.
        let ruby = place_ruby(flow, &read, &self.measured, wrap_w, slack);
        // Each marker in the gutter of
        // the list that owed it, beside
        // the item's first line.
        let marker = place_markers(flow, &marks, &self.measured, indent, pen.0);
        // Each image over the spacer
        // run that bought its room, and
        // capped at the room the block
        // itself was given (ticket 24).
        let images = place_images(flow, &self.measured, pen, wrap_w, line_at);

        let spans = flow
            .spans
            .iter()
            .zip(self.run.iter())
            .enumerate()
            .map(|(i, (s, asked))| {
                // A span that wrapped is
                // placed against the
                // first line it touches;
                // one that measured to
                // nothing keeps the
                // shift its em alone
                // decided.
                let (line, span_h) = first_box(&self.measured, i as u32);
                ElemSpan {
                    at: s.at,
                    len: s.len,
                    color: s.style.color,
                    // The size the seam was
                    // handed, which for a
                    // ruby filler is not
                    // the size the walk
                    // resolved.
                    size: asked.size,
                    weight: s.style.weight,
                    italic: s.style.italic,
                    shift: shift_on(s.style, line, span_h),
                }
            })
            .collect();

        // One target per line a link
        // touches, so a cross-reference
        // that wrapped is clickable on
        // both halves of itself.
        for (i, action) in flow.links.iter().enumerate() {
            span_cover(&self.measured, &mut self.cover, |b| {
                flow.spans.get(b as usize).is_some_and(|s| s.link == i as u32)
            });
            for &(line, left, right, _) in &self.cover {
                let line = self.measured.lines[line as usize];
                self.hits.push(HitTarget {
                    x: Some(line_at(line) + left),
                    y: pen.1 + line.y,
                    w: Some(right - left),
                    h: line.h,
                    action: action.clone(),
                });
            }
        }

        // A pill per line its run
        // touches, in the room its own
        // spacer spans already bought
        // ([`place_pills`]).
        //
        // [`place_pills`]: super::pill::place_pills
        let inline_boxes =
            place_pills(flow, &self.measured, &mut self.cover, pen, line_at);

        let base = flow.base(theme);
        // Where the ink starts once the
        // block's own `text-align` has
        // moved its lines: the widest
        // line's leading edge, which is
        // the pen for an unaligned
        // paragraph ([`Align::Leading`]
        // has no slack).
        //
        // [`Align::Leading`]: super::scene::Align::Leading
        let ink_x = pen.0 + (wrap_w - met.w).max(0.0) * slack;
        // A reading wider than its base
        // overhangs it, and the ink box
        // is the ink: without this an
        // element would report a width
        // its own furigana exceeds.
        //
        // Measured from `ink_x` rather
        // than from `pen.0`, because
        // [`RubyBox::x`] is run-relative
        // and [`place_ruby`] has already
        // put its line's own alignment
        // slack into it. Folding it
        // against `met.w` and then
        // spending the answer as a width
        // from `ink_x` charged that slack
        // twice: a right-aligned poet's
        // name with five readings claimed
        // 380px of ink where it draws 41,
        // and 319px of the claim hung off
        // the panel.
        //
        // Only the right edge grows.
        // [`place_ruby`] clamps a reading
        // into the *wrap* box, which is
        // this box exactly when the
        // paragraph is unaligned; the
        // leading edge is an aligned run's
        // own origin and may not move
        // ([`SceneElem::rect`]).
        //
        // [`RubyBox::x`]: super::scene::RubyBox::x
        // [`SceneElem::rect`]: super::scene::SceneElem::rect
        let ink_w = ruby.iter().fold(met.w, |a, r| a.max(pen.0 + r.x + r.w - ink_x));
        // A marker adds nothing to it,
        // although it hangs left of
        // `pen.0`: the room it sits in
        // is the list's own
        // `--list-padding1`, already
        // counted in `lead.left` and so
        // already in the width demand
        // this returns. Charging for it
        // again would make a table
        // column ask for two gutters.
        self.out.push(SceneElem {
            kind: ElemKind::Text,
            text: flow.text.clone(),
            color: base.color,
            font_size: base.size,
            weight: base.weight,
            italic: base.italic,
            top_gap: flow.top_gap,
            wrap_w,
            align: flow.block.align,
            pen,
            rect: SceneRect { x: ink_x, y: pen.1, w: ink_w, h: met.h },
            lines: met.lines,
            advance: h,
            spans,
            ruby,
            marker,
            block_box: None,
            inline_boxes,
            origin: Some(GlossOrigin {
                dict_id: flow.dict_id,
                entry_id: flow.entry_id,
                path: flow.path,
            }),
            image: None,
        });
        // After the paragraph, so an
        // asset composites over the
        // spacer run it stands on
        // rather than under it. Each
        // advances nothing: the line
        // the riser grew is already
        // in `h`.
        self.out.extend(images);
        Ok((indent + ink_w, flow.top_gap + h))
    }

    /// One block's box, around every
    /// piece inside it.
    ///
    /// Ticket 08's own arithmetic with
    /// the body's total advance where a
    /// paragraph's height used to be,
    /// which is what makes one box
    /// around three paragraphs the same
    /// code as one box around one.
    ///
    /// Three lines, because a `div` and
    /// a `table` differ in what they
    /// put *inside* the box and in
    /// nothing about the box itself:
    /// [`Pass::open_box`] resolves the
    /// lead, [`Pass::close_box`] fills
    /// the geometry in, and what is
    /// left here is the body and the
    /// one rect a block box has that a
    /// grid's has not.
    pub(super) fn boxed(
        &mut self,
        m: &mut dyn TextMeasure,
        boxed: &'a Boxed,
        at: (f32, f32),
        avail_w: f32,
    ) -> Result<(f32, f32), MeasureError> {
        let lead = self.open_box(ElemKind::Block, boxed.header(), at, avail_w);
        let (want, body_h) = self.block(m, &boxed.body, lead.pen, lead.avail)?;
        // Full width, and not the
        // shrink-to-fit a grid gets:
        // `div` is `display: block`.
        let rect = lead.border_box(lead.offered_w(), body_h);
        // Unconditional, where a
        // paragraph's was conditional:
        // only a block that declared a
        // box becomes one of these
        // ([`Boxed::block`]).
        let advance =
            self.close_box(&lead, rect, Some(ElemBox { rect, style: lead.style }), body_h);
        Ok((lead.demand(want), advance))
    }

    /// Opens one block container: its
    /// lead resolved and its own
    /// element pushed, ahead of
    /// everything it frames.
    ///
    /// `at` is the top-left of the
    /// margin box before its own
    /// `top_gap`, and `avail_w` is what
    /// the container around it offers -
    /// the same two arguments a
    /// paragraph takes, so a box nests
    /// inside a box without either
    /// knowing.
    ///
    /// The box is resolved **before**
    /// anything in it is measured, and
    /// this is the whole of why it has
    /// to be: a box spanning several
    /// paragraphs must inset every one
    /// of them, so it narrows the width
    /// once, here, and its body never
    /// learns it is inside one. The
    /// element leads that body in draw
    /// order, so a declared background
    /// sits under every paragraph the
    /// box frames, and its geometry is
    /// filled in afterwards by
    /// [`Pass::close_box`], because a
    /// box is as tall as content
    /// measured at a width the box
    /// itself decided.
    pub(super) fn open_box(
        &mut self,
        kind: ElemKind,
        head: BoxHeader,
        at: (f32, f32),
        avail_w: f32,
    ) -> BoxLead {
        let style = head.block.style;
        let (margin, border, padding) = (style.margin, style.border_used(), style.padding);
        // Outside the box, as it is for
        // a paragraph and for a grid:
        // the indent is the list's own
        // padding, so it shifts this
        // box where the box's own
        // padding insets the content
        // inside it.
        let indent = head.block.indent;
        let lead = Edges {
            top: margin.top + border.top + padding.top,
            right: margin.right + border.right + padding.right,
            bottom: margin.bottom + border.bottom + padding.bottom,
            left: indent + margin.left + border.left + padding.left,
        };
        let top = at.1 + head.top_gap;
        let elem = self.out.len();
        self.out.push(box_elem(kind, head.base, head.block.align, head.origin));
        BoxLead {
            elem,
            style,
            border,
            lead,
            indent,
            offer: avail_w,
            corner: (at.0 + indent + margin.left, top + margin.top),
            pen: (at.0 + lead.left, top + lead.top),
            avail: (avail_w - lead.horizontal()).max(1.0),
            top_gap: head.top_gap,
        }
    }

    /// Closes one block container, and
    /// reports what the walk advances
    /// by.
    ///
    /// `rect` is the element's own
    /// extent - a `div`'s border box, a
    /// table's grid - and `block_box`
    /// is what a bin fills and strokes,
    /// which a table draws only when it
    /// declared one. `content_h` is the
    /// total advance of everything
    /// inside.
    ///
    /// The five field writes are the
    /// same five in both places, and
    /// the height arithmetic is
    /// [`BoxLead::advance`]'s: what the
    /// two containers disagree about is
    /// the two rects, and nothing else.
    pub(super) fn close_box(
        &mut self,
        lead: &BoxLead,
        rect: SceneRect,
        block_box: Option<ElemBox>,
        content_h: f32,
    ) -> f32 {
        let h = lead.advance(content_h);
        let elem = &mut self.out[lead.elem];
        elem.top_gap = lead.top_gap;
        elem.wrap_w = lead.avail;
        elem.pen = lead.pen;
        elem.rect = rect;
        elem.advance = h;
        elem.block_box = block_box;
        lead.top_gap + h
    }

    /// A run of pieces, stacked.
    ///
    /// What a table cell is: a block
    /// container holding paragraphs
    /// and, where a dictionary nested
    /// one, another table. Reports the
    /// widest piece and the total
    /// advance.
    pub(super) fn block(
        &mut self,
        m: &mut dyn TextMeasure,
        pieces: &'a [Piece],
        at: (f32, f32),
        avail_w: f32,
    ) -> Result<(f32, f32), MeasureError> {
        let (mut want, mut y) = (0.0f32, 0.0f32);
        for piece in pieces {
            let (w, advance) = match piece {
                Piece::Flow(flow) => self.gloss(m, flow, (at.0, at.1 + y), avail_w)?,
                Piece::Table(grid) => self.table(m, grid, (at.0, at.1 + y), avail_w)?,
                Piece::Boxed(boxed) => self.boxed(m, boxed, (at.0, at.1 + y), avail_w)?,
            };
            want = want.max(w);
            y += advance;
        }
        Ok((want, y))
    }

    /// How wide `pieces` would like to
    /// be, without keeping the layout
    /// that found out.
    ///
    /// A column's width comes from its
    /// cells' content and a cell's
    /// content cannot be measured
    /// without a width, so the grid
    /// lays each cell out once at the
    /// width every column shares,
    /// reads the extent, and rolls the
    /// elements and targets back off
    /// the scene. Truncating two
    /// vectors is the whole of the
    /// rollback, and it hands their
    /// capacity straight to the real
    /// pass.
    pub(super) fn trial(
        &mut self,
        m: &mut dyn TextMeasure,
        pieces: &'a [Piece],
        avail_w: f32,
    ) -> Result<f32, MeasureError> {
        let (elems, hits) = (self.out.len(), self.hits.len());
        let want = self.block(m, pieces, (0.0, 0.0), avail_w)?.0;
        self.out.truncate(elems);
        self.hits.truncate(hits);
        Ok(want)
    }
}

/// One piece of a row's gloss.
///
/// A small tree rather than a flat
/// list of paragraphs, because two of
/// the three shapes are containers. A
/// table's cells sit on a grid and
/// each has to be measured
/// separately, and a block's box
/// wraps *every* paragraph the block
/// emits rather than its first - so
/// the block half of the inline pass
/// emits this sum type and
/// [`Pass::block`] walks it.
pub(super) enum Piece {
    Flow(Flow),
    Table(Grid),
    /// Several pieces inside one
    /// block's box.
    Boxed(Boxed),
}

impl Piece {
    /// Sets the gap owed above it.
    ///
    /// It stops at a box rather than
    /// descending into one: the gap
    /// above a box goes *outside* it,
    /// and [`Paragraphs::wrap`] has
    /// already spaced the body inside
    /// it - whose first paragraph is
    /// separated from the border by
    /// the box's padding and by
    /// nothing else.
    ///
    /// [`Paragraphs::wrap`]: super::gloss::Paragraphs::wrap
    pub(super) fn top_gap(&mut self, gap: f32) {
        match self {
            Piece::Flow(flow) => flow.top_gap = gap,
            Piece::Table(grid) => grid.top_gap = gap,
            Piece::Boxed(boxed) => boxed.top_gap = gap,
        }
    }

    /// Stamps the term-bank row behind
    /// every paragraph of it, the ones
    /// inside cells included.
    ///
    /// The tree itself does not know
    /// which row stored it, so this is
    /// [`build_elements`]' job - and a
    /// cell's paragraph needs the same
    /// address as any other, or a hit
    /// inside a conjugation table
    /// resolves to no sense at all.
    ///
    /// [`build_elements`]: super::chrome::build_elements
    pub(super) fn stamp(&mut self, dict_id: i64, entry_id: i64) {
        match self {
            Piece::Flow(flow) => {
                flow.dict_id = dict_id;
                flow.entry_id = entry_id;
            }
            Piece::Table(grid) => {
                grid.dict_id = dict_id;
                grid.entry_id = entry_id;
                for cell in &mut grid.cells {
                    for piece in &mut cell.body {
                        piece.stamp(dict_id, entry_id);
                    }
                }
            }
            Piece::Boxed(boxed) => {
                boxed.dict_id = dict_id;
                boxed.entry_id = entry_id;
                for piece in &mut boxed.body {
                    piece.stamp(dict_id, entry_id);
                }
            }
        }
    }
}

/// One block's box, and every piece
/// inside it.
///
/// **Where a block's box belongs.** A
/// box wraps all of its block's
/// content, exactly as a browser
/// draws it: a bordered `div` holding
/// three paragraphs draws one border
/// around all three - not one around
/// each, and not one around the
/// first. So a block that declares a
/// box becomes a container, and the
/// paragraphs it emits become the
/// container's body.
///
/// Two invariants make a container
/// the *only* shape that can hold
/// such a box. The box has to be
/// resolved **before** anything
/// inside it is measured, because its
/// left and right lead decide the
/// wrap width (`wrap_w = avail_w -
/// lead.horizontal()`) - so it cannot
/// be discovered afterwards from
/// where its content landed. And
/// **nothing may edit a line box
/// after the wrap**, because both
/// bins re-measure an element's own
/// spans to paint it - so a box
/// cannot be grown later by
/// rewriting the paragraph that first
/// claimed it. A container satisfies
/// both: it narrows the width on the
/// way in and reads only the total
/// advance on the way out
/// ([`Pass::boxed`]).
///
/// The shape a table already is, for
/// the same reason: [`Cell::body`] is
/// a `Vec<Piece>` too, because a cell
/// is a block container as well.
///
/// [`Cell::body`]: super::table::Cell::body
pub(super) struct Boxed {
    /// Gap owed above it, outside its
    /// own margin.
    pub(super) top_gap: f32,
    /// Its own box, the list indent
    /// outside that box, and what its
    /// body inherits.
    ///
    /// `style.exists()` always holds:
    /// [`Paragraphs::wrap`] builds one
    /// of these only for a block that
    /// declared a box, so a gloss
    /// carrying no box style produces
    /// no container at all and
    /// measures byte for byte as it
    /// did before this pass existed.
    ///
    /// [`Paragraphs::wrap`]: super::gloss::Paragraphs::wrap
    pub(super) block: Block,
    /// What the box wraps, in draw
    /// order: paragraphs, tables, and
    /// nested boxes.
    pub(super) body: Vec<Piece>,
    /// The style its own element draws
    /// in. It carries no text, so only
    /// the cull slack rides on this.
    pub(super) base: Inline,
    pub(super) path: Option<NodePath>,
    pub(super) dict_id: i64,
    pub(super) entry_id: i64,
}

impl Boxed {
    /// Where in its dictionary it came
    /// from.
    pub(super) fn origin(&self) -> GlossOrigin {
        GlossOrigin { dict_id: self.dict_id, entry_id: self.entry_id, path: self.path }
    }

    /// What every block container
    /// shares, as [`Pass::open_box`]
    /// takes it.
    pub(super) fn header(&self) -> BoxHeader {
        BoxHeader {
            top_gap: self.top_gap,
            block: self.block,
            base: self.base,
            origin: self.origin(),
        }
    }
}

/// What every block container has
/// before it is measured.
///
/// The four facts [`Pass::open_box`]
/// needs and the only four a [`Boxed`]
/// and a [`Grid`] hold alike, bundled
/// so that opening a box is four
/// arguments rather than seven - which
/// is where an argument gets passed in
/// the wrong slot, and where two
/// `(f32, f32)` origins in a row get
/// swapped.
#[derive(Clone, Copy)]
pub(super) struct BoxHeader {
    /// Gap owed above it, outside its
    /// own margin.
    pub(super) top_gap: f32,
    /// Its own box, the list indent
    /// outside that box, and what its
    /// body inherits.
    pub(super) block: Block,
    /// The style its own element draws
    /// in. It carries no text, so only
    /// the cull slack rides on this.
    pub(super) base: Inline,
    /// Where in its dictionary it came
    /// from.
    pub(super) origin: GlossOrigin,
}

/// One opened block container: its
/// lead resolved, its element pushed,
/// and everything closing it needs.
///
/// A `div` and a `table` differ in
/// what they put inside the box and in
/// nothing about the box itself, so
/// this is the box itself: the
/// arithmetic between the width a
/// container was offered and the width
/// its content is measured at, and
/// back again. Both sites had it
/// written out, and a margin honoured
/// on one of them and not the other
/// would be a border drawn in the
/// wrong place on exactly the
/// dictionaries that draw borders.
#[derive(Clone, Copy)]
pub(super) struct BoxLead {
    /// Where the box's own element sits
    /// in the scene, so the close can
    /// fill its geometry in.
    pub(super) elem: usize,
    /// The declared box, as a bin
    /// fills and strokes it.
    pub(super) style: BoxStyle,
    /// Used border widths: an edge
    /// whose style is `none` is zero
    /// however wide it was declared.
    /// Resolved once here, because
    /// closing the box reads it four
    /// more times.
    pub(super) border: Edges<f32>,
    /// Margin plus border plus padding
    /// per edge, with the block's own
    /// list indent folded into `left`.
    pub(super) lead: Edges<f32>,
    /// That list indent alone, which
    /// the border box's own x still
    /// needs: the indent sits *outside*
    /// the box.
    pub(super) indent: f32,
    /// What the container offered,
    /// before the lead came off it.
    pub(super) offer: f32,
    /// The border box's own top-left.
    pub(super) corner: (f32, f32),
    /// The content box's top-left: the
    /// pen every piece inside is laid
    /// out from.
    pub(super) pen: (f32, f32),
    /// The width every piece inside is
    /// measured at.
    pub(super) avail: f32,
    /// The gap above this box, already
    /// spent into `corner` and `pen`
    /// and owed back to the walk.
    pub(super) top_gap: f32,
}

impl BoxLead {
    /// This box's border box, with
    /// `content_h` pixels of content
    /// inside it.
    ///
    /// `w` is the border box's own
    /// width, which is the one thing
    /// the two containers disagree
    /// about: see [`BoxLead::offered_w`]
    /// and [`BoxLead::fitted_w`].
    pub(super) fn border_box(&self, w: f32, content_h: f32) -> SceneRect {
        SceneRect {
            x: self.corner.0,
            y: self.corner.1,
            w,
            h: self.border.vertical() + self.style.padding.vertical() + content_h,
        }
    }

    /// The border-box width a
    /// `display: block` container
    /// takes: everything its container
    /// offered, less the list indent
    /// outside the box and its own
    /// margins.
    pub(super) fn offered_w(&self) -> f32 {
        (self.offer - self.indent - self.style.margin.horizontal()).max(0.0)
    }

    /// The border-box width a
    /// shrink-to-fit container takes:
    /// its content, plus the border and
    /// padding around it. What a table
    /// with no declared width is in a
    /// browser.
    pub(super) fn fitted_w(&self, content_w: f32) -> f32 {
        self.border.horizontal() + self.style.padding.horizontal() + content_w
    }

    /// What the walk advances by, once
    /// the content is `content_h` tall.
    ///
    /// Not the border box's own height:
    /// the margin below the box is part
    /// of what the next block starts
    /// after, and the gap above it was
    /// already spent when the box
    /// opened.
    pub(super) fn advance(&self, content_h: f32) -> f32 {
        self.lead.top
            + content_h
            + self.style.padding.bottom
            + self.border.bottom
            + self.style.margin.bottom
    }

    /// The width this container asks
    /// its own container for: what its
    /// content wanted, plus the lead
    /// around it.
    pub(super) fn demand(&self, content_w: f32) -> f32 {
        self.lead.horizontal() + content_w
    }
}

/// The line a span first landed on,
/// and the advance it asked that line
/// for.
///
/// A degenerate `(0, 0)` for a span
/// the measurer reported no box for -
/// an empty run, or one whose glyphs
/// all fell outside it - which
/// [`shift_on`] reads as "no line to
/// align against".
pub(super) fn first_box(measured: &Measured, span: u32) -> (LineBox, f32) {
    match measured.spans.iter().find(|b| b.span == span) {
        Some(b) => (
            measured.lines.get(b.line as usize).copied().unwrap_or_default(),
            b.h,
        ),
        None => (LineBox::default(), 0.0),
    }
}

/// One run of spans, on one line.
///
/// `(line, left, right, height)`, all
/// run-relative: the line index, the
/// run's horizontal extent on it, and
/// the tallest span box in it - which
/// is what a hit target's rect and an
/// inline box's rect are each built
/// from.
pub(super) type Cover = (u32, f32, f32, f32);

/// Where a run of spans landed, one
/// entry per line it touches.
///
/// A span that wrapped has one box
/// per line, and a run of them has
/// several per line, so this unions
/// them: a cross-reference broken
/// across two lines is clickable on
/// both halves and a pill that
/// wrapped is drawn on both. `out` is
/// refilled rather than returned,
/// because the walk does this twice
/// per paragraph.
pub(super) fn span_cover(measured: &Measured, out: &mut Vec<Cover>, wanted: impl Fn(u32) -> bool) {
    out.clear();
    for b in &measured.spans {
        if !wanted(b.span) {
            continue;
        }
        match out.iter_mut().find(|(line, ..)| *line == b.line) {
            Some(seen) => {
                seen.1 = seen.1.min(b.x);
                seen.2 = seen.2.max(b.x + b.w);
                seen.3 = seen.3.max(b.h);
            }
            None => out.push((b.line, b.x, b.x + b.w, b.h)),
        }
    }
}
