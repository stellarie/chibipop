//! A term-bank row supplies a [`GlossDoc`] tree. This module walks that tree into paragraphs.
//!
//! **One reason to change:** this module defines how each [`GlossDoc`] subtree becomes layout output.
//! It covers unsupported tags, role rules, and Yomitan defaults.
//!
//! The block walk follows the rules of the plain-text renderer (`dict::gloss::plain`).
//! A block tag or a `data` marker opens a paragraph. An inline tag adds spans to that
//! paragraph. The walk drops empty paragraphs. The plain-text renderer and this renderer
//! must agree on line positions.
//!
//! The [`Paragraphs`] struct stores the state for the gloss walk. These modules handle
//! the listed subtrees:
//!
//! - a list in [`marker`](super::marker)
//! - a table in [`table`](super::table)
//! - an image in [`image`](super::image)
//! - a reading in [`ruby`](super::ruby)
//! - style resolution in [`style`](super::style)
//!
//! Each file keeps one topic, so the table code has one implementation.

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
use super::scene::TextSource;
use super::pill::{reserves, rooms, spacers, PILL_SPACER};
use super::ruby::{FlowRuby, NO_RUBY};
use super::scene::Rgb;
use super::style::{Block, BoxStyle, Inline};

/// Separates top-level glossary items.
///
/// A dictionary row's `glossary` array has one item in 64 of 72 dictionaries in the
/// census. The panel uses this separator only when an entry has more than one item.
/// List layout stacks these items instead.
pub(super) const ITEM_SEPARATOR: &str = "; ";

/// Stores media data for one term-bank row.
///
/// The dictionary ID and node's `path` form each [`MediaKey`]. This field also stores
/// intrinsic sizes from the build. The presentation step resolves this data before layout.
/// It passes the data in `present::GlossEntry::media`. `scene` measures without a database.
/// An image must not query the database on the paint path.
///
/// An empty table is valid. A tree from a string has no media store. A demo, a geometry
/// fixture, and an Anki round trip use `alt` text for images.
///
/// [`MediaKey`]: crate::dict::media::MediaKey
#[derive(Clone, Copy)]
pub(super) struct Assets<'a> {
    pub(super) dict_id: i64,
    pub(super) sizes: &'a [(String, Intrinsic)],
}

impl Assets<'_> {
    /// The `size` function scans `sizes` because a row names few assets. 字通 averages more
    /// than four assets. A map costs more to build than these comparisons cost to run.
    pub(super) fn size(&self, path: &str) -> Option<Intrinsic> {
        self.sizes.iter().find(|(p, _)| p == path).map(|(_, size)| *size)
    }
}

/// Stores settings for the gloss walk.
///
/// The struct stores the decision table except the compact mode parameter,
/// [`Paragraphs::stack_items`]. The list pass sets that field. These fields stay together
/// because more arguments to [`paragraphs`] can cause argument errors. The type implements
/// `Copy`, so a table-cell pass and the panel stream share one record.
///
/// [`build_elements`] resolves this record once. This type reads no config type, and code
/// below reads no setting.
///
/// [`build_elements`]: super::chrome::build_elements
#[derive(Clone, Copy)]
pub(super) struct Render {
    /// Selects editorial roles that reach the panel.
    ///
    /// The panel uses this filter. The card renderer uses its own filter and always keeps
    /// examples on a mined card. A user can hide examples on screen, but a mined card
    /// always uses [`RoleFilter::CARD`].
    pub(super) roles: RoleFilter,
    /// This flag controls whether dictionary declarations apply.
    ///
    /// [`Paragraphs::declarations`] and [`styled_marker`] are the only functions that read
    /// this resolved style record.
    ///
    /// [`styled_marker`]: super::marker::styled_marker
    pub(super) styling: bool,
    /// Controls whether image assets reach the panel.
    pub(super) images: bool,
}

/// Walks one term-bank row's [`GlossDoc`] tree into paragraphs.
///
/// The block walk follows the rules of the plain-text renderer (`dict::gloss::plain`).
/// A block tag or a `data` marker opens a paragraph. An inline tag adds spans to that
/// paragraph. The walk drops empty paragraphs. The plain-text renderer and this renderer
/// must agree on line positions.
pub(super) struct Paragraphs<'a> {
    pub(super) doc: &'a GlossDoc,
    /// Stores completed pieces in order.
    pub(super) out: Vec<Piece>,
    /// Stores the paragraph that remains open.
    pub(super) cur: Flow,
    /// This list stores links found so far.
    /// [`Flow`] receives these links with local indexes.
    pub(super) links: Vec<HitAction>,
    /// This flag marks an item separator owed to the next text in this paragraph.
    pub(super) pending_sep: bool,
    /// The path of the active ruby node.
    ///
    /// Ruby base text addresses the ruby node as one atomic leaf. The state
    /// applies only while [`Paragraphs::ruby`] walks that node's children.
    pub(super) ruby_path: Option<NodePath>,
    /// This list stores readings found so far.
    /// [`Flow`] receives the readings for their bases with local indexes.
    /// This slot names the current text's reading. It is [`NO_RUBY`] outside a `ruby`.
    pub(super) rubies: Vec<FlowRuby>,
    pub(super) open_ruby: u32,
    /// This flag keeps the next span separate from its neighbor.
    ///
    /// The first run inside an inline box must not join earlier text. The run after the box
    /// must not join its final text. Without this barrier, the pass draws the box around
    /// adjacent text.
    pub(super) barrier: bool,
    /// Stores the number of paragraphs that this walk closed.
    ///
    /// Only [`Paragraphs::pill`] reads this counter. It shows whether child nodes
    /// contained a block and closed the paragraph that the box measured against. The box
    /// records span indexes, and each index then names a closed paragraph. Without this
    /// counter, a box encloses the wrong replacement content.
    pub(super) flushes: u32,
    /// Stores markers that the next marked text run needs, from outermost to innermost list.
    ///
    /// This pass delays markers because an item marker belongs to the paragraph's first
    /// text. For `<li><div>x</div></li>`, the marker belongs to the `div`, not the `li`.
    /// When this pass pushes a marker at resolution, it becomes a paragraph with only a
    /// bullet.
    ///
    /// Nested list content can need multiple markers. Markers for both levels share one
    /// line at their gutters, as a browser does.
    pub(super) pending_marker: Vec<FlowMarker>,
    /// Indicates whether a list gives each item a separate paragraph.
    ///
    /// Compact mode sets this field for lists. A `true` value gives each item its own
    /// paragraph and indent. A `false` value joins items to the open paragraph with
    /// [`ITEM_SEPARATOR`].
    ///
    /// Both modes resolve markers in the same way. [`marker_of`] reads `listStyleType`,
    /// and [`Marker::label`] writes the `n`th label. This flag changes marker placement.
    /// A `true` value hangs the marker in the gutter from the indent ([`MarkerBox`]).
    /// A `false` value has no gutter or separate line, so the compact item writes its
    /// marker as the first paragraph span. See [`Paragraphs::mark`].
    ///
    /// [`build_elements`] sets this flag from `popup.layout_mode`. `LayoutMode::Roomy`
    /// stacks items. `LayoutMode::Compact` joins them.
    ///
    /// [`marker_of`]: super::marker::marker_of
    /// [`Marker::label`]: super::marker::Marker::label
    /// [`MarkerBox`]: super::scene::MarkerBox
    /// [`build_elements`]: super::chrome::build_elements
    pub(super) stack_items: bool,
    /// Stores other render settings for the gloss walk.
    pub(super) render: Render,
    /// This field stores media data for the active row.
    pub(super) assets: Assets<'a>,
    /// This list stores images found so far.
    /// [`Flow`] receives images that enter it with local indexes.
    pub(super) images: Vec<FlowImage>,
    /// Stores the border color for a table cell when the dictionary declares no border color.
    ///
    /// Yomitan sets `border-color: var(--text-color-light2)` on a cell instead of
    /// `currentColor`. That value is the middle step in its three-step text ladder, so the
    /// grid stays visible without high contrast. This panel uses `body_text`,
    /// `collapsed_text`, and `dimmed_text`. The middle step uses `#969aa0` for dark themes
    /// and `#64666e` for light themes.
    /// Yomitan uses `#999999` and `#666666`. Both designs use the same ladder step and
    /// similar colors.
    pub(super) rule: Rgb,
    /// Stores the background color for a header cell.
    ///
    /// Yomitan gives `thead` and `th` `background-color: var(--background-color-dark1)`.
    /// That color sits one step from the panel fill. This theme uses `separator` for that
    /// step. The `separator` color matches other hairlines and tints in the panel. It is
    /// `#323238`, close to Yomitan's `#333333` on dark backgrounds.
    pub(super) tint: Rgb,
    /// Stores the popup's root font size. A `rem` in dictionary declarations uses a fraction
    /// of this size ([`Ems`]).
    ///
    /// This field stores the theme body size. A node's em changes during the descent, but
    /// the root em stays constant.
    ///
    /// [`Ems`]: super::style::Ems
    pub(super) root_em: f32,
}

/// Lays out one row's gloss tree as paragraphs and tables with `top_gap` spacing.
///
/// `stack_items` is [`Paragraphs::stack_items`], the compact-layout setting. `render`
/// holds the other render settings ([`Render`]). [`build_elements`] resolves both values.
/// Code below this point reads neither value again.
///
/// This function takes the theme instead of a resolved [`Inline`]. Yomitan table-cell
/// defaults need two colors that the body role lacks: the cell rule and header tint.
/// See [`Paragraphs::cell_defaults`].
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
        ruby_path: None,
        rubies: Vec::new(),
        open_ruby: NO_RUBY,
        barrier: false,
        flushes: 0,
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

/// Walks one node and its children. It opens, closes, or expands the paragraph with spans.
///
/// Other modules handle four subtrees: a list in [`marker`](super::marker), a table in
/// [`table`](super::table), an image in [`image`](super::image), and a reading in
/// [`ruby`](super::ruby). These modules call style resolution in [`style`](super::style).
impl Paragraphs<'_> {
    /// Processes one top-level glossary item.
    pub(super) fn item(&mut self, id: NodeId, ctx: Ctx) {
        let doc = self.doc;
        // An item opens no block. It receives the path of the paragraph that it enters.
        match doc.node(id).item_type {
            // A `type: image` item has no tag and acts as a replaced element.
            // It takes space on the current line. It does not open a new line.
            ItemType::Image => self.image(id, ctx),
            ItemType::Text => self.text(doc.text(id), ctx.inline, ctx.link, ctx.path, 0),
            // Yomitan places children of a `structured-content` item in a block container.
            // The item itself does not open a paragraph or take part in drop rules.
            ItemType::StructuredContent => self.children(id, ctx),
            _ if doc.is_plain_string(id) =>
                self.text(doc.text(id), ctx.inline, ctx.link, ctx.path, 0),
            _ => self.node(id, ctx, false),
        }
    }

    /// Returns whether a node's editorial role reaches the panel.
    ///
    /// The render settings use [`Role`] as the filter type. The card renderer uses the same
    /// filter. The popup has its own filter. A user can hide examples on screen, but a mined
    /// card keeps them.
    ///
    /// The parser supplies the role. This call does not match names for part-of-speech
    /// markers or the six-name drop list. Every dictionary sets the node role, so
    /// `RoleFilter::allows` is the only check here, in `gloss::html`, and in `gloss::plain`.
    ///
    /// [`Role`]: crate::dict::gloss::Role
    pub(super) fn shows(&self, id: NodeId) -> bool {
        self.render.roles.allows(self.doc.role(id))
    }

    /// Processes one node.
    ///
    /// `prose` is true when visible bare-string text sits beside this node or a prose
    /// fragment precedes it. Either condition exempts an inline node from the marker line
    /// break ([`GlossDoc::prose`], [`GlossDoc::inline_prose`]).
    pub(super) fn node(&mut self, id: NodeId, ctx: Ctx, prose: bool) {
        let doc = self.doc;
        let node = *doc.node(id);
        if !self.shows(id) {
            return;
        }
        match node.tag {
            // A reading appears above its base, not in the adjacent line.
            // This branch handles the whole subtree at once. The main flow would render
            // 漢かん字じ.
            Tag::Ruby => return self.ruby(id, ctx),
            // Both engines break on a newline.
            // A dictionary break stays inside the paragraph and does not split it.
            Tag::Br => return self.text("\n", ctx.inline, ctx.link, None, 0),
            // A table forms a grid of lines, and each word belongs to a cell.
            // This branch handles the whole subtree through `tr`.
            // The plain-text walk treats `tr` as a block because a row breaks a line.
            Tag::Table => return self.table(id, ctx),
            _ => {}
        }
        if node.kind == Kind::Image {
            return self.image(id, ctx);
        }
        if node.kind == Kind::Text && node.tag == Tag::None {
            return self.text(doc.text(id), ctx.inline, ctx.link, ctx.path, 0);
        }
        let inline = self.styled(id, ctx.inline);
        let block = self.boxed(id, ctx.block, inline);
        // **Block box placement:**
        // A box wraps all content in its block, not only the first line.
        // A bordered `div` with three paragraphs therefore draws one border around all three.
        // A block with a box moves its whole subtree into a [`Boxed`] container
        // ([`Paragraphs::wrap`]). Only that container can carry a box across multiple
        // paragraphs.
        //
        // The old code attached the box to the block's first paragraph.
        // The first child then opened its own line and caused `open` to flush the empty
        // paragraph.
        // [`Paragraphs::flush`] drops that paragraph with its box, so a bordered, filled
        // `div` drew nothing.
        // In Jitendex, `div[data-sc-class="extra-box"]` wraps `data.content` children in
        // this way.
        // The line break follows the tag or marker. The box follows only the tag.
        // `has_marker` is a line-break rule from the plain-text walk.
        // That walk marked a `data.content` node as a block to separate senses before this
        // tree existed.
        // The schema's block and inline division decides box placement.
        // `span` is inline, so a `span` with `data.content` opens its line without a box.
        // The code below draws its box once as a pill.
        // One test checked both conditions and assigned the same resolved `BoxStyle` twice:
        // once to `block_box` and once to `inline_boxes`.
        // A renderer over `SceneElem::boxes()` then painted Jitendex's
        // `span[data-sc-class="tag"]` twice.
        if node.tag.is_block() && block.style.exists() {
            return self.wrap(id, ctx, block, inline);
        }
        if node.tag.is_block() {
            self.open(ctx.path, block);
        } else if doc.has_marker(id) && !prose {
            // The marker separates senses only when the senses are adjacent.
            // When sentence text appears beside the marker or wrapped text precedes it, the
            // marker is markup inside a sentence.
            // Jitendex uses this form for `example-keyword` and `attribution-footnote`.
            // A break there splits the sentence ([`GlossDoc::prose`],
            // [`GlossDoc::inline_prose`]).
            self.open(ctx.path, block.inherited());
        }
        let next = Ctx {
            inline,
            block: block.inherited(),
            link: self.link_of(id, ctx.link),
            path: ctx.path,
        };
        // An inline node with ink or horizontal room is a pill.
        // This pass draws the pill as an inline box. It keeps its place on the line and does
        // not break the line.
        //
        // Ink was once the only gate. An inline box could not move its neighbors, so a margin
        // with no ink resolved to nothing.
        // [`measure_pills`] now lets a margin move its neighbors. A dictionary that spaces
        // two inline labels with margins alone therefore gets its declared gaps.
        // This pass draws no box around those labels.
        //
        // [`measure_pills`]: super::pill::measure_pills
        if !node.tag.is_block() && (block.style.paints() || reserves(block.style)) {
            return self.pill(id, next, block.style);
        }
        // A list marks and indents its own children. The list therefore handles them
        // instead of `children`.
        if node.kind == Kind::List {
            return self.list(id, next, node.tag == Tag::Ol, block.style.padding.left);
        }
        self.children(id, next);
    }

    /// Wraps the pieces from one block that declares a box.
    ///
    /// [`Boxed`] stores all pieces from the block. This function collects the subtree
    /// separately, as [`Paragraphs::cell`] collects a table-cell subtree, then returns one
    /// piece.
    ///
    /// **A boxed block closes its own line.** `summary` also closes a line. Every other
    /// block only *opens* a line, as the plain-text walk does, so text after it joins that
    /// block's paragraph.
    /// A box must end at its boundary. A browser draws text after the `div` outside its
    /// border, so that text is not the `div` content. The paragraph that the box closes
    /// cannot remain open for that text.
    /// This walk differs from `gloss::plain` only for a *boxed* block with inline text at the
    /// same level. A browser agrees with this walk for that case.
    pub(super) fn wrap(&mut self, id: NodeId, ctx: Ctx, block: Block, inline: Inline) {
        let doc = self.doc;
        let node = *doc.node(id);
        // The paragraph above the box ends before the box.
        self.flush();
        let inner = Ctx {
            inline,
            // The box includes its edges and list indent once.
            // Its body uses the coordinate system that the box establishes, as a table cell
            // body does ([`Paragraphs::table`]).
            block: Block { indent: 0.0, ..block.inherited() },
            link: self.link_of(id, ctx.link),
            path: ctx.path,
        };
        // This box carries a marker from outside to a line *inside* the box.
        // `<li style="paddingLeft">` is valid, and Jitendex writes it.
        // This walk measures the marker indent from the content edge of the box that holds
        // its paragraph. The box lead already moves that edge inward, so the debt moves
        // with the edge.
        // The bullet still hangs from the *list's* content edge, not the item padding
        // ([`place_markers`]).
        let lead_left = block.indent
            + block.style.margin.left
            + block.style.border_used().left
            + block.style.padding.left;
        for mark in &mut self.pending_marker {
            mark.indent -= lead_left;
        }
        // Store these pieces separately. They belong to the box, not to the stream that the
        // panel stacks.
        let outer = std::mem::take(&mut self.out);
        self.open(ctx.path, inner.block);
        // Apply the same child dispatch as `node`.
        // A list marks and indents its own children, so the list handles them instead of
        // `children`.
        if node.kind == Kind::List {
            self.list(id, inner, node.tag == Tag::Ol, block.style.padding.left);
        } else {
            self.children(id, inner);
        }
        self.flush();
        let mut body = std::mem::replace(&mut self.out, outer);
        // Return unused marker debt to the coordinate system that owed it.
        // The paragraph that this function reopens belongs to the block *around* this box.
        for mark in &mut self.pending_marker {
            mark.indent += lead_left;
        }
        for (i, piece) in body.iter_mut().enumerate() {
            // The box's first paragraph starts at its own padding.
            // The panel spaces child paragraphs as it spaces other stacked paragraphs.
            // A panel gap above the first child would apply twice: outside the border and
            // inside it.
            piece.top_gap(if i == 0 { 0.0 } else { LINE_GAP });
        }
        // An empty box draws nothing.
        // [`Paragraphs::flush`] already drops a `div` that contains only whitespace.
        // That `div` pays no margin, and its address belongs to no element.
        if !body.is_empty() {
            self.out.push(Piece::Boxed(Boxed {
                top_gap: 0.0,
                block,
                body,
                base: inline,
                path: ctx.path,
                dict_id: 0,
                entry_id: 0,
                entry: 0,
            }));
        }
        // The box closes its line. A run after it opens a new paragraph in the block around
        // the box.
        // [`Paragraphs::children`] restores the outer context after `summary` in the same
        // way. Without this call, the next run would inherit the box's inner context, which
        // has no list indent or alignment.
        // A box establishes a coordinate system for its body only.
        //
        // `ctx.path` gives this paragraph the same address that the block already had.
        // Before the box became a container, a run after a block joined the block paragraph.
        self.open(ctx.path, ctx.block);
    }

    /// Adds one inline box's own run, keeps it separate from neighbors, and gives it the
    /// space that its edges declare.
    ///
    /// This pass draws a box around exactly the spans that this node produces. It therefore
    /// places a barrier at both ends. Without the barriers, the box would include neighbor
    /// text. It would also join a spacer span to adjacent text, and both would share one
    /// size. A spacer must keep its own size.
    ///
    /// CSS places four spacer runs around the content: the left margin, the left border and
    /// padding, the right padding and border, and the right margin. The content run sits
    /// between the inner spacer runs. Margins sit outside `from..to` because a margin sits
    /// outside the box that this pass draws around that range ([`InlineBox`]).
    /// This function writes each spacer only when room exists. A box with no horizontal
    /// declaration then measures byte-for-byte as before.
    pub(super) fn pill(&mut self, id: NodeId, ctx: Ctx, style: BoxStyle) {
        let room = rooms(style);
        // If the node has no inline content, return to this point.
        // Unused room remains visible as a gap, but this pass draws no box there.
        let mark = (self.cur.text.len(), self.cur.spans.len());
        let flushes = self.flushes;
        self.barrier = true;
        self.room(room[0], ctx);
        let from = self.cur.spans.len() as u32;
        self.room(room[1], ctx);
        let opened = self.cur.spans.len() as u32;
        self.children(id, ctx);
        self.barrier = true;
        // A block child flushed the paragraph that held these indexes.
        // The indexes name paragraphs that already left the stream, so this function has no
        // box to draw and no state to restore.
        // A box with no span draws no box.
        if self.flushes != flushes {
            return;
        }
        if self.cur.spans.len() as u32 == opened {
            self.cur.spans.truncate(mark.1);
            self.cur.text.truncate(mark.0);
            return;
        }
        self.room(room[2], ctx);
        let to = self.cur.spans.len() as u32;
        self.room(room[3], ctx);
        self.cur.inline.push(InlineBox { from, to, style });
    }

    /// Adds the declared advance for one edge of an inline box.
    ///
    /// This function writes a run of [`PILL_SPACER`]s in the box's resolved style.
    /// [`measure_pills`] sizes the run against its em and keeps it separate from adjacent
    /// text. This function pushes the run and does not coalesce it for that reason.
    /// It writes the count directly into the paragraph string, so [`Paragraphs::raw`] needs
    /// no temporary.
    ///
    /// The run receives the box's link. A clickable pill therefore responds across its
    /// padding, not only its glyphs. The run receives no ruby slot. A reading centered over
    /// a margin would cover nothing.
    ///
    /// [`PILL_SPACER`]: super::pill::PILL_SPACER
    /// [`measure_pills`]: super::pill::measure_pills
    fn room(&mut self, room: f32, ctx: Ctx) {
        let n = spacers(room, ctx.inline.size);
        if n == 0 {
            return;
        }
        let at = self.cur.text.len() as u32;
        for _ in 0..n {
            self.cur.text.push_str(PILL_SPACER);
        }
        self.cur.spans.push(FlowSpan {
            at,
            len: self.cur.text.len() as u32 - at,
            style: ctx.inline,
            link: ctx.link,
            ruby: NO_RUBY,
            filler: false,
            image: NO_IMAGE,
        });
    }

    /// Closes the open paragraph and starts one for `path` with `block`'s box.
    pub(super) fn open(&mut self, path: Option<NodePath>, block: Block) {
        self.flush();
        self.cur.path = path;
        self.cur.block = block;
    }

    /// Processes a node's children.
    ///
    /// A glossary array of bare strings is a list. Yomitan gives each string its own `<li>`
    /// and paragraph. An array that mixes strings with nodes is prose that its markup divides.
    /// An array with more than one *entirely* bare string becomes separate paragraphs.
    ///
    /// Children inherit `ctx`. Its box is already empty because [`Block::inherited`] provides
    /// no box. A bare string declares no box, so each paragraph carries no box.
    pub(super) fn children(&mut self, id: NodeId, ctx: Ctx) {
        let doc = self.doc;
        if !doc.node(id).tag.is_inline() && doc.is_string_list(id) {
            for (i, child) in doc.children(id).enumerate() {
                let child_ctx = ctx.at(i);
                self.open(child_ctx.path, ctx.block);
                self.text(doc.text(child), ctx.inline, ctx.link, child_ctx.path, 0);
            }
            return;
        }
        let mut prose = doc.prose(id);
        for (i, child) in doc.children(id).enumerate() {
            self.node(child, ctx.at(i), prose);
            prose = prose || doc.inline_prose(child);
            // A `summary` opens and closes its line. It is the only tag that closes a line.
            // Every other block only opens a line, as the plain-text walk does, so text after
            // it joins that line.
            // For `details`, that text is the disclosure body. A browser draws it below the
            // summary, not in one sentence with it.
            // This code reopens a paragraph for `details` and gives it this walk's path.
            if doc.node(child).tag == Tag::Summary {
                self.open(ctx.path, ctx.block);
            }
        }
    }

    /// Adds one image run and keeps it separate from other runs.
    ///
    /// `link` travels with the span. An image inside a cross-reference therefore belongs to
    /// that link's hit target. A gaiji in a "see also" link is as clickable as adjacent text.
    ///
    /// `ruby` travels only on the [`IMAGE_SPACER`] run. An image *is* a legal ruby base.
    /// Eight dictionaries place a reading or a 表外字 mark over a gaiji.
    /// A browser places the reading over the base box. It does so whether the box holds
    /// text or replaced content.
    /// The spacer run is the image's horizontal box, so the reading centers over it
    /// ([`place_ruby`]).
    ///
    /// This function keeps [`IMAGE_RISER`] out of the slot. The riser is the image filler.
    /// [`measure_images`] already sizes it for the asset's rise above the baseline.
    /// [`measure_readings`] would overwrite that size if a span served as both image filler
    /// and reading.
    pub(super) fn raw(&mut self, text: &str, style: Inline, link: u32, image: u32, filler: bool) {
        let at = self.cur.text.len() as u32;
        self.cur.text.push_str(text);
        self.cur.spans.push(FlowSpan {
            at,
            len: text.len() as u32,
            style,
            link,
            ruby: if filler { NO_RUBY } else { self.open_ruby },
            filler,
            image,
        });
    }

    /// Appends text after it pays any owed item separator and list marker.
    ///
    /// `path` addresses the real text leaf. Synthetic text passes `None`, so
    /// separators, breaks, and image alternatives never become selection sources.
    pub(super) fn text(
        &mut self,
        text: &str,
        style: Inline,
        link: u32,
        path: Option<NodePath>,
        byte: u32,
    ) {
        if text.is_empty() {
            return;
        }
        if std::mem::take(&mut self.pending_sep) && !self.cur.text.is_empty() {
            self.push(ITEM_SEPARATOR, style, link, None);
        }
        // A marker waits for text that deserves a mark.
        // `<li><br>a</li>` puts it on `a`, not on the break.
        // An item with only dictionary whitespace between nodes draws no marker.
        if !text.trim().is_empty() {
            self.mark();
        }
        let source = path.map(|path| {
            let atomic = self.ruby_path.is_some();
            TextSource {
                at: self.cur.text.len() as u32,
                len: text.len() as u32,
                path: self.ruby_path.unwrap_or(path),
                byte: if atomic { 0 } else { byte },
                atomic,
            }
        });
        self.push(text, style, link, source);
    }

    /// Appends one run and joins it to the previous span when all properties match.
    ///
    /// When this function joins spans, it saves more than memory. A gloss from one plain
    /// string then measures as one span, byte for byte like the panel request before this
    /// pass existed. Geometry goldens therefore do not move.
    pub(super) fn push(
        &mut self,
        text: &str,
        style: Inline,
        link: u32,
        source: Option<TextSource>,
    ) {
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
        if let Some(mut source) = source {
            source.at = at;
            self.cur.sources.push(source);
        }
    }

    /// Closes the open paragraph.
    ///
    /// This function drops paragraphs with no text and does not draw them. Nested blocks
    /// then produce one paragraph for each innermost block instead of blank paragraphs.
    /// It trims remaining text at both ends because Yomitan draws the result in a browser.
    /// A browser does not indent a paragraph by whitespace that a dictionary leaves
    /// between nodes.
    pub(super) fn flush(&mut self) {
        // Both branches below invalidate every span index for the open paragraph.
        // Increment `flushes` before either branch, not only when a branch reaches the
        // output stream.
        self.flushes += 1;
        if self.cur.text.trim().is_empty() {
            self.cur.text.clear();
            self.cur.spans.clear();
            self.cur.inline.clear();
            // A marker needs a line beside it, and a dropped paragraph has none.
            // Only a paragraph that an item opened and never filled reaches this line.
            // That paragraph draws no marker in either case.
            self.cur.marker.clear();
            // The dropped paragraph's block leaves with it.
            // A `div` that contains only whitespace pays no margin, and its address belongs
            // to no element.
            self.cur.block = Block::default();
            self.cur.path = None;
            return;
        }
        let mut flow = std::mem::take(&mut self.cur);
        trim(&mut flow);
        // Renumber links within this paragraph's list.
        // An `<a>` that holds a block spans two paragraphs, and neither paragraph can use
        // an index that the other owns. This code renumbers readings for the same reason.
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

    /// Returns the link that contains a node's content.
    ///
    /// The node's link takes precedence over the inherited link. If the node has no usable
    /// link, this function returns the inherited link.
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

/// Joins every text descendant of a node into one string.
///
/// A reading and an `rp` fallback each form one text run, even when a dictionary wraps
/// the text in many nodes. This function uses the same rule as [`Paragraphs::node`]: a
/// text node has `Kind::Text` and no tag. The two walks therefore agree on the text in a
/// subtree.
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
