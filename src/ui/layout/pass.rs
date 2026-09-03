//! The block geometry pass measures how block pieces stack and how a box surrounds them.
//!
//! **One reason to change:** the block box model.
//! A container subtracts its box from the offered width, measures its content, and reports its width demand.
//!
//! [`Pass`] stores the scene and reusable buffers for one popup.
//! A gloss paragraph can start in the panel stream or inside a table cell.
//! The same layout path handles both cases.
//! A free function would need eight arguments. Six arguments would stay constant across one panel.
//!
//! The grid half uses the same pass in [`table`](super::table).
//! That module measures the table structure.

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

/// The layout state for one popup during the pass.
///
/// [`Pass`] stores the scene that the pass fills and the buffers that it reuses.
/// A gloss paragraph can start in the panel stream or inside a table cell.
/// The same layout path handles both cases.
/// A free function would need eight arguments. Six arguments would stay constant across one panel.
///
/// The lifetime `'a` belongs to the element stream.
/// The pass sends spans to the measurer, and each span borrows text from a paragraph.
/// Therefore, the scratch run cannot outlive those paragraphs.
pub(super) struct Pass<'a> {
    pub(super) font: &'a str,
    pub(super) theme: &'a Theme,
    /// One paragraph's measured lines
    /// and span boxes.
    pub(super) measured: Measured,
    /// Spans for one paragraph, in the form that the measurer accepts.
    pub(super) run: Vec<StyledSpan<'a>>,
    /// Per-line boxes that one span run covers.
    /// A link uses them for hit targets. A pill uses them for its box.
    pub(super) cover: Vec<Cover>,
    /// Scene elements in draw order.
    pub(super) out: Vec<SceneElem>,
    /// Hit targets that this pass has added.
    pub(super) hits: Vec<HitTarget>,
}

/// The block half of the geometry pass.
/// It lays out paragraphs, boxes, and stacked pieces.
///
/// The grid half uses a second `impl` block in [`table`](super::table).
impl<'a> Pass<'a> {
    /// Measures and places one gloss paragraph, then pushes it into the scene.
    ///
    /// `at` gives the paragraph's top-left point before the pass adds `top_gap`.
    /// `avail_w` gives the width that the parent container offers.
    /// If the parent container is a box, it has already reduced `avail_w`.
    ///
    /// The function returns the width demand and the vertical advance.
    /// The width includes paragraph ink and the list gutter.
    /// The advance includes the paragraph's own gap.
    pub(super) fn gloss(
        &mut self,
        m: &mut dyn TextMeasure,
        flow: &'a Flow,
        at: (f32, f32),
        avail_w: f32,
    ) -> Result<(f32, f32), MeasureError> {
        let (font, theme) = (self.font, self.theme);
        // A paragraph never carries a box.
        // A block's box surrounds every paragraph that the block emits, not only its first paragraph.
        // The separate container [`Boxed`] owns that box, and [`Pass::boxed`] measures its paragraphs.
        // The container already narrows their width.
        //
        // A paragraph still adds the list indent.
        // The list owns the `--list-padding1` value, and [`Block::indent`] carries it to the paragraph.
        // The indent shifts the pen and narrows the wrap.
        // Every line in a wrapped list item starts at the item's indent.
        debug_assert!(
            !flow.block.style.exists(),
            "a block's box belongs to its container, never to one of its paragraphs"
        );
        let indent = flow.block.indent;
        let wrap_w = (avail_w - indent).max(1.0);

        // The pass measures the whole paragraph in one request.
        // Its spans wrap as one group, so words with different styles can share a line.
        // The paragraph rewraps as one unit.
        self.run.clear();
        self.run.extend(flow.styled_spans(font));
        // The pass measures images first because each image uses inline space.
        // Each image also increases its line's height.
        // Only a span that the pass sends to the measurer can provide both effects.
        measure_images(m, font, flow, wrap_w, &mut self.run)?;
        // The pass measures readings after images.
        // A reading sets the size of its filler span, which reserves its slot.
        // The paragraph measurement therefore includes the taller line.
        // `met.h` includes this height. A bin that measures the same spans for paint gets the same lines.
        //
        // A reading over gaiji adds its slot after the image rise.
        // The pass reads that rise from the run.
        let read = measure_readings(m, font, flow, wrap_w, &mut self.run)?;
        // The pass measures inline boxes after readings.
        // Each inline box uses its margin, border, and padding as inline space.
        // Only a span that the pass sends to the measurer can reserve that space.
        // A pill's spacer spans do not touch reading or image spacer spans.
        // Therefore, each measurement pass changes only the spans that it adds.
        measure_pills(m, flow, wrap_w, &mut self.run)?;
        m.measure(MeasureRun { spans: &self.run, max_w: wrap_w }, &mut self.measured)?;
        // The pass measures markers after the paragraph.
        // A reading increases the height of its line, so the pass measures it before the paragraph.
        // A marker uses the gutter that the list indent reserves.
        // It does not change the wrap. It has its own run and width.
        let marks = measure_markers(m, font, flow, wrap_w)?;
        let met = self.measured.metrics;
        let top = at.1 + flow.top_gap;
        let h = met.h;
        let pen = (at.0 + indent, top);
        // This closure gives each line an x offset.
        // A centered paragraph centers each wrapped line, not only its overall ink box.
        let slack = flow.block.align.slack_before();
        let line_at = |line: LineBox| pen.0 + (wrap_w - line.w).max(0.0) * slack;
        // `place_ruby` puts each reading over its base text.
        // Each reading uses the slot that the line already reserves.
        let ruby = place_ruby(flow, &read, &self.measured, wrap_w, slack);
        // `place_markers` puts each marker in the list gutter.
        // Each marker sits beside the item's first line.
        let marker = place_markers(flow, &marks, &self.measured, indent, pen.0);
        // `place_images` puts each image over the spacer run that reserves its room.
        // It caps each image at the room that the block receives.
        let images = place_images(flow, &self.measured, pen, wrap_w, line_at);

        let spans = flow
            .spans
            .iter()
            .zip(self.run.iter())
            .enumerate()
            .map(|(i, (s, asked))| {
                // The pass places a wrapped span against its first line.
                // If measurement returns no box, the span keeps the shift from its own em size.
                let (line, span_h) = first_box(&self.measured, i as u32);
                ElemSpan {
                    at: s.at,
                    len: s.len,
                    color: s.style.color,
                    // This is the size that the pass sends to the measurer.
                    // For a ruby filler, it differs from the size that the walk resolves.
                    size: asked.size,
                    weight: s.style.weight,
                    italic: s.style.italic,
                    shift: shift_on(s.style, line, span_h),
                }
            })
            .collect();

        // The loop creates one hit target for each line that a link touches.
        // A wrapped cross-reference therefore stays clickable on both parts.
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

        // [`place_pills`] draws one pill for each line that its run touches.
        // Each pill uses the space that its spacer spans reserve.
        //
        // [`place_pills`]: super::pill::place_pills
        let inline_boxes =
            place_pills(flow, &self.measured, &mut self.cover, pen, line_at);

        let base = flow.base(theme);
        // `ink_x` marks the start of the ink after `text-align` shifts its lines.
        // It is the leading edge of the widest line.
        // For an unaligned paragraph, it equals the pen because [`Align::Leading`] adds no slack.
        //
        // [`Align::Leading`]: super::scene::Align::Leading
        let ink_x = pen.0 + (wrap_w - met.w).max(0.0) * slack;
        // A reading can extend beyond its base text.
        // The ink box must include that overhang, or the element reports too little width.
        //
        // Measure the overhang from `ink_x`, not from `pen.0`.
        // [`RubyBox::x`] uses coordinates relative to the run.
        // [`place_ruby`] already adds each line's alignment slack.
        // If code folds the reading width into `met.w` and measures it from `ink_x`, it counts that slack twice.
        //
        // One case exposed this bug.
        // A right-aligned poet's name had five readings.
        // The name used 41 pixels of ink, but the old code reported 380 pixels.
        // The old code placed 319 pixels of the wrong claim beside the panel.
        //
        // Only the right edge grows.
        // [`place_ruby`] clamps each reading to the wrap box.
        // The wrap box equals this ink box only when the paragraph has no alignment.
        // The leading edge anchors an aligned run and must not move. See [`SceneElem::rect`].
        //
        // [`RubyBox::x`]: super::scene::RubyBox::x
        // [`SceneElem::rect`]: super::scene::SceneElem::rect
        let ink_w = ruby.iter().fold(met.w, |a, r| a.max(pen.0 + r.x + r.w - ink_x));
        // A marker does not add to `ink_w`, even when it hangs left of `pen.0`.
        // The list's `--list-padding1` reserves its space.
        // `lead.left` already counts that space.
        // The returned width demand therefore includes it.
        // A second charge would give a table column two gutters.
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
                entry: flow.entry,
                path: flow.path,
            }),
            image: None,
            sources: flow.sources.clone(),
        });
        // The pass adds images after the paragraph.
        // Each image therefore composites over its spacer run, not under it.
        // Each image advances the walk by zero because the riser already increases the line height.
        // `h` already includes that increase.
        self.out.extend(images);
        Ok((indent + ink_w, flow.top_gap + h))
    }

    /// Builds one block's box around every piece in that block.
    ///
    /// The box uses the body's total advance, not one paragraph's height.
    /// This lets one box surround one paragraph or three paragraphs.
    ///
    /// A `div` and a `table` differ only in their box contents.
    /// [`Pass::open_box`] resolves the lead, and [`Pass::close_box`] fills the geometry.
    /// This function lays out the body and supplies the rect that a block box has but a grid does not.
    pub(super) fn boxed(
        &mut self,
        m: &mut dyn TextMeasure,
        boxed: &'a Boxed,
        at: (f32, f32),
        avail_w: f32,
    ) -> Result<(f32, f32), MeasureError> {
        let lead = self.open_box(ElemKind::Block, boxed.header(), at, avail_w);
        let (want, body_h) = self.block(m, &boxed.body, lead.pen, lead.avail)?;
        // This rect uses the full offered width, not the grid's shrink-to-fit width.
        // A `div` uses `display: block`.
        let rect = lead.border_box(lead.offered_w(), body_h);
        // This box always exists here.
        // A paragraph has a box only when its block declares one.
        // That block becomes a `Boxed` piece through [`Boxed::block`].
        let advance =
            self.close_box(&lead, rect, Some(ElemBox { rect, style: lead.style }), body_h);
        Ok((lead.demand(want), advance))
    }

    /// Opens one block container and pushes its element before the framed content.
    ///
    /// `at` gives the margin box's top-left before `top_gap`.
    /// `avail_w` gives the width that the parent container offers.
    /// These match the arguments for a paragraph, so boxes can nest.
    ///
    /// The box resolves before the pass measures its content.
    /// One box can contain several paragraphs. It narrows their width once.
    /// The element comes first in draw order, so its background stays below the content.
    /// [`Pass::close_box`] fills its geometry after the pass measures the content.
    /// The pass measures content at that width, then uses its height for the box.
    pub(super) fn open_box(
        &mut self,
        kind: ElemKind,
        head: BoxHeader,
        at: (f32, f32),
        avail_w: f32,
    ) -> BoxLead {
        let style = head.block.style;
        let (margin, border, padding) = (style.margin, style.border_used(), style.padding);
        // The list indent stays outside the box for paragraphs and grids.
        // It shifts the box, while the box's padding insets the content.
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

    /// Closes one block container and returns its vertical advance.
    ///
    /// `rect` gives the element's extent: a `div` border box or a table grid.
    /// `block_box` gives the box that a bin fills and strokes.
    /// A table has this box only when its style declares one.
    /// `content_h` gives the total advance of its content.
    ///
    /// Both callers update the same five fields, and [`BoxLead::advance`] supplies the height.
    /// The callers differ only in their rect values.
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

    /// Lays out a stacked run of pieces.
    ///
    /// A table cell is a block container with paragraphs and any nested table.
    /// The function returns the widest piece and the total advance.
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

    /// Measures how wide `pieces` need to be without keeping the layout.
    ///
    /// A grid must measure each cell at the shared column width before it knows each column's width.
    /// The trial reads each extent, then removes temporary elements and targets from the scene.
    /// It truncates two vectors and keeps their capacity for the real pass.
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
/// The layout uses a small tree instead of a flat paragraph list because two of three shapes are containers.
/// Table cells need separate measurement, and a block's box surrounds *every* paragraph it emits.
/// [`Pass::block`] walks this sum type.
pub(super) enum Piece {
    Flow(Flow),
    Table(Grid),
    /// Several pieces inside one
    /// block's box.
    Boxed(Boxed),
}

impl Piece {
    /// Sets the gap above a piece.
    ///
    /// It stops at a box and does not enter it.
    /// The gap above a box stays outside it.
    /// [`Paragraphs::wrap`] already spaces its body inside the box.
    /// The box's padding alone separates the first paragraph from the border.
    ///
    /// [`Paragraphs::wrap`]: super::gloss::Paragraphs::wrap
    pub(super) fn top_gap(&mut self, gap: f32) {
        match self {
            Piece::Flow(flow) => flow.top_gap = gap,
            Piece::Table(grid) => grid.top_gap = gap,
            Piece::Boxed(boxed) => boxed.top_gap = gap,
        }
    }

    /// Stamps the term-bank row on every paragraph.
    /// This includes paragraphs inside cells.
    ///
    /// The tree cannot identify the stored row, so [`build_elements`] adds the address.
    /// A paragraph in a conjugation table needs the same address as any other paragraph.
    /// Without that address, a hit resolves to no sense.
    ///
    /// [`build_elements`]: super::chrome::build_elements
    pub(super) fn stamp(&mut self, dict_id: i64, entry_id: i64, entry: u32) {
        match self {
            Piece::Flow(flow) => {
                flow.dict_id = dict_id;
                flow.entry_id = entry_id;
                flow.entry = entry;
            }
            Piece::Table(grid) => {
                grid.dict_id = dict_id;
                grid.entry_id = entry_id;
                grid.entry = entry;
                for cell in &mut grid.cells {
                    for piece in &mut cell.body {
                        piece.stamp(dict_id, entry_id, entry);
                    }
                }
            }
            Piece::Boxed(boxed) => {
                boxed.dict_id = dict_id;
                boxed.entry_id = entry_id;
                boxed.entry = entry;
                for piece in &mut boxed.body {
                    piece.stamp(dict_id, entry_id, entry);
                }
            }
        }
    }
}

/// One block's box and every piece inside it.
///
/// **Where a block's box belongs.** A box surrounds all content from its block.
/// A bordered `div` with three paragraphs therefore has one border around all three.
/// It does not have one border per paragraph.
///
/// A block that declares a box becomes a container.
/// Its paragraphs become the container's body.
///
/// Two invariants make a container the *only* shape that can hold this box.
/// The pass must resolve the box before it measures the content.
/// Its left and right lead set `wrap_w = avail_w - lead.horizontal()`.
/// The pass cannot discover that width from content after layout.
///
/// **the pass must not edit a line box after the wrap**.
/// Both bins measure an element's spans again for paint.
/// Therefore, the pass cannot grow the box by changing the first paragraph later.
///
/// A container satisfies both rules.
/// It narrows the width before it measures content and reads only total advance after it measures content.
/// See [`Pass::boxed`].
///
/// A table already has this shape.
/// [`Cell::body`] is a `Vec<Piece>`, so a cell is also a block container.
///
/// [`Cell::body`]: super::table::Cell::body
pub(super) struct Boxed {
    /// Gap above the box, outside its margin.
    pub(super) top_gap: f32,
    /// The box, the list indent outside it, and the values that its body inherits.
    ///
    /// `style.exists()` always holds.
    /// [`Paragraphs::wrap`] creates a `Boxed` value only for a block that declares a box.
    /// A gloss without box style creates no container.
    /// Its measurement remains byte for byte as before this pass.
    ///
    /// [`Paragraphs::wrap`]: super::gloss::Paragraphs::wrap
    pub(super) block: Block,
    /// The box contents in draw order: paragraphs, tables, and nested boxes.
    pub(super) body: Vec<Piece>,
    /// The style for the box element.
    /// The element has no text, so only the cull slack uses this style.
    pub(super) base: Inline,
    pub(super) path: Option<NodePath>,
    pub(super) dict_id: i64,
    pub(super) entry_id: i64,
    pub(super) entry: u32,
}

impl Boxed {
    /// Returns this element's `GlossOrigin` in the Dictionary.
    pub(super) fn origin(&self) -> GlossOrigin {
        GlossOrigin {
            dict_id: self.dict_id,
            entry_id: self.entry_id,
            entry: self.entry,
            path: self.path,
        }
    }

    /// Returns the header that every block container shares with [`Pass::open_box`].
    pub(super) fn header(&self) -> BoxHeader {
        BoxHeader {
            top_gap: self.top_gap,
            block: self.block,
            base: self.base,
            origin: self.origin(),
        }
    }
}

/// The data that every block container has before measurement.
///
/// [`Pass::open_box`] needs four facts.
/// [`Boxed`] and [`Grid`] hold the same four facts.
/// One header keeps these facts together instead of seven arguments.
/// It prevents wrong-slot errors, such as swapped `(f32, f32)` origins.
#[derive(Clone, Copy)]
pub(super) struct BoxHeader {
    /// Gap above the box, outside its margin.
    pub(super) top_gap: f32,
    /// The box, the list indent outside it, and the values that its body inherits.
    pub(super) block: Block,
    /// The style for the box element.
    /// The element has no text, so only the cull slack uses this style.
    pub(super) base: Inline,
    /// The element's `GlossOrigin` in the Dictionary.
    pub(super) origin: GlossOrigin,
}

/// The state of one open block container.
///
/// It stores the resolved lead, the element index, and the values that `close_box` needs.
///
/// A `div` and a `table` differ only in their contents.
/// This struct shares one box calculation between both containers.
/// One shared calculation keeps margin handling consistent for dictionaries that draw borders.
#[derive(Clone, Copy)]
pub(super) struct BoxLead {
    /// Index of the box element in the scene.
    /// `close_box` fills this element's geometry.
    pub(super) elem: usize,
    /// The declared box style that a bin fills and strokes.
    pub(super) style: BoxStyle,
    /// Border widths after style resolution.
    /// An edge with style `none` has width zero.
    /// `close_box` reads all four values, so the pass resolves them once.
    pub(super) border: Edges<f32>,
    /// Margin, border, and padding for each edge.
    /// The block's list indent is part of `left`.
    pub(super) lead: Edges<f32>,
    /// The list indent alone.
    /// It stays outside the border box and affects its x coordinate.
    pub(super) indent: f32,
    /// Width that the container offers before the pass removes the lead.
    pub(super) offer: f32,
    /// Top-left point of the border box.
    pub(super) corner: (f32, f32),
    /// Top-left point of the content box.
    /// Each piece uses this point as its layout origin.
    pub(super) pen: (f32, f32),
    /// Width that the pass gives to each piece.
    pub(super) avail: f32,
    /// Gap above the box.
    /// The pass adds it to `corner` and `pen`, then returns it to the walk.
    pub(super) top_gap: f32,
}

impl BoxLead {
    /// Returns the border box with `content_h` pixels inside.
    ///
    /// `w` gives the border-box width.
    /// The block and grid containers use different width rules.
    /// See [`BoxLead::offered_w`] and [`BoxLead::fitted_w`].
    pub(super) fn border_box(&self, w: f32, content_h: f32) -> SceneRect {
        SceneRect {
            x: self.corner.0,
            y: self.corner.1,
            w,
            h: self.border.vertical() + self.style.padding.vertical() + content_h,
        }
    }

    /// Returns the border-box width for a `display: block` container.
    /// It subtracts the list indent outside the box and the box's margins from the offered width.
    pub(super) fn offered_w(&self) -> f32 {
        (self.offer - self.indent - self.style.margin.horizontal()).max(0.0)
    }

    /// Returns the border-box width for a shrink-to-fit container.
    /// It adds content, border, and padding.
    /// A table with no declared width uses this rule in a browser.
    pub(super) fn fitted_w(&self, content_w: f32) -> f32 {
        self.border.horizontal() + self.style.padding.horizontal() + content_w
    }

    /// Returns the distance that the walk advances after the content reaches `content_h`.
    ///
    /// The distance exceeds the border box height because the next block starts after the bottom margin.
    /// The box already spent its top gap when it opened.
    pub(super) fn advance(&self, content_h: f32) -> f32 {
        self.lead.top
            + content_h
            + self.style.padding.bottom
            + self.border.bottom
            + self.style.margin.bottom
    }

    /// Returns the width that this container requests from its parent.
    /// It adds `content_w` to the lead around the content.
    pub(super) fn demand(&self, content_w: f32) -> f32 {
        self.lead.horizontal() + content_w
    }
}

/// Returns the first line that holds a span and the advance that it requests.
///
/// If the measurer reports no box, this function returns `(0, 0)` for an empty run or a run whose glyphs fall outside it.
/// [`shift_on`] reads that result as no line for alignment.
pub(super) fn first_box(measured: &Measured, span: u32) -> (LineBox, f32) {
    match measured.spans.iter().find(|b| b.span == span) {
        Some(b) => (
            measured.lines.get(b.line as usize).copied().unwrap_or_default(),
            b.h,
        ),
        None => (LineBox::default(), 0.0),
    }
}

/// One run of spans on one line.
///
/// `(line, left, right, height)` stores run-relative data: the line index, horizontal extent, and tallest span box.
/// Hit target and inline box rects use this data.
pub(super) type Cover = (u32, f32, f32, f32);

/// Builds one `span-cover` entry for each line that a run touches.
///
/// A span that wraps has one box per line.
/// A run can have several boxes per line, so this function unions their extents.
/// A cross-reference across two lines stays clickable on both parts.
/// A wrapped pill draws on both lines.
/// `out` receives the result because the walk calls this twice per paragraph.
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
