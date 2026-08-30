//! One term-bank row's parsed tree, walked into paragraphs.
//!
//! **One reason to change:** what a `GlossDoc` subtree becomes. A tag this
//! build does not yet render, a role rule, a Yomitan default - each is one
//! arm of the descent in this file.
//!
//! The block half of the inline pass, and deliberately the same rules the
//! plain-text renderer uses (`dict::gloss::plain`): a block tag or a `data`
//! marker opens a paragraph, an inline tag adds spans to the one already
//! open, and a paragraph with no text is dropped. Two renderers over one
//! tree must not disagree about where the lines are - that disagreement is
//! the bug class this pass exists to close.
//!
//! [`Paragraphs`] is the walk's whole state, and four of its arms live
//! beside the thing they build rather than here: a list's in
//! [`marker`](super::marker), a table's in [`table`](super::table), an
//! image's in [`image`](super::image), a reading's in
//! [`ruby`](super::ruby), and the style resolution in
//! [`style`](super::style). One topic, one file - so "how does this
//! renderer do tables" has one answer and not two.

use crate::controller::HitAction;
use crate::dict::gloss::{GlossDoc, ItemType, Kind, NodeId, NodePath, RoleFilter, Tag};
use crate::dict::media::Intrinsic;
use crate::ui::theme::Theme;
use super::chrome::LINE_GAP;
use super::flow::{trim, Ctx, Flow, FlowSpan, InlineBox, NO_LINK};
use super::image::{FlowImage, NO_IMAGE};
use super::link::link_action;
use super::marker::FlowMarker;
use super::pass::{Boxed, Piece};
use super::ruby::{FlowRuby, NO_RUBY};
use super::scene::Rgb;
use super::style::{Block, BoxStyle, Inline};

/// What separates two top-level
/// glossary items.
///
/// A dictionary row's `glossary`
/// array holds exactly one item for
/// 64 of the census's 72
/// dictionaries, so this is a no-op
/// on nearly every entry - and on the
/// ones that hold more it is what the
/// panel has always drawn. Ticket 14
/// is what lets a reader stack them
/// instead.
pub(super) const ITEM_SEPARATOR: &str = "; ";

/// What one term-bank row's images can
/// be sized from.
///
/// The dictionary that shipped them -
/// half of a [`MediaKey`], the other
/// half being the node's own `path` -
/// and the intrinsic sizes the build
/// recorded for the ones it could
/// store. Resolved before layout runs
/// and carried on the presentation
/// (`present::GlossEntry::media`),
/// because `scene` is a measure pass
/// with no database behind it and an
/// image must not put a query on the
/// paint path.
///
/// An empty table is a real state and
/// not a missing input: a tree parsed
/// from a string - a demo, a geometry
/// fixture, an Anki round trip - has
/// no store behind it, and its images
/// take the `alt`-text rung.
///
/// [`MediaKey`]: crate::dict::media::MediaKey
#[derive(Clone, Copy)]
pub(super) struct Assets<'a> {
    pub(super) dict_id: i64,
    pub(super) sizes: &'a [(String, Intrinsic)],
}

impl Assets<'_> {
    /// One asset's recorded size.
    ///
    /// A linear scan, because a row
    /// names a handful of assets - 字通
    /// averages more than four - and a
    /// map would cost more to build
    /// than these comparisons cost to
    /// run.
    pub(super) fn size(&self, path: &str) -> Option<Intrinsic> {
        self.sizes.iter().find(|(p, _)| p == path).map(|(_, size)| *size)
    }
}

/// What the render settings leave for
/// the gloss walk to spend.
///
/// The decision table's own record,
/// minus the one knob that was
/// already a parameter: compact mode
/// is [`Paragraphs::stack_items`],
/// which the list pass built and
/// which the settings only flip.
/// Bundled because three more
/// arguments on [`paragraphs`] is
/// where an argument gets passed in
/// the wrong slot, and `Copy` because
/// a table cell's paragraph pass is
/// handed the same one the panel's
/// stream got.
///
/// Resolved once, in
/// [`build_elements`]. Nothing here
/// reads a config type and nothing
/// below here reads a setting.
///
/// [`build_elements`]: super::chrome::build_elements
#[derive(Clone, Copy)]
pub(super) struct Render {
    /// Which editorial roles reach the
    /// panel.
    ///
    /// The card renderer's own
    /// vocabulary and a separate
    /// *value* of it: hiding examples
    /// on screen must not strip them
    /// from a mined card, which takes
    /// [`RoleFilter::CARD`] and no
    /// setting at all.
    pub(super) roles: RoleFilter,
    /// Do a dictionary's own
    /// declarations apply?
    ///
    /// Spent by
    /// [`Paragraphs::declarations`]
    /// and [`styled_marker`], which
    /// are the only three places a
    /// resolved style record is read.
    ///
    /// [`styled_marker`]: super::marker::styled_marker
    pub(super) styling: bool,
    /// Do image assets reach the
    /// panel?
    pub(super) images: bool,
}

/// Turns one term-bank row's parsed
/// tree into paragraphs.
///
/// The block half of the pass, and
/// deliberately the same rules the
/// plain-text renderer uses
/// (`dict::gloss::plain`): a block tag
/// or a `data` marker opens a
/// paragraph, an inline tag adds spans
/// to the one already open, and a
/// paragraph with no text is dropped.
/// Two renderers over one tree must
/// not disagree about where the lines
/// are - that disagreement is the bug
/// class this spec exists to close.
pub(super) struct Paragraphs<'a> {
    pub(super) doc: &'a GlossDoc,
    /// Finished, in order.
    pub(super) out: Vec<Piece>,
    /// The one still filling.
    pub(super) cur: Flow,
    /// Every link seen so far;
    /// [`Flow`] gets the ones it
    /// reached, renumbered.
    pub(super) links: Vec<HitAction>,
    /// A separator owed to the next
    /// text this paragraph takes.
    pub(super) pending_sep: bool,
    /// Every reading seen so far;
    /// [`Flow`] gets the ones its own
    /// bases wear, renumbered.
    pub(super) rubies: Vec<FlowRuby>,
    /// The slot the text being read
    /// right now belongs to, or
    /// [`NO_RUBY`] outside a `ruby`.
    pub(super) open_ruby: u32,
    /// A span break owed before the
    /// next run.
    ///
    /// What keeps an inline box's spans
    /// its own: its first run must not
    /// coalesce into the text before it
    /// and the run after it must not
    /// coalesce into its last, or the
    /// box would be drawn around its
    /// neighbours too.
    pub(super) barrier: bool,
    /// List markers owed to the next
    /// run of text worth marking,
    /// outermost list first.
    ///
    /// Owed rather than pushed, because
    /// the paragraph an item's marker
    /// belongs to is the one the item's
    /// first text lands in - for
    /// `<li><div>x</div></li>` the
    /// `div`'s, not the `li`'s. A
    /// marker pushed where it is
    /// resolved would be flushed out as
    /// a paragraph of its own holding
    /// nothing but a bullet.
    ///
    /// More than one is owed when an
    /// item's whole content is a nested
    /// list: both levels' markers then
    /// share the one line they have
    /// between them, at their own two
    /// gutters, as a browser draws
    /// them.
    pub(super) pending_marker: Vec<FlowMarker>,
    /// Does a list stack its items?
    ///
    /// The one thing ticket 14's
    /// compact mode changes about a
    /// list, and the only field in this
    /// pass that may know a layout mode
    /// exists: `true` gives every item
    /// its own paragraph and the
    /// indent, `false` joins the items
    /// into the paragraph already open
    /// with [`ITEM_SEPARATOR`] between
    /// them.
    ///
    /// Marker *resolution* sits above
    /// it and is shared by both:
    /// [`marker_of`] reads the list's
    /// own `listStyleType` and
    /// [`Marker::label`] writes the
    /// `n`th label, neither of them
    /// aware of this flag. Marker
    /// *placement* is the one thing
    /// that cannot be: `true` hangs the
    /// marker in the gutter the indent
    /// made ([`MarkerBox`]), and
    /// `false` has neither a gutter nor
    /// a line of its own to hang it
    /// beside, so a compact item writes
    /// its marker inline as its
    /// paragraph's first span. See
    /// [`Paragraphs::mark`].
    ///
    /// Wired from `popup.layout_mode`
    /// by [`build_elements`]:
    /// `LayoutMode::Roomy` stacks and
    /// `LayoutMode::Compact` joins.
    ///
    /// [`marker_of`]: super::marker::marker_of
    /// [`Marker::label`]: super::marker::Marker::label
    /// [`MarkerBox`]: super::scene::MarkerBox
    /// [`build_elements`]: super::chrome::build_elements
    pub(super) stack_items: bool,
    /// What else the render settings
    /// leave for this walk to spend.
    pub(super) render: Render,
    /// The media store's answers for
    /// the row being walked.
    pub(super) assets: Assets<'a>,
    /// Every image seen so far;
    /// [`Flow`] gets the ones that
    /// landed in it, renumbered.
    pub(super) images: Vec<FlowImage>,
    /// What a table cell's border
    /// draws in, where the dictionary
    /// declares none.
    ///
    /// Yomitan gives a cell
    /// `border-color:
    /// var(--text-color-light2)`
    /// rather than the `currentColor`
    /// CSS would otherwise use - the
    /// middle rung of its three-step
    /// text ladder, so a grid reads as
    /// a grid without shouting. This
    /// panel's ladder is `body_text`,
    /// `collapsed_text`,
    /// `dimmed_text`, and its middle
    /// rung carries `#969aa0` dark and
    /// `#64666e` light against
    /// Yomitan's own `#999999` and
    /// `#666666`: the same rung, and
    /// very nearly the same colour.
    pub(super) rule: Rgb,
    /// What a header cell's background
    /// draws in.
    ///
    /// Yomitan gives `thead` and `th`
    /// `background-color:
    /// var(--background-color-dark1)`,
    /// which is its palette's one step
    /// off the panel fill. This
    /// theme's one step off the panel
    /// fill is `separator`, the colour
    /// every other hairline and tint
    /// in the panel already uses -
    /// `#323238` against Yomitan's own
    /// `#333333` on a near-identical
    /// dark background.
    pub(super) tint: Rgb,
    /// The popup's root font size: what
    /// a `rem` in a dictionary's own
    /// declarations is a fraction of
    /// ([`Ems`]).
    ///
    /// The theme's body size, taken
    /// once here rather than re-derived
    /// per node, because a node's own
    /// em changes as the walk descends
    /// and the root's does not.
    ///
    /// [`Ems`]: super::style::Ems
    pub(super) root_em: f32,
}

/// One row's gloss tree, laid out as
/// paragraphs and tables at `top_gap`
/// apart.
///
/// `stack_items` is
/// [`Paragraphs::stack_items`], the
/// compact-layout knob, and `render`
/// is the rest of the render
/// settings' decision table
/// ([`Render`]). Both are resolved by
/// [`build_elements`] and neither is
/// re-read below this point.
///
/// The theme rather than a resolved
/// [`Inline`], because a table cell's
/// Yomitan defaults need two colours
/// the body role does not carry - the
/// cell rule and the header tint (see
/// [`Paragraphs::cell_defaults`]).
///
/// [`build_elements`]: super::chrome::build_elements
pub(super) fn paragraphs(
    doc: &GlossDoc,
    theme: &Theme,
    top_gap: f32,
    stack_items: bool,
    assets: Assets<'_>,
    render: Render,
) -> Vec<Piece> {
    let base = Inline::body(theme);
    let mut p = Paragraphs {
        doc,
        out: Vec::new(),
        cur: Flow::default(),
        links: Vec::new(),
        pending_sep: false,
        rubies: Vec::new(),
        open_ruby: NO_RUBY,
        barrier: false,
        pending_marker: Vec::new(),
        stack_items,
        render,
        assets,
        images: Vec::new(),
        rule: theme.collapsed_text,
        tint: theme.separator,
        root_em: theme.body_size,
    };
    let root = Ctx {
        inline: base,
        block: Block::default(),
        link: NO_LINK,
        path: Some(NodePath::ROOT),
    };
    for (i, id) in doc.items().enumerate() {
        p.item(id, root.at(i));
        p.pending_sep = true;
    }
    p.flush();
    for piece in &mut p.out {
        piece.top_gap(top_gap);
    }
    p.out
}

/// The block walk itself: one node, its children, and the paragraph they
/// open, close or add spans to.
///
/// Four subtrees are answered elsewhere, each beside the thing it builds:
/// a list in [`marker`](super::marker), a table in
/// [`table`](super::table), an image in [`image`](super::image), a
/// reading in [`ruby`](super::ruby). The style resolution these arms all
/// call is in [`style`](super::style).
impl Paragraphs<'_> {
    /// One top-level glossary item.
    pub(super) fn item(&mut self, id: NodeId, ctx: Ctx) {
        let doc = self.doc;
        // An item opens no block of its
        // own, so the paragraph it
        // lands in is attributed to the
        // item - which for the 20
        // census dictionaries that emit
        // one plain string per item is
        // the only address a scene
        // element can have. A block
        // inside it takes the address
        // back.
        if self.cur.text.is_empty() {
            self.cur.path = ctx.path;
        }
        match doc.node(id).item_type {
            // A `type: image` item is an
            // image node with no tag, and
            // the same replaced element:
            // it takes room on the line
            // the item lands on rather
            // than opening one.
            ItemType::Image => self.image(id, ctx),
            ItemType::Text => self.text(doc.text(id), ctx.inline, ctx.link),
            // Yomitan drops a
            // `structured-content`
            // item's children straight
            // into a block container,
            // so the item itself
            // neither opens a paragraph
            // nor takes part in the
            // drop rules.
            ItemType::StructuredContent => self.children(id, ctx),
            _ if doc.is_plain_string(id) => self.text(doc.text(id), ctx.inline, ctx.link),
            _ => self.node(id, ctx),
        }
    }

    /// Does this node's editorial role
    /// reach the panel?
    ///
    /// The render settings' role half,
    /// filtered on the same [`Role`]
    /// the card renderer filters on -
    /// with the popup's own answer and
    /// not the card's, which is what
    /// lets a user hide examples on
    /// screen and keep them on a mined
    /// card.
    ///
    /// One clause, over the role the
    /// parser classified. Ticket 15
    /// deleted the name-matched
    /// part-of-speech marker and the
    /// six-name drop list that used to
    /// stand beside this call: the
    /// role is now on the node for
    /// every dictionary, so
    /// `RoleFilter::allows` is the
    /// whole gate here, in
    /// `gloss::html`, and in
    /// `gloss::plain`.
    ///
    /// [`Role`]: crate::dict::gloss::Role
    pub(super) fn shows(&self, id: NodeId) -> bool {
        self.render.roles.allows(self.doc.role(id))
    }

    /// One node.
    pub(super) fn node(&mut self, id: NodeId, ctx: Ctx) {
        let doc = self.doc;
        let node = *doc.node(id);
        if !self.shows(id) {
            return;
        }
        match node.tag {
            // A reading is drawn above
            // its base rather than in
            // the line beside it, so
            // the whole subtree is
            // handled at once: dropped
            // into the flow it would
            // read as 漢かん字じ.
            Tag::Ruby => return self.ruby(id, ctx),
            // Both engines break hard on
            // a newline, so a dictionary's
            // own break reaches the panel
            // inside the paragraph rather
            // than splitting it.
            Tag::Br => return self.text("\n", ctx.inline, ctx.link),
            // A table is a grid rather
            // than a run of lines, and
            // every word in one belongs
            // to a cell, so the whole
            // subtree is taken over at
            // once - down to the `tr`,
            // which is a block here only
            // because a row breaks a
            // line for the plain-text
            // walk.
            Tag::Table => return self.table(id, ctx),
            _ => {}
        }
        if node.kind == Kind::Image {
            return self.image(id, ctx);
        }
        if node.kind == Kind::Text && node.tag == Tag::None {
            return self.text(doc.text(id), ctx.inline, ctx.link);
        }
        let inline = self.styled(id, ctx.inline);
        let block = self.boxed(id, ctx.block, inline);
        // **Where a block's box goes**,
        // and it is not "the first line
        // the block opens". A box wraps
        // all of its block's content, as
        // a browser draws it: a bordered
        // `div` holding three paragraphs
        // draws one border around all
        // three. So a block that
        // declares one takes its whole
        // subtree off to one side and
        // hands the pieces back inside a
        // container ([`Boxed`],
        // [`Paragraphs::wrap`]), which
        // is the only shape that can
        // carry a box over more than one
        // paragraph.
        //
        // Hanging the box on the
        // paragraph the block opened
        // instead lost it outright
        // whenever the block's *first*
        // child opened a line of its
        // own: the paragraph the block
        // opened was still empty when
        // that child's `open` flushed
        // it, and [`Paragraphs::flush`]
        // drops an empty paragraph and
        // its box with it, so a
        // bordered, filled `div` drew
        // nothing at all. Jitendex's
        // `div[data-sc-class="extra-box"]`
        // over `data.content` children
        // is exactly that shape.
        if node.tag.is_block() && block.style.exists() {
            return self.wrap(id, ctx, block, inline);
        }
        // A block carrying no box
        // *opens* a line and never
        // closes one, exactly as the
        // plain-text walk's mark does:
        // text after such a block joins
        // the block's own paragraph. A
        // boxed one closes its line, for
        // the reason [`Paragraphs::wrap`]
        // gives.
        //
        // The line break follows the
        // **tag** or the marker; the box
        // follows the tag alone.
        // `has_marker` is a line-break
        // rule inherited from the
        // plain-text walk: ticket 01 gave
        // a `data.content` node a block
        // mark because that mark is what
        // separated senses before a tree
        // existed. What decides where a
        // box goes is the schema's own
        // block/inline division, by tag,
        // and `span` is inline - so a
        // `span` carrying `data.content`
        // opens its line with no box of
        // its own and draws its box once,
        // below, as the pill it is.
        // Answering both questions with
        // one test gave such a node the
        // *same* resolved `BoxStyle`
        // twice, once as `block_box` and
        // once in `inline_boxes`, and a
        // bin looping over
        // `SceneElem::boxes()` painted
        // Jitendex's
        // `span[data-sc-class="tag"]`
        // twice.
        if node.tag.is_block() {
            self.open(ctx.path, block);
        } else if doc.has_marker(id) {
            self.open(ctx.path, block.inherited());
        }
        let next = Ctx {
            inline,
            block: block.inherited(),
            link: self.link_of(id, ctx.link),
            path: ctx.path,
        };
        // An inline node carrying ink is
        // a pill, and a pill is drawn as
        // an inline box: it keeps its
        // place on its line instead of
        // breaking one. Ink is the whole
        // of the gate, because the seam
        // takes styled spans and no
        // boxes (ADR-0013): an inline
        // box cannot push its neighbours
        // aside, so a margin or a
        // padding with nothing to draw
        // resolves to nothing - whatever
        // its node marks.
        if !node.tag.is_block() && block.style.paints() {
            return self.pill(id, next, block.style);
        }
        // A list marks and indents its
        // own children, so it takes
        // them over from `children`.
        if node.kind == Kind::List {
            return self.list(id, next, node.tag == Tag::Ol);
        }
        self.children(id, next);
    }

    /// One block that declared a box:
    /// its own pieces, inside it.
    ///
    /// The box wraps all of them
    /// ([`Boxed`]), so the block's
    /// subtree is collected off to one
    /// side - exactly as a table cell's
    /// is ([`Paragraphs::cell`]) - and
    /// handed back as one piece.
    ///
    /// **A boxed block closes its own
    /// line**, and it is the second tag
    /// shape that does; `summary` is the
    /// other. Everywhere else a block
    /// only *opens* a line, the rule the
    /// plain-text walk shares, and text
    /// written after one joins the
    /// block's paragraph. A box has to
    /// end somewhere: text after the
    /// `div` is not the `div`'s content
    /// and a browser draws it outside
    /// the border, so the paragraph a
    /// box closes with cannot stay open
    /// to collect it. The divergence
    /// from `gloss::plain` is bounded to
    /// a *boxed* block followed by
    /// inline text at the same level,
    /// and there a browser agrees with
    /// this walk rather than with the
    /// mark.
    pub(super) fn wrap(&mut self, id: NodeId, ctx: Ctx, block: Block, inline: Inline) {
        let doc = self.doc;
        let node = *doc.node(id);
        // The line above the box ends
        // above it.
        self.flush();
        let inner = Ctx {
            inline,
            // The box and the list indent
            // are paid once, by the box:
            // its body sits inside a
            // coordinate system the box
            // established, exactly as a
            // table cell's body does
            // ([`Paragraphs::table`]).
            block: Block { indent: 0.0, ..block.inherited() },
            link: self.link_of(id, ctx.link),
            path: ctx.path,
        };
        // A marker owed from outside is
        // owed to a line *inside* this
        // box - `<li style="paddingLeft">`
        // is real and Jitendex writes it
        // - and a marker's indent is
        // measured from the content edge
        // of whatever the paragraph it
        // lands in sits in. That edge has
        // just moved in by this box's own
        // lead, so the debt moves with
        // it: the bullet still hangs off
        // the *list's* content edge and
        // not off the item's padding
        // ([`place_markers`]).
        let lead_left = block.indent
            + block.style.margin.left
            + block.style.border_used().left
            + block.style.padding.left;
        for mark in &mut self.pending_marker {
            mark.indent -= lead_left;
        }
        // Off to one side, because these
        // pieces belong to the box and
        // not to the stream the panel is
        // stacking.
        let outer = std::mem::take(&mut self.out);
        self.open(ctx.path, inner.block);
        // The two things `node` would
        // have done next, done here
        // instead: a list marks and
        // indents its own children, so it
        // takes them over from
        // `children`.
        if node.kind == Kind::List {
            self.list(id, inner, node.tag == Tag::Ol);
        } else {
            self.children(id, inner);
        }
        self.flush();
        let mut body = std::mem::replace(&mut self.out, outer);
        // A debt the box did not spend
        // goes back to the coordinate
        // system it was owed in: the
        // paragraph reopened below
        // belongs to the block *around*
        // this box.
        for mark in &mut self.pending_marker {
            mark.indent += lead_left;
        }
        for (i, piece) in body.iter_mut().enumerate() {
            // The box's first paragraph
            // starts at the box's own
            // padding; the ones under it
            // are spaced as the panel
            // spaces every other stacked
            // paragraph. Charging the
            // panel's gap above the first
            // one as well would pay it
            // twice - once outside the
            // border and once inside it.
            piece.top_gap(if i == 0 { 0.0 } else { LINE_GAP });
        }
        // A box around nothing draws
        // nothing, which is what
        // [`Paragraphs::flush`] already
        // did with a `div` holding only
        // whitespace: it pays no margin
        // and its address belongs to no
        // element.
        if !body.is_empty() {
            self.out.push(Piece::Boxed(Boxed {
                top_gap: 0.0,
                block,
                body,
                base: inline,
                path: ctx.path,
                dict_id: 0,
                entry_id: 0,
            }));
        }
        // The box closed its line, so a
        // run written after it opens a
        // fresh paragraph belonging to
        // the block *around* the box -
        // the same restoration
        // [`Paragraphs::children`] does
        // after a `summary`. Without it
        // the next run would inherit the
        // box's own inner context, which
        // has no list indent and no
        // alignment: a box establishes a
        // coordinate system for its body
        // and for nothing else.
        //
        // `ctx.path` addresses that
        // paragraph to this block, which
        // is the address it already had:
        // before a box was a container,
        // the run after a block joined
        // the block's own paragraph.
        self.open(ctx.path, ctx.block);
    }

    /// One inline box's own run, kept
    /// out of its neighbours' spans.
    ///
    /// The box is drawn around exactly
    /// the spans this node produced, so
    /// a barrier goes in at each end:
    /// coalescing is what would
    /// otherwise hand the box a
    /// neighbour's text as well.
    pub(super) fn pill(&mut self, id: NodeId, ctx: Ctx, style: BoxStyle) {
        self.barrier = true;
        let from = self.cur.spans.len() as u32;
        self.children(id, ctx);
        let to = self.cur.spans.len() as u32;
        self.barrier = true;
        // `to < from` when the node held
        // a block after all and flushed
        // the paragraph out from under
        // itself; a box over no span is
        // no box either way.
        if to > from {
            self.cur.inline.push(InlineBox { from, to, style });
        }
    }

    /// Ends the paragraph being built
    /// and starts one that belongs to
    /// `path`, carrying `block`'s box.
    pub(super) fn open(&mut self, path: Option<NodePath>, block: Block) {
        self.flush();
        self.cur.path = path;
        self.cur.block = block;
    }

    /// A node's children.
    ///
    /// A glossary array of bare strings
    /// is a list, one paragraph per
    /// string - Yomitan gives each its
    /// own `<li>`. An array mixing
    /// strings with nodes is prose
    /// broken up by its own markup, so
    /// only a run that is *entirely*
    /// bare strings, and holds more
    /// than one, becomes paragraphs.
    ///
    /// `ctx` is what the children
    /// inherit, so its box is already
    /// the empty one
    /// [`Block::inherited`] hands down:
    /// a bare string declares no box
    /// and each of these paragraphs
    /// carries none.
    pub(super) fn children(&mut self, id: NodeId, ctx: Ctx) {
        let doc = self.doc;
        if !doc.node(id).tag.is_inline() && doc.is_string_list(id) {
            for (i, child) in doc.children(id).enumerate() {
                self.open(ctx.at(i).path, ctx.block);
                self.text(doc.text(child), ctx.inline, ctx.link);
            }
            return;
        }
        for (i, child) in doc.children(id).enumerate() {
            self.node(child, ctx.at(i));
            // A `summary` closes its line
            // as well as opening one, and
            // it is the only tag that
            // does. What a block
            // otherwise does is *open* a
            // line - the rule the
            // plain-text walk shares - so
            // text after one joins it.
            // For a `details` that text
            // is the disclosure's body,
            // which a browser draws under
            // the summary and not as one
            // sentence with it. The
            // paragraph reopened here
            // belongs to the `details`,
            // whose path is this walk's
            // own.
            if doc.node(child).tag == Tag::Summary {
                self.open(ctx.path, ctx.block);
            }
        }
    }

    /// One of an image's own spans,
    /// joined to nothing.
    ///
    /// `link` rides along so an image
    /// inside a cross-reference is part
    /// of that link's hit target - a
    /// gaiji in a "see also" is as
    /// clickable as the word beside it.
    /// `ruby` does not: an image is
    /// never a ruby base, and claiming
    /// a slot would centre a reading
    /// over a picture.
    pub(super) fn raw(&mut self, text: &str, style: Inline, link: u32, image: u32, filler: bool) {
        let at = self.cur.text.len() as u32;
        self.cur.text.push_str(text);
        self.cur.spans.push(FlowSpan {
            at,
            len: text.len() as u32,
            style,
            link,
            ruby: NO_RUBY,
            filler,
            image,
        });
    }

    /// Appends text, paying any owed
    /// item separator and list marker
    /// first.
    pub(super) fn text(&mut self, text: &str, style: Inline, link: u32) {
        if text.is_empty() {
            return;
        }
        if std::mem::take(&mut self.pending_sep) && !self.cur.text.is_empty() {
            self.push(ITEM_SEPARATOR, style, link);
        }
        // A marker waits for text worth
        // marking: `<li><br>a</li>` puts
        // it on the `a` rather than on
        // the break, and an item holding
        // only the whitespace a
        // dictionary left between two
        // nodes draws none at all.
        if !text.trim().is_empty() {
            self.mark();
        }
        self.push(text, style, link);
    }

    /// Appends one run, joining it to
    /// the span before it when nothing
    /// about it differs.
    ///
    /// Coalescing is not only economy.
    /// It is what keeps a gloss that is
    /// one plain string measuring as
    /// exactly one span - byte for byte
    /// the request the panel made
    /// before this pass existed, which
    /// is why the geometry goldens do
    /// not move.
    pub(super) fn push(&mut self, text: &str, style: Inline, link: u32) {
        let at = self.cur.text.len() as u32;
        let len = text.len() as u32;
        let ruby = self.open_ruby;
        let joins = !std::mem::take(&mut self.barrier);
        self.cur.text.push_str(text);
        match self.cur.spans.last_mut() {
            Some(last)
                if joins
                    && !last.filler
                    && last.image == NO_IMAGE
                    && last.style == style
                    && last.link == link
                    && last.ruby == ruby
                    && last.at + last.len == at =>
            {
                last.len += len;
            }
            _ => self.cur.spans.push(FlowSpan {
                at,
                len,
                style,
                link,
                ruby,
                filler: false,
                image: NO_IMAGE,
            }),
        }
    }

    /// Ends the open paragraph.
    ///
    /// A paragraph with no text is
    /// dropped rather than drawn, so
    /// nested blocks give one paragraph
    /// per innermost block instead of a
    /// run of blank ones. What survives
    /// is trimmed at both ends, because
    /// Yomitan draws this in a browser
    /// and a browser does not indent a
    /// paragraph by the space a
    /// dictionary happened to leave
    /// between two nodes.
    pub(super) fn flush(&mut self) {
        if self.cur.text.trim().is_empty() {
            self.cur.text.clear();
            self.cur.spans.clear();
            self.cur.inline.clear();
            // A marker needs a line to
            // hang beside, and a dropped
            // paragraph has none. Only
            // reachable through a
            // paragraph an item opened
            // and never filled, which
            // draws no marker either way.
            self.cur.marker.clear();
            // The dropped paragraph's
            // block goes with it: a
            // `div` holding only
            // whitespace pays no margin,
            // and its address belongs to
            // no element.
            self.cur.block = Block::default();
            self.cur.path = None;
            return;
        }
        let mut flow = std::mem::take(&mut self.cur);
        trim(&mut flow);
        // Renumber onto this paragraph's
        // own lists: an `<a>` holding a
        // block spans two paragraphs,
        // and neither may name an index
        // the other owns. A reading is
        // renumbered the same way, for
        // the same reason.
        let mut seen: Vec<(u32, u32)> = Vec::new();
        for span in &mut flow.spans {
            if span.link == NO_LINK {
                continue;
            }
            span.link = match seen.iter().find(|(old, _)| *old == span.link) {
                Some((_, new)) => *new,
                None => {
                    let new = flow.links.len() as u32;
                    flow.links.push(self.links[span.link as usize].clone());
                    seen.push((span.link, new));
                    new
                }
            };
        }
        let mut seen: Vec<(u32, u32)> = Vec::new();
        for span in &mut flow.spans {
            if span.ruby == NO_RUBY {
                continue;
            }
            span.ruby = match seen.iter().find(|(old, _)| *old == span.ruby) {
                Some((_, new)) => *new,
                None => {
                    let new = flow.ruby.len() as u32;
                    flow.ruby.push(self.rubies[span.ruby as usize].clone());
                    seen.push((span.ruby, new));
                    new
                }
            };
        }
        let mut seen: Vec<(u32, u32)> = Vec::new();
        for span in &mut flow.spans {
            if span.image == NO_IMAGE {
                continue;
            }
            span.image = match seen.iter().find(|(old, _)| *old == span.image) {
                Some((_, new)) => *new,
                None => {
                    let new = flow.images.len() as u32;
                    flow.images.push(self.images[span.image as usize].clone());
                    seen.push((span.image, new));
                    new
                }
            };
        }
        self.out.push(Piece::Flow(flow));
    }

    /// The link a node's content sits
    /// inside, its own or its
    /// parent's.
    pub(super) fn link_of(&mut self, id: NodeId, inherited: u32) -> u32 {
        let doc = self.doc;
        if doc.node(id).kind != Kind::Link {
            return inherited;
        }
        let Some(href) = doc.attr_of(id, "href").and_then(|v| doc.scalar_str(v)) else {
            return inherited;
        };
        match link_action(href) {
            Some(action) => {
                self.links.push(action);
                self.links.len() as u32 - 1
            }
            None => inherited,
        }
    }
}

/// Every text descendant of a node,
/// concatenated.
///
/// What a reading and an `rp` fallback
/// are: one run of text, however many
/// wrappers a dictionary put around
/// it. The same "a text node is
/// `Kind::Text` with no tag" rule
/// [`Paragraphs::node`] uses, so the
/// two walks cannot disagree about
/// what text a subtree holds.
pub(super) fn text_of(doc: &GlossDoc, id: NodeId) -> String {
    let mut out = String::new();
    push_text_of(doc, id, &mut out);
    out
}

pub(super) fn push_text_of(doc: &GlossDoc, id: NodeId, out: &mut String) {
    let node = doc.node(id);
    if node.kind == Kind::Text && node.tag == Tag::None {
        out.push_str(doc.text(id));
    }
    for child in doc.children(id) {
        push_text_of(doc, child, out);
    }
}
