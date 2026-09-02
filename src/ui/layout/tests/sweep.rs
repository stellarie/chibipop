//! The corpus sweep sends dictionary entries through the production parser, stylesheet
//! cascade, and renderer.
//!
//! The sweep uses [`FakeMeasure`], the measurement seam used by this module's other
//! tests. It checks every render invariant for every scene. It uses corpus input to add
//! shapes, not new assertions. It remains in this file so the test suite and sweep use
//! the same checks.
//!
//! The corpus sweep does not run in CI. [`corpus_render_sweep`] carries `#[ignore]` and
//! reads a directory from an environment variable. The corpus does not enter the
//! repository. CI runs each invariant's unit tests. CI also runs
//! [`a_row_capped_sweep_of_the_fixture_archive_reports_every_entry`], which sweeps the
//! committed three-row fixture archive. This test checks the sweep code without the
//! corpus.
//!
//! The sweep commits one file: its [`Suppressions`]. This file lists non-bugs that a
//! human judged. The path is `tests/render-sweep/suppressions.toml`. A candidate quotes
//! dictionary content verbatim, so it never enters the repository. The repository stores
//! one verdict for each shape. A later run stays quiet about each judged shape.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;

use super::*;
use crate::config::MAX_WIDTH_RANGE;
use crate::dict::archive::{for_each_term, read_index, read_styles_css, supplies_terms};
use crate::dict::gloss::{plain_items, renders_text, GlossDoc, Kind, NodeId, NodePath, StyleKey};
use crate::dict::sheet::{self, Sheet};
use crate::present::Presentation;

/// The panel width for every corpus entry.
///
/// Other tests use this width. Candidate values therefore use the same arithmetic.
const SWEEP_W: f32 = 424.0;

/// A height that prevents the layout clamp for corpus entries.
///
/// The scene retains every stacked element at this height. The clamp changes only
/// `view_h`. The sweep requests the full height because a scrolled panel hides content.
const SWEEP_H: f32 = 100_000.0;

/// The monitor-width percentage represented by [`SWEEP_W`].
///
/// `Config::default()` sets the default cap to one quarter of the screen. This suite
/// measures 424 pixels on a 1696-pixel monitor. The test
/// [`the_swept_widths_run_from_the_default_cap_to_the_ceiling`] checks this value. The
/// wide width uses the same default value.
const SWEEP_W_PERCENT: f32 = 25.0;

/// The wider panel for the width monotonicity check.
///
/// This width uses the monitor value at the settings ceiling. [`MAX_WIDTH_RANGE`] caps
/// the width at 90 percent, so this is the widest panel a reader can request. The pair
/// contains the default width and the widest width, not the full [`MAX_WIDTH_RANGE`]
/// range. A reader can also request 10 percent. Narrower panels preserve the property
/// when they draw taller content. The pair checks that extra width does not increase
/// content height.
const SWEEP_WIDE_W: f32 = SWEEP_W * MAX_WIDTH_RANGE.1 as f32 / SWEEP_W_PERCENT;

/// Enables every editorial role for the panel.
///
/// The sweep checks content that the renderer could omit. It disables the only filter
/// that reader settings apply. The default filter moves a part-of-speech label to the
/// card field, so that label is not dropped text. An omitted subtree cannot form a
/// candidate. This filter renders the most shapes and adds the least noise.
const SWEEP_ROLES: RoleFilter =
    RoleFilter { examples: true, attributions: true, part_of_speech: true };

/// The render settings for each swept entry.
///
/// `stack_items`, `styling`, and `images` stay enabled so the renderer handles the
/// dictionary markup. A setting that omits markup could hide a defect.
fn sweep_settings() -> RenderSettings {
    RenderSettings { stack_items: true, styling: true, images: true, roles: SWEEP_ROLES }
}

/// Which render invariant a violation broke.
///
/// [`as_str`](Self::as_str) gives the candidate file its own name for the
/// invariant. One shape therefore reads the same in a filename, in a signature,
/// and in the summary.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Invariant {
    /// A visible string that the parsed entry holds but the scene does not draw.
    DroppedText,
    /// A small marked fragment that trails prose on its own scene paragraph.
    OrphanFragment,
    /// A marker gutter wider than the tree asks for between a marker and its first glyph.
    MarkerGap,
    /// A box that stands outside the panel's own edges.
    HorizontalOverflow,
    /// Two boxes that the walk stacks and that occupy the same pixels.
    OverlappingBoxes,
    /// Content that grew taller at the wider of two panels than at the narrower panel.
    WidthMonotonicity,
}

impl Invariant {
    /// Every invariant that this build checks.
    ///
    /// [`is_candidate_file`] reads this list. The sweep recognizes only its own
    /// candidate files. An absent invariant leaves stale files after later runs.
    const ALL: [Invariant; 6] = [
        Invariant::DroppedText,
        Invariant::OrphanFragment,
        Invariant::MarkerGap,
        Invariant::HorizontalOverflow,
        Invariant::OverlappingBoxes,
        Invariant::WidthMonotonicity,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Invariant::DroppedText => "dropped-text",
            Invariant::OrphanFragment => "orphan-fragment",
            Invariant::MarkerGap => "marker-gap",
            Invariant::HorizontalOverflow => "horizontal-overflow",
            Invariant::OverlappingBoxes => "overlapping-boxes",
            Invariant::WidthMonotonicity => "width-monotonicity",
        }
    }

    /// The invariant named by a signature, or `None` when the name is not valid.
    ///
    /// This function reverses [`as_str`](Self::as_str). A suppression can become
    /// `UNKNOWN` after an invariant rename or removal. Such an entry cannot absorb a
    /// violation, so the run reports it.
    fn named(name: &str) -> Option<Invariant> {
        Invariant::ALL.into_iter().find(|i| i.as_str() == name)
    }
}

/// The fingerprint that groups equivalent violations into one candidate.
///
/// It stores the invariant, dictionary, and shape around the violation. The shape has
/// one string per node from the glossary item to the node with the failure. Each string
/// stores the stylesheet selector and the resolved properties. This pair represents the
/// specification's "structural path and resolved selectors". In this tree, one walk
/// provides both values. `dict::sheet` selects tags and `data-*` hooks, and the cascade
/// adds the properties that the cascade selected to each node's style record.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Signature {
    invariant: Invariant,
    dictionary: String,
    /// The glossary item is first. The node that broke the rule is last.
    shape: Vec<String>,
}

impl Signature {
    /// The separator that [`key`](Self::key) uses between its three fields.
    ///
    /// A candidate file writes this format, and the suppression list matches it. Both
    /// must use the same separator or every exemption misses without a message.
    const FIELDS: &'static str = " | ";

    /// The shape signature as one line. This line is the key of one candidate,
    /// and the filename digest covers it.
    fn key(&self) -> String {
        let sep = Self::FIELDS;
        let shape = self.shape.join(" > ");
        format!("{}{sep}{}{sep}{shape}", self.invariant.as_str(), self.dictionary)
    }

    /// The invariant named by a signature, or `None` when this build cannot produce it.
    ///
    /// `None` covers an unknown invariant and a signature with too few fields. The
    /// function does not validate the dictionary title because the corpus is local and
    /// unbounded. A title absent from this run is an `UNUSED` entry, not a malformed
    /// entry.
    fn named_invariant(key: &str) -> Option<Invariant> {
        let mut fields = key.split(Self::FIELDS);
        let name = fields.next()?;
        // A dictionary and a shape must follow the name. A title with the separator
        // creates more fields, never fewer.
        let (_dictionary, _shape) = (fields.next()?, fields.next()?);
        Invariant::named(name)
    }
}

/// One invariant violation, before the sweep removes duplicates.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Violation {
    signature: Signature,
    /// What the check measured. The candidate file carries these numbers.
    measured: BTreeMap<String, String>,
}

/// One visible string from a parsed entry and the shape around it.
struct Visible {
    text: String,
    shape: Vec<String>,
}

/// Reduces each whitespace run to one space and trims both ends.
///
/// Every text comparison folds both values. The renderer can fold dictionary newlines
/// and repeated spaces without text loss. Only missing text is a violation.
fn folded(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut gap = false;
    for c in text.chars() {
        if c.is_whitespace() {
            gap = !out.is_empty();
            continue;
        }
        if gap {
            out.push(' ');
            gap = false;
        }
        out.push(c);
    }
    out
}

/// One node as a selector with its resolved properties.
///
/// This string contains the two parts of a shape signature. The first part contains the
/// tag and each `data-sc-*` hook that the dictionary stylesheet can select. The second
/// part contains the properties that resolve there. The function sorts both lists, so
/// field and declaration order cannot change a signature.
///
/// A hook name describes the shape, but its value does not. The `data` namespace is
/// local to each dictionary and has no fixed size (`docs/research/dict-shapes.md`).
/// Jitendex uses `data.sentence-key` and `data.source` in example boxes, and each value
/// identifies an entry. Values would create 26 186 violations for 6 892 distinct
/// shapes. Each entry would become a separate shape. The sweep can omit values and still
/// preserve shape information. The resolved property list states which stylesheet rules
/// matched and which properties the cascade selected. Two `data.content` values with the
/// same style are one renderer shape. The exemplar still quotes the entry verbatim.
fn step(doc: &GlossDoc, id: NodeId) -> String {
    let mut out = match doc.tag_name(id) {
        // A bare glossary string and a `content`-only wrapper object have no tag. They
        // are different shapes.
        "" => kind_name(doc.node(id).kind).to_string(),
        name => name.to_string(),
    };
    let mut hooks: Vec<String> =
        doc.data(id).iter().map(|(k, _)| format!("[data-sc-{}]", doc.key(*k))).collect();
    hooks.sort();
    hooks.dedup();
    for hook in hooks {
        out.push_str(&hook);
    }
    let mut props: Vec<&'static str> =
        doc.style(id).iter().map(|(k, _)| style_key_name(*k)).collect();
    props.sort_unstable();
    props.dedup();
    if !props.is_empty() {
        out.push('{');
        out.push_str(&props.join(","));
        out.push('}');
    }
    out
}

/// The name for an untagged node in a signature.
///
/// The exhaustive `match` has the same purpose as [`style_key_name`]. A new node kind
/// must get a name here. Otherwise it shares a shape with a different node, and the
/// report gives no useful detail.
fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Text => "text",
        Kind::Break => "break",
        Kind::Container => "content",
        Kind::List => "list",
        Kind::ListItem => "item",
        Kind::Table => "table",
        Kind::Row => "row",
        Kind::Cell => "cell",
        Kind::Ruby => "ruby",
        Kind::Link => "link",
        Kind::Image => "image",
        Kind::Unknown => "unknown",
    }
}

/// The name for a resolved style property in a signature.
///
/// The exhaustive `match` makes a new [`StyleKey`] variant a compile error here.
/// Without it, a new property leaves a silent gap in every affected signature.
///
/// This function does not use `css_name` from `gloss::html`. That function spells
/// [`StyleKey::TextDecorationLine`] as the `text-decoration` shorthand. The shorthand
/// predates some Anki note templates. That spelling describes browser behavior. A
/// signature names the property that the cascade resolved. The two names can differ.
fn style_key_name(key: StyleKey) -> &'static str {
    match key {
        StyleKey::FontStyle => "font-style",
        StyleKey::FontWeight => "font-weight",
        StyleKey::FontSize => "font-size",
        StyleKey::TextDecorationLine => "text-decoration-line",
        StyleKey::TextDecorationStyle => "text-decoration-style",
        StyleKey::TextDecorationColor => "text-decoration-color",
        StyleKey::VerticalAlign => "vertical-align",
        StyleKey::TextAlign => "text-align",
        StyleKey::WhiteSpace => "white-space",
        StyleKey::WordBreak => "word-break",
        StyleKey::Cursor => "cursor",
        StyleKey::ListStyleType => "list-style-type",
        StyleKey::Color => "color",
        StyleKey::BackgroundColor => "background-color",
        StyleKey::BorderColor => "border-color",
        StyleKey::BorderStyle => "border-style",
        StyleKey::BorderRadius => "border-radius",
        StyleKey::BorderWidth => "border-width",
        StyleKey::Margin => "margin",
        StyleKey::MarginTop => "margin-top",
        StyleKey::MarginRight => "margin-right",
        StyleKey::MarginBottom => "margin-bottom",
        StyleKey::MarginLeft => "margin-left",
        StyleKey::Padding => "padding",
        StyleKey::PaddingTop => "padding-top",
        StyleKey::PaddingRight => "padding-right",
        StyleKey::PaddingBottom => "padding-bottom",
        StyleKey::PaddingLeft => "padding-left",
    }
}
/// Every visible string from a parsed entry and its shape.
///
/// This function provides the reference for "no dropped text". It lists text that the
/// renderer must draw. It excludes three valid omissions. The first is a role that the
/// filter drops. The second is an image node, whose text is `alt`, because a resolved
/// image draws the asset. The third is `<rp>`, which only helps renderers that cannot
/// draw ruby. This renderer can draw ruby.
fn visible_strings(doc: &GlossDoc, roles: RoleFilter) -> Vec<Visible> {
    let mut out = Vec::new();
    let mut shape = Vec::new();
    for item in doc.items() {
        walk_visible(doc, item, roles, &mut shape, &mut out);
    }
    out
}

fn walk_visible(
    doc: &GlossDoc,
    id: NodeId,
    roles: RoleFilter,
    shape: &mut Vec<String>,
    out: &mut Vec<Visible>,
) {
    let node = *doc.node(id);
    if !roles.allows(node.role) || node.kind == Kind::Image || node.tag == Tag::Rp {
        return;
    }
    shape.push(step(doc, id));
    if node.kind == Kind::Text {
        let text = folded(doc.text(id));
        if !text.is_empty() {
            out.push(Visible { text, shape: shape.clone() });
        }
    }
    for child in doc.children(id) {
        walk_visible(doc, child, roles, shape, out);
    }
    shape.pop();
}

/// Every string that a scene draws, folded, in draw order.
///
/// The result includes a run's own text, markers, and readings that sit outside its
/// flow. A reader sees glyphs for each part, so each part represents parsed text. The
/// function joins parts with spaces instead of concatenating them. A match cannot cross
/// two paragraphs that do not touch on screen.
fn drawn_text(s: &PopupScene) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(s.elems.len());
    for e in &s.elems {
        parts.push(e.text.as_str());
        parts.extend(e.marker.iter().map(|m| m.text.as_str()));
        parts.extend(e.ruby.iter().map(|r| r.text.as_str()));
    }
    folded(&parts.join(" "))
}

/// The results that each invariant recorded for one entry.
///
/// The record keeps check counts because zero violations differs from zero checks. A
/// walk that finds no text could otherwise report a clean corpus. Each invariant has
/// its own count, so one silent invariant cannot hide behind another.
#[derive(Default)]
struct Checked {
    /// Visible strings covered by *no dropped text*.
    strings: u64,
    /// Trailing fragments covered by *no orphan trailing fragment*.
    fragments: u64,
    /// Marker boxes covered by *bounded marker gap*.
    markers: u64,
    /// Drawn boxes covered by *no horizontal overflow*.
    boxes: u64,
    /// Pairs of stacked boxes covered by *no overlapping boxes*.
    ///
    /// The count includes only pairs that the check compares. Boxes separated by a
    /// full paragraph cannot intersect, so the walk does not compare them.
    pairs: u64,
    /// Entries covered by *width monotonicity*.
    ///
    /// The count also equals the number of second layouts because the invariant compares
    /// each entry once. A zero `widths` value with nonzero `wide_cost` indicates wasted
    /// time.
    widths: u64,
    /// The cost of the second layouts.
    ///
    /// This field is not a count. It records the cost of *width monotonicity*, which
    /// doubles layout work. [`sweep_entry`] sets the field after the second layout.
    /// Other checkers report [`Duration::ZERO`].
    wide_cost: Duration,
    /// Number of times that an invariant declines a check.
    ///
    /// A fragment that no paragraph draws as one string causes one decline. A marker
    /// with no list ancestor causes another. The check has no basis for a judgment in
    /// either case, so it records the decline. A large decline count identifies a shape
    /// that needs support, not a clean corpus.
    declined: u64,
    violations: Vec<Violation>,
}

impl Checked {
    /// The answer of every invariant for one entry, as one record.
    fn merge(&mut self, other: Checked) {
        self.strings += other.strings;
        self.fragments += other.fragments;
        self.markers += other.markers;
        self.boxes += other.boxes;
        self.pairs += other.pairs;
        self.declined += other.declined;
        self.widths += other.widths;
        self.wide_cost += other.wide_cost;
        self.violations.extend(other.violations);
    }
}

/// *No dropped text*: every visible string from a parsed entry reaches the scene.
///
/// This invariant needs no threshold. The scene either draws each string or does not.
/// The check tests containment, not equality, because a paragraph can contain text from
/// more than one node. The renderer also folds whitespace. See
/// [`folded_whitespace_and_a_line_break_drop_no_text`].
fn dropped_text(
    dictionary: &str,
    doc: &GlossDoc,
    roles: RoleFilter,
    s: &PopupScene,
) -> Checked {
    let drawn = drawn_text(s);
    let mut checked = Checked::default();
    for v in visible_strings(doc, roles) {
        checked.strings += 1;
        if drawn.contains(&v.text) {
            continue;
        }
        let mut measured = BTreeMap::new();
        measured.insert("missing".to_string(), v.text);
        measured.insert("scene_runs".to_string(), s.elems.len().to_string());
        measured.insert("scene_chars".to_string(), drawn.chars().count().to_string());
        checked.violations.push(Violation {
            signature: Signature {
                invariant: Invariant::DroppedText,
                dictionary: dictionary.to_string(),
                shape: v.shape,
            },
            measured,
        });
    }
    checked
}

// ---- the paragraph family ----

/// A trailing fragment is *small* when it has at most this many drawn characters.
///
/// A long marked node on its own line represents a sense separation. The marker line
/// break supports that separation. The cap limits this invariant to fragments. Twelve
/// covers Jitendex footnote marks `[1]` through `[12]`, which make up the #18 class.
/// It also allows a mark with its own punctuation.
///
/// The local archive provides the count. Jitendex has 65 004 marked nodes that trail
/// prose. This cap checks 64 980 and excludes 24. The cap targets the other 96
/// archives. A cap that includes every marked node would apply the invariant to phrases
/// where an own line is correct.
const ORPHAN_MAX_CHARS: usize = 12;

/// The minimum amount of other text that must share a trailing fragment's paragraph.
///
/// The value is one because every #18 fragment in the local class has no other text in
/// its paragraph. The renderer puts the mark on a line below its sentence. The check
/// therefore needs company, not prose before the fragment. `example-keyword` in Jitendex has 51 062
/// nodes and often starts its own sentence. A fragment with no prose before it is
/// normal. A fragment with no prose beside it is a defect. A larger value would make
/// the renderer delay the fragment until it had part of the sentence. Browsers have no
/// such rule. Use a larger value only in
/// [`the_fixed_footnote_re_flags_when_its_company_threshold_is_tightened`].
const ORPHAN_MIN_COMPANY_CHARS: usize = 1;

/// The extra gutter allowed between a marker box and its item glyph, in ems.
///
/// The #19 class adds one default list level, [`LIST_INDENT_EM`] at 1.4em. The fixed
/// shape has the 0.5em that its dictionary declares. A tolerance below one level and
/// above round-off separates the two. Half an em also covers one round-off difference.
/// A padding declaration resolves against one em, but the check scales it with another
/// em. The declared node can use a smaller size than the paragraph at the gap.
const MARKER_GAP_SLACK_EM: f32 = 0.5;

/// The threshold values that the invariants use.
///
/// The sweep passes these values instead of reading constants directly. A detector must
/// have a test that can tighten its threshold. Each resolved defect follows three steps:
/// render the real shape, find zero candidates, then tighten one threshold until the
/// checker flags the same geometry. These tests show that the checkers detect the defects
/// that they describe.
#[derive(Clone, Copy, Debug)]
struct Thresholds {
    orphan_max_chars: usize,
    orphan_min_company: usize,
    marker_gap_slack_em: f32,
    overflow_slack_px: f32,
    overlap_slack_px: f32,
    monotonic_slack_px: f32,
}

impl Thresholds {
    /// The threshold values that a sweep uses.
    const DEFAULT: Thresholds = Thresholds {
        orphan_max_chars: ORPHAN_MAX_CHARS,
        orphan_min_company: ORPHAN_MIN_COMPANY_CHARS,
        marker_gap_slack_em: MARKER_GAP_SLACK_EM,
        overflow_slack_px: OVERFLOW_SLACK_PX,
        overlap_slack_px: OVERLAP_SLACK_PX,
        monotonic_slack_px: MONOTONIC_SLACK_PX,
    };
}

/// One small marked fragment that trails prose and the shape around it.
struct Fragment {
    text: String,
    shape: Vec<String>,
}

/// Every small marked fragment that trails prose inside its parent.
///
/// This function provides the reference for *no orphan trailing fragment*. The marker
/// line break is exempt from this walk. A marked inline node whose parent already has
/// prose belongs inside that sentence ([`GlossDoc::inline_prose`]), so the renderer
/// owes it the sentence's paragraph. The function excludes nodes that the walk splits
/// before it checks them. A marked node with no preceding prose separates senses. A
/// marked node that contains a block opens its own line because of its tag, not its
/// mark.
fn trailing_fragments(doc: &GlossDoc, roles: RoleFilter, t: Thresholds) -> Vec<Fragment> {
    let mut out = Vec::new();
    let mut shape = Vec::new();
    // No fragment can trail the top-level item. The walk finds fragments only in its
    // children.
    for item in doc.items() {
        walk_fragments(doc, item, roles, false, t, &mut shape, &mut out);
    }
    out
}

/// `prose` records text that the walk has seen in this parent's children.
///
/// The value follows parent order, like [`Paragraphs::children`]. A sense head has
/// marked pills before its prose fragment. Prose after a marked node does not support
/// the fragment as trailing prose.
///
/// [`Paragraphs::children`]: super::gloss::Paragraphs::children
fn walk_fragments(
    doc: &GlossDoc,
    id: NodeId,
    roles: RoleFilter,
    prose: bool,
    t: Thresholds,
    shape: &mut Vec<String>,
    out: &mut Vec<Fragment>,
) {
    let node = *doc.node(id);
    if !roles.allows(node.role) || node.kind == Kind::Image || node.tag == Tag::Rp {
        return;
    }
    shape.push(step(doc, id));
    if prose && doc.has_marker(id) && inline_only(doc, id) {
        let text = folded(&unglued(&run_text(doc, id)));
        if !text.is_empty() && text.chars().count() <= t.orphan_max_chars {
            out.push(Fragment { text, shape: shape.clone() });
        }
    }
    let mut ahead = doc.prose(id);
    for child in doc.children(id) {
        walk_fragments(doc, child, roles, ahead, t, shape, out);
        ahead = ahead || doc.inline_prose(child);
    }
    shape.pop();
}

/// Returns true when a node contains only inline content.
///
/// A marked node that contains a `div`, list, table, or image starts its own line. The
/// renderer must not keep that node in the sentence. Its text size does not change this
/// rule.
fn inline_only(doc: &GlossDoc, id: NodeId) -> bool {
    let node = doc.node(id);
    !node.tag.is_block()
        && !matches!(
            node.kind,
            Kind::List | Kind::ListItem | Kind::Table | Kind::Row | Kind::Cell | Kind::Image
        )
        && doc.children(id).all(|child| inline_only(doc, child))
}

/// The text of one node as a continuous run, excluding text that the reader does not
/// see.
///
/// The function joins spans without separators. Spans in one run follow each other
/// directly, so the result is the string that the search uses. It excludes three child
/// types:
///
/// - `<rt>` readings do not flow. [`RubyBox`] draws them separately through
///   [`measure_readings`].
/// - `<rp>` is excluded for the reason [`visible_strings`] gives.
/// - An image draws its `alt` text as an asset.
///
/// [`measure_readings`]: super::ruby::measure_readings
fn run_text(doc: &GlossDoc, id: NodeId) -> String {
    let node = doc.node(id);
    if matches!(node.tag, Tag::Rt | Tag::Rp) || node.kind == Kind::Image {
        return String::new();
    }
    if node.kind == Kind::Text {
        return doc.text(id).to_string();
    }
    let mut out = String::new();
    for child in doc.children(id) {
        out.push_str(&run_text(doc, child));
    }
    out
}

/// Removes glyphs that the reader cannot see from a run.
///
/// [`RUBY_FILLER`] is the word joiner that each ruby base contains. It sits between
/// base characters. A tree string cannot match a drawn run that includes this filler
/// unless both values omit it. The function compares against the filler itself, so its
/// codepoint cannot drift from the renderer. It also removes the zero-width space
/// because the shaper gives it no advance ([`zero_advance`](super::tests::zero_advance)).
/// Neither character changes the shape.
fn unglued(text: &str) -> String {
    let mut buf = [0u8; 4];
    text.chars()
        .filter(|c| c.encode_utf8(&mut buf) != RUBY_FILLER && *c != '\u{200b}')
        .collect()
}

/// *No orphan trailing fragment*: a small marked fragment that trails prose stays in
/// that paragraph, not on its own line.
///
/// The check applies this rule to drawn paragraphs, not to the walk's decisions. It
/// therefore checks the final placement. The measured value is text beside the
/// fragment in its paragraph. #18 has zero company. A fragment that the scene does not
/// draw belongs to *no dropped text*, not to this invariant.
fn orphan_fragment(
    dictionary: &str,
    doc: &GlossDoc,
    roles: RoleFilter,
    s: &PopupScene,
    t: Thresholds,
) -> Checked {
    let drawn: Vec<String> = s.elems.iter().map(|e| folded(&unglued(&e.text))).collect();
    let mut checked = Checked::default();
    for f in trailing_fragments(doc, roles, t) {
        // A fragment belongs to *no dropped text* when the scene does not draw it as one
        // string. A missing fragment would make this invariant appear to work without
        // evidence.
        let Some(company) = best_company(&drawn, &f.text) else {
            checked.declined += 1;
            continue;
        };
        checked.fragments += 1;
        if company >= t.orphan_min_company {
            continue;
        }
        let mut measured = BTreeMap::new();
        measured.insert("fragment".to_string(), f.text.clone());
        measured.insert("fragment_chars".to_string(), f.text.chars().count().to_string());
        measured.insert("company_chars".to_string(), company.to_string());
        measured.insert("min_company_chars".to_string(), t.orphan_min_company.to_string());
        checked.violations.push(Violation {
            signature: Signature {
                invariant: Invariant::OrphanFragment,
                dictionary: dictionary.to_string(),
                shape: f.shape,
            },
            measured,
        });
    }
    checked
}

/// The largest amount of text that one paragraph draws beside `fragment`, or `None` when
/// no paragraph draws the fragment.
///
/// This function reports the best case instead of the first case. A short fragment can
/// appear in several paragraphs. The question is whether the renderer keeps it inside
/// a sentence anywhere. One paragraph that keeps it is enough. A flag for another
/// occurrence would add noise.
fn best_company(drawn: &[String], fragment: &str) -> Option<usize> {
    let own = fragment.chars().count();
    drawn
        .iter()
        .filter(|text| text.contains(fragment))
        .map(|text| text.chars().count().saturating_sub(own))
        .max()
}

/// *Bounded marker gap*: the space between a marker box and its item's first glyph is
/// within the tree's requested value.
///
/// [`place_markers`] right-aligns the marker against the list content edge. The
/// space left to the paragraph pen equals the indent from levels below that list.
/// The reference follows the browser rule for #19. Each level costs [`LIST_INDENT_EM`]
/// unless the list declares left padding. That declaration replaces the default level.
/// Each node adds its declared left margin, border, and padding.
///
/// The check examines only the innermost marker. A nested list can place two markers on
/// one line. The outer marker must remain one level from the text.
fn marker_gap(dictionary: &str, doc: &GlossDoc, s: &PopupScene, t: Thresholds) -> Checked {
    let mut checked = Checked::default();
    for e in &s.elems {
        let Some(mark) = e.marker.last() else { continue };
        // Decline a paragraph with no address or a list that this sweep cannot align.
        // A decline is not a check. The count must show when origin resolution stops.
        // Otherwise `markers=` could remain unchanged while the judgment stops.
        let Some(chain) = e.origin.and_then(|o| o.path).and_then(|p| chain_of(doc, p)) else {
            checked.declined += 1;
            continue;
        };
        let Some(at) = marker_list(doc, &chain) else {
            checked.declined += 1;
            continue;
        };
        checked.markers += 1;
        // The marker's trailing gap is inside its width ([`MARKER_GAP`]). This gap is
        // the indent from levels below the marker's list, not marker spacing.
        let gap = -(mark.x + mark.w);
        if gap <= 0.0 {
            continue;
        }
        let owed = owed_indent_em(doc, &chain[at + 1..]);
        let allowed = (owed + t.marker_gap_slack_em) * e.font_size;
        if gap <= allowed {
            continue;
        }
        let mut measured = BTreeMap::new();
        measured.insert("gap_px".to_string(), format!("{gap:.2}"));
        measured.insert("owed_em".to_string(), format!("{owed:.3}"));
        measured.insert("slack_em".to_string(), format!("{:.3}", t.marker_gap_slack_em));
        measured.insert("em_px".to_string(), format!("{:.2}", e.font_size));
        measured.insert("marker".to_string(), mark.text.clone());
        checked.violations.push(Violation {
            signature: Signature {
                invariant: Invariant::MarkerGap,
                dictionary: dictionary.to_string(),
                shape: chain.iter().map(|id| step(doc, *id)).collect(),
            },
            measured,
        });
    }
    checked
}

/// The node IDs that a path addresses, from glossary item to final node.
///
/// The arena has first-child and next-sibling links, but no parent link. A scene
/// element's [`NodePath`] is therefore the only route back through the tree. The gap
/// check needs each ancestor between the marker and glyph.
///
/// [`NodePath::resolve`] walks the same route but keeps only the final node. The path
/// has no prefix constructor for one-step access. This function repeats the descent so
/// both routes change together if the path format changes. It visits the nth item, then
/// the nth child.
fn chain_of(doc: &GlossDoc, path: NodePath) -> Option<Vec<NodeId>> {
    let mut steps = path.steps().iter();
    let mut id = doc.items().nth(*steps.next()? as usize)?;
    let mut out = vec![id];
    for step in steps {
        id = doc.children(id).nth(*step as usize)?;
        out.push(id);
    }
    Some(out)
}

/// The index of the list that owns the innermost drawn marker on `chain`.
///
/// The deepest list ancestor that draws a marker for its child owns the marker gutter
/// ([`Paragraphs::list`]). `None` means no ancestor list draws the marker. This sweep
/// does not judge that case.
///
/// [`Paragraphs::list`]: super::marker::Paragraphs::list
fn marker_list(doc: &GlossDoc, chain: &[NodeId]) -> Option<usize> {
    (0..chain.len().saturating_sub(1)).rev().find(|&i| {
        let (list, item) = (chain[i], chain[i + 1]);
        doc.node(list).kind == Kind::List
            && doc.node(item).kind == Kind::ListItem
            && marker_draws(doc, list, item)
    })
}

/// Returns true when `list` draws a marker beside `item`.
///
/// `list-style-type` is inherited. The list resolves its value over the tag's initial
/// value, then the item resolves its value over the list value. [`Paragraphs::list`]
/// makes these calls in this order. The ordinal does not affect marker existence, so
/// the function asks for the first item.
fn marker_draws(doc: &GlossDoc, list: NodeId, item: NodeId) -> bool {
    let ordered = doc.node(list).tag == Tag::Ol;
    let inherited = styled_marker(doc, list, initial_marker(ordered), true);
    styled_marker(doc, item, inherited, true).label(1).is_some()
}

/// The left indent owed by nodes below a marker list, in ems.
///
/// Each node contributes its declared value. A list with no left padding contributes
/// [`LIST_INDENT_EM`] as the default list level. An author's `padding-left` replaces the
/// browser gutter. This rule follows the #19 fix. The function states the rule directly
/// instead of duplicating the layout arithmetic.
fn owed_indent_em(doc: &GlossDoc, below: &[NodeId]) -> f32 {
    below
        .iter()
        .map(|&id| {
            let pad = left_of(doc, id, StyleKey::Padding, Some(StyleKey::PaddingLeft));
            let lead = pad
                + left_of(doc, id, StyleKey::Margin, Some(StyleKey::MarginLeft))
                + left_of(doc, id, StyleKey::BorderWidth, None);
            let level =
                if doc.node(id).kind == Kind::List && pad <= 0.0 { LIST_INDENT_EM } else { 0.0 };
            lead + level
        })
        .sum()
}

/// One node's declared left edge for a box property, in its own ems.
///
/// The result is the larger left edge from the shorthand and longhand. This matches
/// cascade order and overestimates allowed space, so a declaration cannot create a
/// false candidate. The result is resolved against one em, which gives the edge
/// multiple. The comparison uses paragraph size. [`MARKER_GAP_SLACK_EM`] covers a node
/// that declares padding at another size.
fn left_of(doc: &GlossDoc, id: NodeId, short: StyleKey, long: Option<StyleKey>) -> f32 {
    let one = Ems { own: 1.0, root: 1.0 };
    let edges = doc
        .style_of(id, short)
        .and_then(|v| box_edges(doc, v, one))
        .map_or(0.0, |e| e.left);
    let edge = long
        .and_then(|key| doc.style_of(id, key))
        .and_then(|v| box_len(doc, v, one))
        .unwrap_or(0.0);
    edges.max(edge).max(0.0)
}

// ---- the box family ----

/// The allowed distance beyond the panel edge before the check reports overflow, in pixels.
///
/// Half a pixel is below one device pixel. It covers float arithmetic from box
/// placement. The value uses pixels, not ems, because a runaway indent must not increase
/// its own tolerance with font size.
const OVERFLOW_SLACK_PX: f32 = 0.5;

/// The required overlap on both axes before two boxes overlap, in pixels.
///
/// Half a pixel covers float arithmetic. Stacked boxes meet edge to edge by design, so
/// this invariant tests penetration, not contact. The value must exceed the rounding
/// error at both edges.
const OVERLAP_SLACK_PX: f32 = 0.5;

/// One element's own text, cut to this many characters for a candidate file.
const SNIPPET_CHARS: usize = 40;

/// The panel width that bounds each box.
///
/// A scene reports its requested width only when a side column expands it beyond the
/// offered box. Without a side column, the main column and two paddings define the box.
/// Read the width from the scene instead of [`SWEEP_W`] so the invariant also applies to
/// other panel widths.
fn panel_width(s: &PopupScene) -> f32 {
    s.panel_w.unwrap_or(2.0 * s.origin + s.content_w)
}

/// One box that an element puts on the panel.
struct DrawnBox<'a> {
    /// The box name in a signature.
    name: &'static str,
    /// The text inside the box.
    text: &'a str,
    /// The rectangle in the scene's panel space.
    rect: SceneRect,
}

/// Every box that an element puts on the panel, in panel space.
///
/// The first box is the element's ink box. The other boxes sit outside its flow: a
/// reading above its base and a marker in its list gutter. The scene stores both as
/// run-relative coordinates. A platform adds [`SceneElem::pen`]. Each box has a name
/// because a paragraph with overflow and an outside marker have different defects.
///
/// Angle brackets mark names that no glossary node can create. A shape step uses a
/// node selector, but these names identify scene boxes.
fn drawn_boxes(e: &SceneElem) -> impl Iterator<Item = DrawnBox<'_>> {
    let (px, py) = e.pen;
    std::iter::once(DrawnBox { name: "<element-box>", text: &e.text, rect: e.rect })
        .chain(e.ruby.iter().map(move |r| DrawnBox {
            name: "<ruby-box>",
            text: &r.text,
            rect: SceneRect { x: px + r.x, y: py + r.y, w: r.w, h: r.h },
        }))
        .chain(e.marker.iter().map(move |m| DrawnBox {
            name: "<marker-box>",
            text: &m.text,
            rect: SceneRect { x: px + m.x, y: py + m.y, w: m.w, h: m.h },
        }))
}

/// The shape around a scene element, or `None` when the sweep cannot name it.
///
/// A gloss element uses its address and the nodes on that path, as [`marker_gap`] does.
/// Panel chrome has no address, so its shape uses its element kind. A headword that
/// overflows is a renderer defect. Distinct defects must remain distinct.
///
/// `None` marks a gloss node deeper than [`NodePath`] can reach. Do not replace it with
/// a chrome name. One chrome exemption could then absorb every deep-tree defect. A
/// caller declines such a box, as [`marker_gap`] declines an unaligned marker.
///
/// Chrome names use angle brackets. `step` writes a tag and `data-sc-*` hooks. A
/// dictionary can choose any node tag, so a sweep name must not match one.
fn elem_shape(doc: &GlossDoc, e: &SceneElem) -> Option<Vec<String>> {
    let Some(origin) = e.origin else {
        return Some(vec![format!("<chrome:{}>", e.kind.as_str())]);
    };
    let chain = chain_of(doc, origin.path?)?;
    Some(chain.iter().map(|id| step(doc, *id)).collect())
}

/// Folds `text` and limits it to [`SNIPPET_CHARS`] characters.
///
/// A measured value shows that two boxes overlap. The text shows which boxes they are.
/// This helps review. Glossary paragraphs can contain hundreds of characters, but the
/// exemplar already quotes the full entry.
fn snippet(text: &str) -> String {
    let text = folded(text);
    match text.char_indices().nth(SNIPPET_CHARS) {
        Some((at, _)) => format!("{}\u{2026}", &text[..at]),
        None => text,
    }
}

/// *No horizontal overflow*: every box that the panel draws stays inside its edges.
///
/// Runaway indents and unbreakable lines are the two target shapes. An indent moves
/// the pen past the right edge. A chunk that the shaper cannot break moves the ink past
/// the wrap width. [`FakeMeasure`] overflows in that case, like a real shaper. The check
/// reads the placed box, so it tests the result of every layout rule.
///
/// The invariant uses panel edges, not content edges. Markers sit left of the content
/// edge by design. The list indent provides their space. The panel edge marks what a
/// reader can lose.
fn horizontal_overflow(
    dictionary: &str,
    doc: &GlossDoc,
    s: &PopupScene,
    t: Thresholds,
) -> Checked {
    let panel_w = panel_width(s);
    let mut checked = Checked::default();
    for e in &s.elems {
        for b in drawn_boxes(e) {
            checked.boxes += 1;
            let (right, edge) = (b.rect.x + b.rect.w, panel_w + t.overflow_slack_px);
            let (side, over) = if b.rect.x < -t.overflow_slack_px {
                ("left", -b.rect.x)
            } else if right > edge {
                ("right", right - panel_w)
            } else {
                continue;
            };
            let mut measured = BTreeMap::new();
            measured.insert("box".to_string(), b.name.to_string());
            measured.insert("side".to_string(), side.to_string());
            measured.insert("over_px".to_string(), format!("{over:.2}"));
            measured.insert("x_px".to_string(), format!("{:.2}", b.rect.x));
            measured.insert("w_px".to_string(), format!("{:.2}", b.rect.w));
            measured.insert("panel_w_px".to_string(), format!("{panel_w:.2}"));
            measured.insert("text".to_string(), snippet(b.text));
            // If the sweep cannot name the element, it declines the box instead of
            // assigning another shape.
            let Some(mut shape) = elem_shape(doc, e) else {
                checked.declined += 1;
                continue;
            };
            shape.push(b.name.to_string());
            checked.violations.push(Violation {
                signature: Signature {
                    invariant: Invariant::HorizontalOverflow,
                    dictionary: dictionary.to_string(),
                    shape,
                },
                measured,
            });
        }
    }
    checked
}

/// Which promise a box kind carries, or `None` when it carries no promise.
///
/// This lists every exemption for *no overlapping boxes*. The exhaustive match makes a
/// new scene kind a compile error instead of a silent candidate flood. The check
/// compares only boxes from the same promise set:
///
/// - [`Stack::Flow`] contains boxes that the walk stacks in sequence. A mis-stacked row
///   has two boxes from this set on shared pixels.
/// - [`Stack::Assets`] contains images. Each image keeps the spacer run's space and
///   can overlap that paragraph. Two images at one place form the double-drawn element
///   that this invariant detects.
///
/// Containers carry no promise. A block, table, or cell leads its inner paragraphs and
/// covers them. A comparison would report each bordered `div` as a collision
/// with its contents. A corner also carries no promise. It uses reserved headword
/// width and adds no vertical space.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stack {
    /// Boxes that the walk stacks down the panel.
    Flow,
    /// Assets composited over flow boxes.
    Assets,
}

fn stack_of(kind: ElemKind) -> Option<Stack> {
    match kind {
        ElemKind::Separator
        | ElemKind::Text
        | ElemKind::Collapsed
        | ElemKind::Headword
        | ElemKind::Pitch
        | ElemKind::BackButton => Some(Stack::Flow),
        ElemKind::Image => Some(Stack::Assets),
        ElemKind::Corner | ElemKind::Block | ElemKind::Table | ElemKind::Cell => None,
    }
}

/// *No overlapping boxes*: stacked boxes do not share interior pixels, and assets do not
/// cover other assets.
///
/// Mis-stacked rows and double-drawn elements follow each box's promise set
/// ([`stack_of`]). The check compares element rectangles only. Ruby and marker boxes are
/// excluded because they are not element rectangles. A reading sits inside the line its
/// base reserves ([`measure_readings`]). If the check included it, every ruby would appear
/// as a collision against its paragraph.
///
/// [`measure_readings`]: super::ruby::measure_readings
fn overlapping_boxes(
    dictionary: &str,
    doc: &GlossDoc,
    s: &PopupScene,
    t: Thresholds,
) -> Checked {
    let mut checked = Checked::default();
    let mut boxes: Vec<&SceneElem> =
        s.elems.iter().filter(|e| stack_of(e.kind).is_some()).collect();
    // Sort by top edge, then keep draw order for ties. `sort_by` is stable, so the loop
    // can stop at the first later box and name each pair in one order.
    boxes.sort_by(|a, b| a.rect.y.total_cmp(&b.rect.y));
    for (i, upper) in boxes.iter().enumerate() {
        let (top, bottom) = (upper.rect.y, upper.rect.y + upper.rect.h);
        for lower in &boxes[i + 1..] {
            // The sort puts every later box lower on the panel.
            if lower.rect.y >= bottom - t.overlap_slack_px {
                break;
            }
            // Compare only boxes with the same promise. An asset over a paragraph is
            // valid compositing.
            if stack_of(upper.kind) != stack_of(lower.kind) {
                continue;
            }
            checked.pairs += 1;
            let across = (upper.rect.x + upper.rect.w).min(lower.rect.x + lower.rect.w)
                - upper.rect.x.max(lower.rect.x);
            let down = bottom.min(lower.rect.y + lower.rect.h) - top.max(lower.rect.y);
            if across <= t.overlap_slack_px || down <= t.overlap_slack_px {
                continue;
            }
            let mut measured = BTreeMap::new();
            measured.insert("overlap_w_px".to_string(), format!("{across:.2}"));
            measured.insert("overlap_h_px".to_string(), format!("{down:.2}"));
            measured.insert("upper_kind".to_string(), upper.kind.as_str().to_string());
            measured.insert("lower_kind".to_string(), lower.kind.as_str().to_string());
            measured.insert("upper".to_string(), snippet(&upper.text));
            measured.insert("lower".to_string(), snippet(&lower.text));
            // Decline a pair when either box has no sweep name. See
            // [`horizontal_overflow`].
            let (Some(mut shape), Some(under)) =
                (elem_shape(doc, upper), elem_shape(doc, lower))
            else {
                checked.declined += 1;
                continue;
            };
            shape.push("<overlaps>".to_string());
            shape.extend(under);
            checked.violations.push(Violation {
                signature: Signature {
                    invariant: Invariant::OverlappingBoxes,
                    dictionary: dictionary.to_string(),
                    shape,
                },
                measured,
            });
        }
    }
    checked
}

// ---- width monotonicity ----

/// The height increase allowed for the wider panel before a violation, in pixels.
///
/// Half a pixel covers float arithmetic in the two content-height sums. It is not wrap
/// tolerance. One unwanted line is much larger. The value uses pixels because this
/// invariant measures content height, not font-scaled space.
const MONOTONIC_SLACK_PX: f32 = 0.5;

/// *Width monotonicity*: a wider panel does not draw taller content.
/// This invariant compares two renders of one entry. Extra width gives each line at
/// least as much room, so content height can stay the same or decrease. An increase
/// shows that another rule uses the extra width. Examples include a panel-scaled indent,
/// a new column break, or a box that fits at one width only. A reader sees a longer entry
/// after a wider popup.
///
/// The check uses `content_h`, which includes the body, both paddings, and the panel
/// size. Neither scene is clamped. [`SWEEP_H`] exceeds every corpus entry, and the
/// clamp only sets `view_h`.
fn width_monotonicity(
    dictionary: &str,
    doc: &GlossDoc,
    narrow: &PopupScene,
    wide: &PopupScene,
    t: Thresholds,
) -> Checked {
    let mut checked = Checked { widths: 1, ..Checked::default() };
    let taller = wide.content_h - narrow.content_h;
    if taller <= t.monotonic_slack_px {
        return checked;
    }
    let mut measured = BTreeMap::new();
    measured.insert("narrow_w_px".to_string(), format!("{:.2}", panel_width(narrow)));
    measured.insert("wide_w_px".to_string(), format!("{:.2}", panel_width(wide)));
    measured.insert("narrow_h_px".to_string(), format!("{:.2}", narrow.content_h));
    measured.insert("wide_h_px".to_string(), format!("{:.2}", wide.content_h));
    measured.insert("taller_px".to_string(), format!("{taller:.2}"));
    let shape = match grown_shape(doc, narrow, wide, t) {
        Some((shape, grew)) => {
            measured.insert("shape_taller_px".to_string(), format!("{grew:.2}"));
            shape
        }
        // No named shape grew. The increase came from space between boxes, such as a
        // gap or margin, or from padding that [`shape_heights`] excludes. File the
        // violation under the entry because no node owns that space. Angle brackets
        // keep this name outside dictionary node names, as in [`elem_shape`].
        None => vec!["<entry>".to_string()],
    };
    checked.violations.push(Violation {
        signature: Signature {
            invariant: Invariant::WidthMonotonicity,
            dictionary: dictionary.to_string(),
            shape,
        },
        measured,
    });
    checked
}

/// The shape whose own boxes grew most between widths, with the growth amount.
///
/// This puts entries with the same markup into one candidate. An entry-level shape gives
/// a reviewer no reusable detail. A node shape lets one exemplar describe many entries.
///
/// `None` means no named shape grew. The caller uses the entry shape instead. The total
/// height still defines the violation. This function only attributes the increase.
fn grown_shape(
    doc: &GlossDoc,
    narrow: &PopupScene,
    wide: &PopupScene,
    t: Thresholds,
) -> Option<(Vec<String>, f32)> {
    let before = shape_heights(doc, narrow);
    let mut worst: Option<(Vec<String>, f32)> = None;
    for (shape, after) in shape_heights(doc, wide) {
        let grew = after - before.get(&shape).copied().unwrap_or(0.0);
        if grew <= t.monotonic_slack_px {
            continue;
        }
        if worst.as_ref().is_none_or(|(_, most)| grew > *most) {
            worst = Some((shape, grew));
        }
    }
    worst
}

/// Each shape that a scene draws, with the total height of its boxes.
///
/// The key matches the shape stored in a signature. The function sums each shape, not
/// individual boxes. Two widths can wrap one shape into different box counts, but
/// monotonicity applies to the total.
///
/// A box that this sweep cannot name is omitted. [`elem_shape`] declines a node deeper
/// than [`NodePath`] can reach. A shared bucket could assign one shape's growth to another.
fn shape_heights(doc: &GlossDoc, s: &PopupScene) -> BTreeMap<Vec<String>, f32> {
    let mut out: BTreeMap<Vec<String>, f32> = BTreeMap::new();
    for e in &s.elems {
        let Some(shape) = elem_shape(doc, e) else { continue };
        *out.entry(shape).or_insert(0.0) += e.rect.h;
    }
    out
}

// ---- the suppression list ----

/// The committed list of reviewed non-bugs.
///
/// A sweep's judgment must accumulate. Otherwise each run presents shapes that the last
/// run already judged safe. This list maps a shape signature to a one-line reason in a
/// file that a human reviews. It does not quote dictionary content, so it is the only
/// sweep data that can enter the repository.
///
/// One lookup absorbs a violation. The summary stays in a stable order regardless of
/// file order.
#[derive(Default)]
struct Suppressions {
    entries: BTreeMap<String, Suppression>,
}

/// One adjudicated non-bug and the work it absorbed.
struct Suppression {
    /// Why the shape is not a bug, as the file states.
    reason: String,
    /// `None` when this build checks no such invariant. The entry cannot absorb a
    /// violation, so the run reports it instead of calling it `UNUSED`.
    invariant: Option<Invariant>,
    /// Violations that this entry absorbed in this run.
    ///
    /// Zero after a full run means `UNUSED`. The run reports an exemption that it did not
    /// use, so the list does not grow without review.
    absorbed: u64,
}

/// The suppression list as the committed file spells it.
///
/// `deny_unknown_fields` defines the complete format. A misspelled `resaon` would
/// otherwise create an exemption without a reason.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SuppressFile {
    #[serde(default)]
    suppress: Vec<SuppressEntry>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SuppressEntry {
    signature: String,
    reason: String,
}

impl Suppressions {
    /// Reads the committed list.
    fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading the suppression list at {}", path.display()))?;
        Self::parse(&text)
            .with_context(|| format!("in the suppression list at {}", path.display()))
    }

    /// One entry per shape, or an error that names the entry that stopped the parse.
    ///
    /// Every defect in a *committed* file is an error, not a warning. An entry without
    /// a reason exempts a shape that nobody can review. A duplicate signature hides one
    /// of the reasons that a reviewer approved.
    fn parse(text: &str) -> anyhow::Result<Self> {
        let file: SuppressFile = toml::from_str(text)?;
        let mut entries = BTreeMap::new();
        for e in file.suppress {
            let signature = e.signature.trim().to_string();
            let reason = e.reason.trim().to_string();
            anyhow::ensure!(!signature.is_empty(), "an entry with an empty signature");
            anyhow::ensure!(!reason.is_empty(), "{signature}: an entry with no reason");
            anyhow::ensure!(
                !reason.contains('\n'),
                "{signature}: a reason is one line, and this one is not",
            );
            let invariant = Signature::named_invariant(&signature);
            let seen = entries.insert(signature.clone(), Suppression {
                reason,
                invariant,
                absorbed: 0,
            });
            anyhow::ensure!(seen.is_none(), "{signature}: listed twice");
        }
        Ok(Suppressions { entries })
    }

    /// Absorbs one violation, if the entry claims its shape.
    fn absorb(&mut self, key: &str) -> bool {
        match self.entries.get_mut(key) {
            Some(e) => {
                e.absorbed += 1;
                true
            }
            None => false,
        }
    }
}

// ---- candidates and the report ----

/// One deduplicated violation that awaits review.
///
/// Each candidate stores one shape, one exemplar, and an occurrence count. The exemplar
/// is the first entry with the shape, not the worst entry. A stable exemplar keeps
/// candidate file diffs to count changes.
struct Candidate {
    signature: Signature,
    occurrences: u64,
    exemplar: Exemplar,
}

/// The one entry that a candidate quotes.
struct Exemplar {
    /// The one-based row number in the archive.
    ///
    /// The sweep reads archives, not the built store. This value is not the
    /// `entry_id` that a built `chibipop.sqlite` assigns. The headword beside it locates
    /// the row again.
    row: i64,
    term: String,
    reading: String,
    measured: BTreeMap<String, String>,
    /// The term-bank row's glossary, verbatim.
    ///
    /// Review uses the dictionary author's exact content. A summarized tree would add a
    /// second interpretation. This is the only report string with license impact, so
    /// candidates never enter the repository.
    glossary: String,
}

/// The results from one dictionary's sweep.
#[derive(Default)]
struct DictSummary {
    dictionary: String,
    entries: u64,
    /// Visible strings checked by *no dropped text*.
    strings: u64,
    /// Trailing fragments checked by *no orphan trailing fragment*.
    fragments: u64,
    /// Marker boxes checked by *bounded marker gap*.
    markers: u64,
    /// Drawn boxes checked by *no horizontal overflow*.
    boxes: u64,
    /// Pairs of stacked boxes checked by *no overlapping boxes*.
    pairs: u64,
    /// Entries checked by *width monotonicity*.
    ///
    /// This count also gives the number of second layouts that the run paid for.
    widths: u64,
    /// What those second layouts cost. See [`Checked::wide_cost`].
    wide_cost: Duration,
    /// Wall-clock cost of sweeping this archive.
    ///
    /// This is the denominator for the second-layout share. *Width monotonicity* doubles
    /// layout work, so its share tells a reviewer how much time that invariant uses.
    elapsed: Duration,
    declined: u64,
    violations: u64,
    /// Violations that a committed suppression absorbed.
    ///
    /// Keep this count beside `violations`, not inside it. A lower violation count would
    /// hide an exemption from review.
    suppressed: u64,
    candidates: u64,
    /// Entries with parse or layout panics, plus a refused archive walk.
    ///
    /// An unreadable bank file counts as one archive error.
    errors: u64,
}

/// Result after the sweep records one violation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Filed {
    /// A committed suppression claimed the shape. No candidate exists.
    Suppressed,
    /// A repeated shape increments its candidate count.
    Repeat,
    /// A new shape opens a candidate.
    Fresh,
}

/// What one sweep saw.
struct Report {
    dicts: Vec<DictSummary>,
    /// Entries keyed by [`Signature::key`]. One shape has one entry, regardless of how
    /// many entries show it.
    candidates: BTreeMap<String, Candidate>,
    /// Suppression entries applied in this run.
    suppress: Suppressions,
    /// Invariants checked by this run.
    ///
    /// This field stays here because the summary must report the run's coverage. It
    /// prevents a narrowed run from calling other invariants' exemptions `UNUSED`.
    /// Those invariants received no violations.
    only: Vec<Invariant>,
    /// True when this run left rows unchecked.
    ///
    /// The walk sets this after a row cap, unreadable bank file, or entry panic. This
    /// value records what happened, not only the requested cap. It prevents the run from
    /// labeling an exemption `UNUSED` when an unread row could have used it.
    partial: bool,
}

impl Default for Report {
    /// A complete sweep with every invariant enabled, no suppressions, and no rows seen.
    ///
    /// The method sets `only` explicitly. An empty `only` would mean that no invariant
    /// ran. The default must represent a caller that provided no options.
    fn default() -> Self {
        Report {
            dicts: Vec::new(),
            candidates: BTreeMap::new(),
            suppress: Suppressions::default(),
            only: Invariant::ALL.to_vec(),
            partial: false,
        }
    }
}

impl Report {
    /// Files one violation as suppressed, repeated, or fresh.
    ///
    /// Suppression comes first. A suppressed shape needs no exemplar and creates no
    /// candidate file.
    fn record(&mut self, v: Violation, exemplar: impl FnOnce() -> Exemplar) -> Filed {
        let key = v.signature.key();
        if self.suppress.absorb(&key) {
            return Filed::Suppressed;
        }
        match self.candidates.get_mut(&key) {
            Some(c) => {
                c.occurrences += 1;
                Filed::Repeat
            }
            None => {
                let mut exemplar = exemplar();
                exemplar.measured = v.measured;
                self.candidates
                    .insert(key, Candidate { signature: v.signature, occurrences: 1, exemplar });
                Filed::Fresh
            }
        }
    }

    /// Combines every dictionary summary into one total row.
    ///
    /// The summary and manifest use this row, so both report identical counts.
    fn totals(&self) -> DictSummary {
        let mut total = DictSummary { dictionary: "TOTAL".into(), ..DictSummary::default() };
        for d in &self.dicts {
            total.entries += d.entries;
            total.strings += d.strings;
            total.fragments += d.fragments;
            total.markers += d.markers;
            total.boxes += d.boxes;
            total.pairs += d.pairs;
            total.widths += d.widths;
            total.wide_cost += d.wide_cost;
            total.elapsed += d.elapsed;
            total.declined += d.declined;
            total.violations += d.violations;
            total.suppressed += d.suppressed;
            total.errors += d.errors;
        }
        total.candidates = self.candidates.len() as u64;
        total
    }

    /// Builds the complete run summary printed by `--nocapture`.
    ///
    /// A string makes absorbed counts visible to tests. A criterion that no test can
    /// read is not an enforced criterion.
    fn summary(&self) -> String {
        let mut out = String::new();
        for d in &self.dicts {
            out.push_str(&summary_line(d));
            out.push('\n');
        }
        let total = self.totals();
        out.push_str(&summary_line(&total));
        out.push('\n');
        if self.only.contains(&Invariant::WidthMonotonicity) {
            out.push_str(&monotonicity_cost(&total));
            out.push('\n');
        }
        out.push_str(&self.suppression_summary());
        out
    }

    /// Builds the suppression summary for this run.
    ///
    /// It lists each exemption, its absorbed count, and its verdict. It prints even
    /// when the list is empty because that state also needs review.
    fn suppression_summary(&self) -> String {
        let mut out = String::new();
        let (mut absorbed, mut unused, mut unknown, mut unchecked) = (0u64, 0u64, 0u64, 0u64);
        for (signature, e) in &self.suppress.entries {
            absorbed += e.absorbed;
            let verdict = if e.invariant.is_none() {
                unknown += 1;
                "UNKNOWN"
            } else if e.invariant.is_some_and(|i| !self.only.contains(&i)) {
                // This run did not check the invariant. The entry had nothing to absorb.
                // Do not call it `UNUSED`. That verdict describes the filter, not the
                // exemption.
                unchecked += 1;
                "unchecked"
            } else if e.absorbed == 0 && !self.partial {
                unused += 1;
                "UNUSED"
            } else {
                "ok"
            };
            out.push_str(&format!(
                "sweep  suppression  absorbed={:<8} {verdict:<9} {signature}  # {}\n",
                e.absorbed, e.reason,
            ));
        }
        let entries = self.suppress.entries.len();
        out.push_str(&format!(
            "sweep  suppressions  entries={entries}  absorbed={absorbed}  \
             unused={unused}  unchecked={unchecked}  unknown={unknown}",
        ));
        if self.only.len() < Invariant::ALL.len() {
            let names: Vec<&str> = self.only.iter().map(|i| i.as_str()).collect();
            out.push_str(&format!("\nsweep  invariants  this run checked {}", names.join(", ")));
        }
        if self.partial {
            out.push_str(
                "\nsweep  suppressions  rows went unread, so no entry is called unused",
            );
        }
        out
    }

    /// Writes one JSON file per candidate into `dir`, writes this run's manifest, and
    /// returns the candidate files.
    ///
    /// It first removes stale candidate files that this sweep could have written. After
    /// the run, disk contains exactly this run's candidates. A shape that no longer
    /// appears no longer needs review. The sweep removes only its own files.
    /// `CHIBIPOP_SWEEP_OUT` can name any directory, so deleting another JSON file would
    /// corrupt the report.
    ///
    /// The manifest gives an empty directory one meaning. It records whether a clean
    /// corpus or a sweep that did not run left the directory empty. It also records
    /// coverage, so the finish-line check can reject a capped or narrowed run as incomplete.
    fn write(&self, dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        std::fs::create_dir_all(dir)?;
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if is_candidate_file(&path) {
                std::fs::remove_file(path)?;
            }
        }
        let mut written = Vec::with_capacity(self.candidates.len());
        for (key, c) in &self.candidates {
            let path = dir.join(candidate_file(c, key));
            std::fs::write(&path, candidate_json(c, key))?;
            written.push(path);
        }
        std::fs::write(dir.join(RUN_MANIFEST), self.manifest_json())?;
        Ok(written)
    }

    /// The manifest for this run: its coverage and results.
    ///
    /// `rows_unread` and `invariants` remain separate from `whole_corpus`. A partial run
    /// then shows which condition made it partial.
    fn manifest_json(&self) -> String {
        let t = self.totals();
        let only: Vec<&str> = self.only.iter().map(|i| i.as_str()).collect();
        let all = Invariant::ALL.len();
        let json = serde_json::json!({
            "invariants": only,
            "invariants_available": all,
            "rows_unread": self.partial,
            "whole_corpus": !self.partial && self.only.len() == all,
            "dictionaries": self.dicts.len(),
            "entries": t.entries,
            "violations": t.violations,
            "suppressed": t.suppressed,
            "candidates": t.candidates,
            "errors": t.errors,
        });
        format!("{}\n", serde_json::to_string_pretty(&json).expect("a manifest serialises"))
    }
}

/// One summary row for a dictionary or the total.
fn summary_line(d: &DictSummary) -> String {
    format!(
        "sweep  {:<40} entries={:<8} strings={:<9} fragments={:<8} markers={:<8} \
         boxes={:<9} pairs={:<9} widths={:<8} declined={:<7} violations={:<8} \
         suppressed={:<8} candidates={:<5} errors={}",
        d.dictionary,
        d.entries,
        d.strings,
        d.fragments,
        d.markers,
        d.boxes,
        d.pairs,
        d.widths,
        d.declined,
        d.violations,
        d.suppressed,
        d.candidates,
        d.errors,
    )
}

/// The cost of the second layout for *width monotonicity*, as a share of the run.
///
/// This invariant doubles layout work. The summary must show that cost so a reviewer
/// can decide whether a full-corpus run keeps this invariant enabled.
///
/// Print this line only when the run checked the invariant. A run without a second
/// layout has no cost share.
fn monotonicity_cost(total: &DictSummary) -> String {
    let (wide, elapsed) = (total.wide_cost.as_secs_f64(), total.elapsed.as_secs_f64());
    let share = if elapsed > 0.0 { 100.0 * wide / elapsed } else { 0.0 };
    format!(
        "sweep  width-monotonicity  second layout {wide:.2}s of {elapsed:.2}s \
         ({share:.1}% of the run) over {} entries",
        total.widths,
    )
}

/// The filename of the run manifest.
///
/// This is not a candidate filename. [`is_candidate_file`] leaves it alone, and each
/// rerun replaces it by name.
const RUN_MANIFEST: &str = "run.json";

/// A candidate's filename: readable prefix, stable suffix.
fn candidate_file(c: &Candidate, key: &str) -> String {
    format!(
        "{}-{}-{:016x}.json",
        c.signature.invariant.as_str(),
        slug(&c.signature.dictionary),
        digest(key),
    )
}

/// Returns true when this sweep could have written `path`.
///
/// The invariant name at the start of a candidate filename lets the sweep remove stale
/// files in a directory that it does not own.
fn is_candidate_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return false };
    name.ends_with(".json")
        && Invariant::ALL.iter().any(|i| name.starts_with(&format!("{}-", i.as_str())))
}

/// One candidate as JSON, pretty-printed for a reviewer's diff.
fn candidate_json(c: &Candidate, key: &str) -> String {
    let json = serde_json::json!({
        "invariant": c.signature.invariant.as_str(),
        "dictionary": c.signature.dictionary,
        "signature": key,
        "shape": c.signature.shape,
        "occurrences": c.occurrences,
        "exemplar": {
            "row": c.exemplar.row,
            "term": c.exemplar.term,
            "reading": c.exemplar.reading,
            "measured": c.exemplar.measured,
            "glossary": c.exemplar.glossary,
        },
    });
    let mut text = serde_json::to_string_pretty(&json).expect("a candidate is plain data");
    text.push('\n');
    text
}

/// A dictionary title as a filename part.
fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len().min(40));
    let mut dash = false;
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
        if out.len() >= 40 {
            break;
        }
    }
    let trimmed = out.trim_end_matches('-');
    if trimmed.is_empty() {
        "dict".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Applies FNV-1a to canonical signature text.
///
/// This stable shape name keeps the same candidate filename across runs. A review diff
/// then shows count changes instead of one deleted file and one new file.
fn digest(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ---- the sweep itself ----

/// One term-bank row that the sweep reads.
///
/// These six values travel through two functions. Their order changed once, and a
/// transposed argument then assigned a candidate to the wrong entry.
///
/// `dict` and `row` use sweep numbering. The sweep reads archives, not the built store,
/// so neither value is a `chibipop.sqlite` identifier. A candidate also stores the
/// headword.
struct Row {
    dict: String,
    dict_id: i64,
    /// One-based, in archive order.
    row: i64,
    term: String,
    reading: String,
    /// The row's glossary, serialized exactly as the builder stores it.
    glossary: String,
}

/// One corpus row as a complete popup with one card, block, and entry.
///
/// The panel has no side column. A sweep checks one entry's render. A second dictionary
/// would make each shape signature depend on unrelated scene content.
fn sweep_card(r: &Row, doc: &Arc<GlossDoc>) -> Presentation {
    let card = Card {
        written: Some(r.term.clone()),
        reading: (!r.reading.is_empty() && r.reading != r.term).then(|| r.reading.clone()),
        pos: Vec::new(),
        freq: None,
        blocks: vec![GlossBlock {
            dict_name: r.dict.clone(),
            dict_id: r.dict_id,
            entries: vec![GlossEntry {
                // The row ordinal replaces the store identifier. No database is involved,
                // and each sweep scene has one entry, so the scene needs no distinction.
                entry_id: r.row,
                glosses: plain_items(doc),
                tags: Vec::new(),
                doc: Arc::clone(doc),
                media: Vec::new(),
            }],
        }],
        match_len: r.term.chars().count(),
        pitch: Vec::new(),
    };
    Presentation {
        top: Some(card.clone()),
        collapsed: Vec::new(),
        all_cards: vec![card],
        sentence: None,
    }
}

/// Sends one term-bank row through the renderer and checks every invariant in `only`.
///
/// `None` means that the row renders no text. The dictionary builder creates no `entry`
/// record for such a row, so no reader can hover it. The sweep excludes it.
///
/// The two calls around the scene match the hover path: parse stored text, apply the
/// dictionary stylesheet, then render. A candidate therefore represents a defect that a
/// reader can see.
fn sweep_entry(r: &Row, sheet: &Sheet, only: &[Invariant]) -> Option<Checked> {
    let mut doc = GlossDoc::parse(&r.glossary);
    if !renders_text(&doc) {
        return None;
    }
    sheet::apply(&mut doc, sheet);
    let doc = Arc::new(doc);
    let p = sweep_card(r, &doc);
    let s = sweep_scene(&p, SWEEP_W);
    // The second layout is the cost of *width monotonicity*. Create it only when the
    // invariant is checked. Measure its time because the invariant doubles layout work.
    let mut spent = Duration::ZERO;
    let wide = only.contains(&Invariant::WidthMonotonicity).then(|| {
        let at = Instant::now();
        let wide = sweep_scene(&p, SWEEP_WIDE_W);
        spent = at.elapsed();
        wide
    });
    // Each checker appears in this match. A new invariant in [`Invariant`] causes a
    // compile error until the caller connects it to a check.
    let mut found = Checked::default();
    let t = Thresholds::DEFAULT;
    for invariant in only {
        found.merge(match invariant {
            Invariant::DroppedText => dropped_text(&r.dict, &doc, SWEEP_ROLES, &s),
            Invariant::OrphanFragment => orphan_fragment(&r.dict, &doc, SWEEP_ROLES, &s, t),
            Invariant::MarkerGap => marker_gap(&r.dict, &doc, &s, t),
            Invariant::HorizontalOverflow => horizontal_overflow(&r.dict, &doc, &s, t),
            Invariant::OverlappingBoxes => overlapping_boxes(&r.dict, &doc, &s, t),
            Invariant::WidthMonotonicity => {
                let wide = wide.as_ref().expect("a checked invariant has its second layout");
                width_monotonicity(&r.dict, &doc, &s, wide, t)
            }
        });
    }
    found.wide_cost = spent;
    Some(found)
}

/// One popup in a panel with maximum width `max_w`, as the hover path requests.
///
/// *Width monotonicity* needs two scenes that differ only in width. This helper keeps
/// their render settings identical. A hand-written second call could change another
/// setting and create a false defect.
fn sweep_scene(p: &Presentation, max_w: f32) -> PopupScene {
    let theme = Theme::dark();
    let mut m = FakeMeasure::default();
    scene(
        &SceneRequest {
            presentation: p,
            theme: &theme,
            max_w,
            max_h: SWEEP_H,
            show_back: false,
            side_panel: false,
            render: sweep_settings(),
            anki: None,
        },
        &mut m,
    )
    .expect("FakeMeasure never refuses a run")
}

/// The row cap represented as an archive-walk error.
///
/// `for_each_term` sends one row to the callback and stops on the first `Err`. The cap
/// uses that error to stop before the next bank file. This keeps a capped run from
/// reading every row of a 37 MB archive.
#[derive(Debug)]
struct RowCap;

impl std::fmt::Display for RowCap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("row cap reached")
    }
}

impl std::error::Error for RowCap {}

/// Sweeps one archive into `report`.
///
/// The function renders each entry inside [`catch_unwind`](std::panic::catch_unwind).
/// A parser or walk panic becomes an error for this archive, and other archives
/// continue. The default panic hook still prints the panic, so a reviewer sees the
/// count and the backtrace.
///
/// `cap` limits the *rows read*, not the entries rendered. An image-only gaiji row
/// therefore costs the same as any other row under a cap. The cap bounds work, not
/// sample size.
fn sweep_archive(zip: &Path, dict_id: i64, cap: Option<u64>, report: &mut Report) {
    let title = archive_title(zip);
    let css = read_styles_css(zip).ok().flatten().unwrap_or_default();
    let sheet = Sheet::compile(&css);
    // Copy the list once per archive. The walk borrows `report` for each row, and six
    // copied names cost less than repeated access.
    let only = report.only.clone();
    let mut sum = DictSummary { dictionary: title.clone(), ..DictSummary::default() };
    let mut rows: i64 = 0;
    let at = Instant::now();
    let walk = for_each_term(zip, |t| {
        if cap.is_some_and(|c| rows as u64 >= c) {
            return Err(anyhow::Error::new(RowCap));
        }
        rows += 1;
        let r = Row {
            dict: title.clone(),
            dict_id,
            row: rows,
            term: t.term.into_owned(),
            reading: t.reading.into_owned(),
            glossary: t.glossary.to_string(),
        };
        let found = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            sweep_entry(&r, &sheet, &only)
        }));
        let Ok(checked) = found else {
            sum.errors += 1;
            eprintln!("sweep  {title}: row {} ({}) panicked", r.row, r.term);
            return Ok(());
        };
        let Some(checked) = checked else { return Ok(()) };
        sum.entries += 1;
        sum.strings += checked.strings;
        sum.fragments += checked.fragments;
        sum.markers += checked.markers;
        sum.boxes += checked.boxes;
        sum.pairs += checked.pairs;
        sum.widths += checked.widths;
        sum.wide_cost += checked.wide_cost;
        sum.declined += checked.declined;
        for v in checked.violations {
            sum.violations += 1;
            let filed = report.record(v, || Exemplar {
                row: r.row,
                term: r.term.clone(),
                reading: r.reading.clone(),
                measured: BTreeMap::new(),
                glossary: r.glossary.clone(),
            });
            match filed {
                Filed::Suppressed => sum.suppressed += 1,
                Filed::Repeat => {}
                Filed::Fresh => sum.candidates += 1,
            }
        }
        Ok(())
    });
    if let Err(err) = walk {
        if err.chain().any(|e| e.is::<RowCap>()) {
            // The archive has rows that this run did not read.
            report.partial = true;
        } else {
            sum.errors += 1;
            eprintln!("sweep  {title}: {err:#}");
        }
    }
    // A panic or unreadable bank file leaves rows unchecked, like a row cap. Do not mark
    // an exemption as stale when the run did not inspect all rows.
    report.partial |= sum.errors > 0;
    sum.elapsed = at.elapsed();
    report.dicts.push(sum);
}

/// A dictionary's own title, or its filename when the archive has no index.
fn archive_title(zip: &Path) -> String {
    let titled = read_index(zip)
        .ok()
        .and_then(|index| index.get("title")?.as_str().map(str::to_string))
        .filter(|t| !t.is_empty());
    titled.unwrap_or_else(|| match zip.file_stem() {
        Some(stem) => stem.to_string_lossy().into_owned(),
        None => zip.display().to_string(),
    })
}

/// Every term archive in a corpus directory, in stable order.
///
/// An archive needs the terms role to provide glossary content. An archive with only
/// frequency or Pitch data has nothing to render. This function checks the archive's
/// banks, like the library.
fn corpus_archives(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading the corpus at {}: {e}", dir.display()))
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")))
        .filter(|p| supplies_terms(p))
        .collect();
    // `read_dir` order is not defined, and a sweep numbers its dictionaries.
    out.sort();
    out
}

// ---- the environment ----

/// The corpus directory of Yomitan `.zip` archives.
const CORPUS_ENV: &str = "CHIBIPOP_SWEEP_CORPUS";
/// Term-bank rows read per dictionary. An unset value reads every row.
const ROWS_ENV: &str = "CHIBIPOP_SWEEP_ROWS";
/// Directory for candidate files.
const OUT_ENV: &str = "CHIBIPOP_SWEEP_OUT";
/// Invariants to check, separated by commas. An unset value checks every invariant.
const ONLY_ENV: &str = "CHIBIPOP_SWEEP_ONLY";

/// The row cap, or `None` for the entire archive.
///
/// An unset value means no cap. The full run is the primary target. The cap supports
/// short test runs. Forgetting it therefore sweeps all rows instead of a sample.
fn row_cap() -> Option<u64> {
    let raw = std::env::var(ROWS_ENV).ok()?;
    Some(raw.parse().unwrap_or_else(|_| panic!("{ROWS_ENV} must be a row count, got {raw:?}")))
}

/// The invariants that this run checks.
///
/// [`row_cap`] and this filter support short test runs. A narrowed run can answer
/// quickly and report one invariant's count instead of a combined count.
fn invariant_filter() -> Vec<Invariant> {
    match std::env::var(ONLY_ENV) {
        Ok(raw) => named_invariants(&raw),
        Err(_) => Invariant::ALL.to_vec(),
    }
}

/// The invariants named by a comma-separated list.
///
/// An unknown name stops the run, like a malformed row cap. A filter that silently
/// selects nothing could sweep a full corpus and report it as clean.
fn named_invariants(raw: &str) -> Vec<Invariant> {
    let only: Vec<Invariant> = raw
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| {
            Invariant::named(name).unwrap_or_else(|| {
                panic!("{ONLY_ENV} names no invariant this build checks: {name:?}")
            })
        })
        .collect();
    assert!(!only.is_empty(), "{ONLY_ENV} is set and names no invariant");
    only
}

/// The directory for candidate files.
///
/// The default is under `.scratch/`, which this repository does not track. A candidate
/// quotes dictionary content verbatim, so no candidate content can enter the repository.
fn candidate_dir() -> PathBuf {
    match std::env::var_os(OUT_ENV) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".scratch/render-sweep/candidates"),
    }
}

/// The committed suppression list.
///
/// This path is fixed. The environment variables above can move local input and output,
/// but they cannot move the reviewed list. A sweep must use the list that the repository
/// commits.
fn suppression_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/render-sweep/suppressions.toml")
}

/// A scene that loses parsed text is a *No dropped text* violation. The candidate names
/// the text and its shape.
///
/// The test removes a paragraph from a real scene. This models the #18 defect.
/// A hand-built scene would test only this fixture's arithmetic.
#[test]
fn a_scene_that_lost_a_paragraph_is_a_dropped_text_violation() {
    let glossary = sc(concat!(
        r#"{"tag":"div","content":["#,
        r#"{"tag":"div","content":"kept"},"#,
        r#"{"tag":"div","data":{"content":"note"},"content":"vanished"}]}"#
    ));
    let doc = GlossDoc::parse(&glossary);
    let p = card_with(vec![css_tree("Fixture", &glossary, "")]);
    let mut s = shown(&p, sweep_settings());
    assert!(drawn_text(&s).contains("vanished"), "the renderer draws it before the mutation");
    s.elems.retain(|e| !e.text.contains("vanished"));

    let found = dropped_text("Fixture", &doc, SWEEP_ROLES, &s);

    assert_eq!(2, found.strings, "both strings were checked, not just the lost one");
    assert_eq!(1, found.violations.len(), "one lost string, one violation");
    let v = &found.violations[0];
    assert_eq!(Invariant::DroppedText, v.signature.invariant);
    assert_eq!("Fixture", v.signature.dictionary);
    assert_eq!(Some(&"vanished".to_string()), v.measured.get("missing"));
    assert_eq!(
        vec!["content", "div", "div[data-sc-content]", "text"],
        v.signature.shape,
        "the shape names every node from the glossary item down to the string",
    );
}

/// Repeated instances of one shape become one candidate.
///
/// This is the purpose of a signature. Jitendex's footnote defect appeared 9 784 times,
/// but those identical items form one review batch. The first entry remains the exemplar,
/// so later runs change only the count in the candidate file.
#[test]
fn repeats_of_one_shape_collapse_into_one_counted_candidate() {
    let glossary = sc(r#"{"tag":"div","data":{"content":"note"},"content":"vanished"}"#);
    let doc = GlossDoc::parse(&glossary);
    let p = card_with(vec![css_tree("Fixture", &glossary, "")]);
    let mut s = shown(&p, sweep_settings());
    s.elems.retain(|e| !e.text.contains("vanished"));
    let found = dropped_text("Fixture", &doc, SWEEP_ROLES, &s);
    assert_eq!(1, found.violations.len(), "one lost string");

    let mut report = Report::default();
    for (row, term) in [(7, "\u{4e00}"), (8, "\u{4e8c}"), (9, "\u{4e09}")] {
        let filed = report.record(found.violations[0].clone(), || Exemplar {
            row,
            term: term.to_string(),
            reading: term.to_string(),
            measured: BTreeMap::new(),
            glossary: glossary.clone(),
        });
        let want = if row == 7 { Filed::Fresh } else { Filed::Repeat };
        assert_eq!(want, filed, "only the first sighting of a shape is a new candidate");
    }

    assert_eq!(1, report.candidates.len(), "three violations, one shape, one candidate");
    let c = report.candidates.values().next().expect("the candidate");
    assert_eq!(3, c.occurrences);
    assert_eq!(7, c.exemplar.row, "the first sighting stays the exemplar");
    assert_eq!("\u{4e00}", c.exemplar.term);
    assert_eq!(Some(&"vanished".to_string()), c.exemplar.measured.get("missing"));

    let key = c.signature.key();
    let json: serde_json::Value =
        serde_json::from_str(&candidate_json(c, &key)).expect("readable JSON");
    assert_eq!(3, json["occurrences"]);
    assert_eq!("dropped-text", json["invariant"]);
    assert_eq!(glossary, json["exemplar"]["glossary"], "the glossary is quoted verbatim");
    assert_eq!("vanished", json["exemplar"]["measured"]["missing"]);
    // The filename gives the shape a stable name. A rerun rewrites the same file instead
    // of leaving a stale file beside a new file.
    assert_eq!(candidate_file(c, &key), candidate_file(c, &key));
    assert!(
        candidate_file(c, &key).starts_with("dropped-text-fixture-"),
        "readable prefix: {}",
        candidate_file(c, &key),
    );
}

/// The boundary: folded dictionary whitespace and a line break do not drop text.
///
/// The input text reaches the panel, but not character-for-character. `  spaced\n   out  `
/// becomes `spaced out`, and `br` puts `after` in its own paragraph. A verbatim
/// comparison would flag valid output and add noise.
#[test]
fn folded_whitespace_and_a_line_break_drop_no_text() {
    let glossary = sc(concat!(
        r#"{"tag":"div","content":["#,
        r#"{"tag":"span","content":"  spaced\n   out  "},"#,
        r#"{"tag":"br"},"#,
        r#"{"tag":"span","content":"after"}]}"#
    ));
    let doc = GlossDoc::parse(&glossary);
    let p = card_with(vec![css_tree("Fixture", &glossary, "")]);
    let s = shown(&p, sweep_settings());

    let found = dropped_text("Fixture", &doc, SWEEP_ROLES, &s);

    assert_eq!(2, found.strings, "both text nodes were checked; the `br` holds none");
    assert_eq!(Vec::<Violation>::new(), found.violations);
}

/// A fixture glossary and its dictionary CSS use the same two calls as [`sweep_entry`].
///
/// The function returns the parsed tree and scene because paragraph-family invariants
/// inspect both. The stylesheet and tree match the renderer's inputs, so the reference
/// stays aligned with the judged render.
fn swept(glossary: &str, css: &str) -> (Arc<GlossDoc>, PopupScene) {
    swept_media(glossary, css, Vec::new())
}

/// [`swept`] for a dictionary build that records assets.
///
/// The size pass reads recorded asset dimensions, not asset bytes. An image fixture
/// therefore needs a path and four numbers. This helper keeps media data out of callers
/// that do not inspect assets.
fn swept_media(
    glossary: &str,
    css: &str,
    media: Vec<(String, Intrinsic)>,
) -> (Arc<GlossDoc>, PopupScene) {
    let (doc, p) = sweep_fixture(glossary, css, media);
    let s = shown(&p, sweep_settings());
    (doc, s)
}

/// One fixture glossary at the two widths used by *width monotonicity*.
///
/// The pair is required because the invariant compares two scenes. A hand-built wide
/// scene would compare the renderer with test arithmetic.
fn swept_pair(glossary: &str, css: &str) -> (Arc<GlossDoc>, PopupScene, PopupScene) {
    let (doc, p) = sweep_fixture(glossary, css, Vec::new());
    (doc, sweep_scene(&p, SWEEP_W), sweep_scene(&p, SWEEP_WIDE_W))
}

/// Builds one fixture glossary as a parsed tree and popup presentation.
///
/// The parsed tree and presentation support the helpers above. Tests can then inspect
/// one panel, two panels, or a chosen width.
fn sweep_fixture(
    glossary: &str,
    css: &str,
    media: Vec<(String, Intrinsic)>,
) -> (Arc<GlossDoc>, Presentation) {
    let sheet = Sheet::compile(css);
    let mut doc = GlossDoc::parse(glossary);
    sheet::apply(&mut doc, &sheet);
    let doc = Arc::new(doc);
    let p = card_with(vec![GlossBlock {
        dict_name: "Fixture".to_string(),
        dict_id: crate::present::NO_ROW,
        entries: vec![GlossEntry {
            entry_id: crate::present::NO_ROW,
            glosses: plain_items(&doc),
            tags: Vec::new(),
            doc: Arc::clone(&doc),
            media,
        }],
    }]);
    (doc, p)
}

/// The #18 shape, copied verbatim from the corpus: Jitendex's example translation and
/// trailing footnote mark.
const FOOTNOTE_TREE: &str = concat!(
    r##"{"tag":"div","data":{"content":"example-sentence-b"},"content":["##,
    r##"{"tag":"span","lang":"en","content":"He still holds the heavyweight title."},"##,
    r##"{"tag":"span","data":{"content":"attribution-footnote"},"content":"[1]"}]}"##,
);

/// The sentence the mark trails, as the panel draws it.
const FOOTNOTE_SENTENCE: &str = "He still holds the heavyweight title.";

/// A fragment on its own paragraph is a violation. The candidate names the fragment and
/// its source prose.
///
/// The test starts with a real scene and removes the paragraph split that the #18 defect
/// caused. A hand-built scene would test only fixture arithmetic.
#[test]
fn a_footnote_split_from_its_sentence_is_an_orphan_fragment() {
    let glossary = sc(FOOTNOTE_TREE);
    let (doc, mut s) = swept(&glossary, "");
    let at = s.elems.iter().position(|e| e.text.contains("[1]")).expect("the sentence");
    assert_eq!(
        format!("{FOOTNOTE_SENTENCE}[1]"),
        s.elems[at].text,
        "the fix draws sentence and mark as one paragraph",
    );
    let mut fragment = s.elems[at].clone();
    fragment.text = "[1]".to_string();
    s.elems[at].text = FOOTNOTE_SENTENCE.to_string();
    s.elems.insert(at + 1, fragment);

    let found = orphan_fragment("Fixture", &doc, SWEEP_ROLES, &s, Thresholds::DEFAULT);

    assert_eq!(1, found.fragments, "one marked node trails prose in this tree");
    assert_eq!(1, found.violations.len(), "one orphan, one violation");
    let v = &found.violations[0];
    assert_eq!(Invariant::OrphanFragment, v.signature.invariant);
    assert_eq!("Fixture", v.signature.dictionary);
    assert_eq!(Some(&"[1]".to_string()), v.measured.get("fragment"));
    assert_eq!(
        Some(&"0".to_string()),
        v.measured.get("company_chars"),
        "the whole defect in one number: nothing else was drawn in the mark's paragraph",
    );
    assert_eq!(
        vec!["content", "div[data-sc-content]", "span[data-sc-content]"],
        v.signature.shape,
        "the shape names every node from the glossary item down to the fragment",
    );
}

/// The resolved #18 shape stays quiet, then flags when the threshold exceeds the fix.
///
/// The first check confirms correct output. The second raises
/// [`ORPHAN_MIN_COMPANY_CHARS`] above the sentence length. The checker then reports the
/// mark's actual company, which proves that the same check sees the corpus shape.
#[test]
fn the_fixed_footnote_re_flags_when_its_company_threshold_is_tightened() {
    let glossary = sc(FOOTNOTE_TREE);
    let (doc, s) = swept(&glossary, "");

    let found = orphan_fragment("Fixture", &doc, SWEEP_ROLES, &s, Thresholds::DEFAULT);
    assert_eq!(1, found.fragments, "the mark was checked, not skipped");
    assert_eq!(Vec::<Violation>::new(), found.violations, "and it sits in its sentence");

    let strict = Thresholds { orphan_min_company: 64, ..Thresholds::DEFAULT };
    let found = orphan_fragment("Fixture", &doc, SWEEP_ROLES, &s, strict);

    assert_eq!(1, found.violations.len(), "tightened past the fix, the same mark is flagged");
    assert_eq!(
        Some(&FOOTNOTE_SENTENCE.chars().count().to_string()),
        found.violations[0].measured.get("company_chars"),
        "and the number it reports is the sentence the fix keeps beside it",
    );
}

/// Boundary case: adjacent marked labels separate senses, so neither is an orphan.
///
/// Jitendex's sense head has a part-of-speech pill and a forms-restriction span. Both
/// nodes are marked, but neither trails a sentence. Prose after a marked node does not
/// support it. A symmetric parent check would flag every sense head.
#[test]
fn marked_labels_that_trail_no_prose_are_no_orphans() {
    let glossary = sc(concat!(
        r#"{"tag":"div","data":{"content":"sense"},"content":["#,
        r#"{"tag":"span","data":{"content":"part-of-speech-info"},"content":"adj"},"#,
        r#"{"tag":"span","data":{"content":"forms-label"},"content":"\u773c only"}]}"#
    ));
    let (doc, s) = swept(&glossary, "");
    assert_eq!(
        vec!["adj", "\u{773c} only"],
        s.elems.iter().filter(|e| e.origin.is_some()).map(|e| e.text.as_str()).collect::<Vec<_>>(),
        "the walk does break between them, as a sense separation",
    );

    let found = orphan_fragment("Fixture", &doc, SWEEP_ROLES, &s, Thresholds::DEFAULT);

    assert_eq!(0, found.fragments, "neither label trails prose, so neither is a fragment");
    assert_eq!(Vec::<Violation>::new(), found.violations);
}

/// Checks both orphan thresholds at their exact boundaries.
///
/// A boundary test protects each comparison. A change from `<=` to `<` would omit the
/// widest allowed fragment. A change from `>=` to `>` would flag the fragment with least
/// company. The test checks a fragment of exactly [`ORPHAN_MAX_CHARS`] characters with
/// exactly [`ORPHAN_MIN_COMPANY_CHARS`] character of company. It also checks a fragment
/// one character longer, which is not a fragment.
#[test]
fn a_fragment_at_the_cap_with_one_character_of_company_passes() {
    let tree = |mark: &str| {
        sc(&format!(
            concat!(
                r#"{{"tag":"div","content":["#,
                r#"{{"tag":"span","content":"x"}},"#,
                r#"{{"tag":"span","data":{{"content":"note"}},"content":"{mark}"}}]}}"#
            ),
            mark = mark,
        ))
    };
    let widest = "[1234567890]";
    assert_eq!(ORPHAN_MAX_CHARS, widest.chars().count(), "the cap, exactly");

    let (doc, s) = swept(&tree(widest), "");
    assert_eq!(
        format!("x{widest}"),
        s.elems.iter().find(|e| e.origin.is_some()).expect("the paragraph").text,
        "one paragraph: the mark trails prose, so the walk keeps them together",
    );
    let found = orphan_fragment("Fixture", &doc, SWEEP_ROLES, &s, Thresholds::DEFAULT);
    assert_eq!(1, found.fragments, "a fragment at the cap is still a fragment");
    assert_eq!(
        Vec::<Violation>::new(),
        found.violations,
        "and one character of company is company enough",
    );

    let (doc, s) = swept(&tree("[12345678901]"), "");
    let found = orphan_fragment("Fixture", &doc, SWEEP_ROLES, &s, Thresholds::DEFAULT);
    assert_eq!(0, found.fragments, "one character past the cap is a phrase, not a fragment");
}

/// The #19 shape, copied verbatim: Jitendex's sense list around its glossary list that
/// declares its own indent.
const SENSE_TREE: &str = concat!(
    r#"{"tag":"ul","data":{"content":"sense-groups"},"content":["#,
    r#"{"tag":"li","data":{"content":"sense-group"},"content":["#,
    r#"{"tag":"ol","content":["#,
    r#"{"tag":"li","data":{"content":"sense"},"#,
    r#""style":{"listStyleType":"\"\u2460\""},"content":["#,
    r#"{"tag":"ul","data":{"content":"glossary"},"#,
    r#""content":[{"tag":"li","content":"to eat"}]}]}]}]}]}"#,
);

/// The dictionary's own `styles.css` file, copied verbatim.
const SENSE_CSS: &str = "ul[data-sc-content=\"sense-groups\"] { list-style-type: \"\u{ff0a}\" }
     li[data-sc-content=\"sense-group\"] { padding-left: 0.25em }
     li[data-sc-content=\"sense\"] {
         padding-left: 0.25em;
         & ul[data-sc-content=\"glossary\"] {
             list-style-type: none;
             padding-left: 0.25em;
         }
     }";

/// A marker one default level from its gloss is a violation. The fixed shape is not.
///
/// The test recreates #19 with the real tree and CSS. The glossary list adds one default
/// [`LIST_INDENT_EM`] level plus its declared padding, so the marker sits 1.9em away
/// instead of the requested 0.5em. The test moves the placed marker left by one level
/// and restores the pre-fix geometry. It uses the real scene that a reader of あくどい saw.
#[test]
fn a_marker_a_default_level_from_its_gloss_is_a_marker_gap() {
    let (doc, mut s) = swept(&sc(SENSE_TREE), SENSE_CSS);
    let at = s.elems.iter().position(|e| e.text == "to eat").expect("the gloss");
    assert_eq!(2, s.elems[at].marker.len(), "the outer \u{ff0a} and the sense's \u{2460}");

    let found = marker_gap("Fixture", &doc, &s, Thresholds::DEFAULT);
    assert_eq!(1, found.markers, "one marked paragraph was checked");
    assert_eq!(
        Vec::<Violation>::new(),
        found.violations,
        "the fixed shape leaves exactly the 0.5em its dictionary declared",
    );

    let last = s.elems[at].marker.len() - 1;
    s.elems[at].marker[last].x -= LEVEL;
    let found = marker_gap("Fixture", &doc, &s, Thresholds::DEFAULT);

    assert_eq!(1, found.violations.len(), "a level of surplus gutter, one violation");
    let v = &found.violations[0];
    assert_eq!(Invariant::MarkerGap, v.signature.invariant);
    assert_eq!(
        Some(&format!("{:.2}", 0.5 * BOX_EM + LEVEL)),
        v.measured.get("gap_px"),
        "the pre-fix number: the declared 0.5em plus a whole level",
    );
    assert_eq!(
        Some(&"0.500".to_string()),
        v.measured.get("owed_em"),
        "against the 0.5em the two declarations below the \u{2460} ask for",
    );
    assert_eq!(
        vec![
            "content",
            "ul[data-sc-content]{list-style-type}",
            "li[data-sc-content]{padding-left}",
            "ol",
            "li[data-sc-content]{list-style-type,padding-left}",
            "ul[data-sc-content]{list-style-type,padding-left}",
            "li",
        ],
        v.signature.shape,
        "the shape names the whole list nest the gap was measured inside",
    );
}

/// Boundary case: a list without padding keeps its default gutter. A gap of exactly one
/// level uses no tolerance.
///
/// This is the counterpart to #19. The glossary list declares no padding, so the default
/// level remains. The marker from the outer list sits 1.4em from the gloss and matches
/// browser output and Yomitan's `--list-padding1`. Zero slack isolates the invariant
/// arithmetic.
#[test]
fn a_default_gutter_under_a_suppressed_marker_is_no_marker_gap() {
    let glossary = sc(concat!(
        r#"{"tag":"ol","content":[{"tag":"li","content":["#,
        r#"{"tag":"ul","style":{"listStyleType":"\"\""},"#,
        r#""content":[{"tag":"li","content":"gloss"}]}]}]}"#
    ));
    let (doc, s) = swept(&glossary, "");
    let item = s.elems.iter().find(|e| e.text == "gloss").expect("the gloss");
    assert_eq!(1, item.marker.len(), "the inner list draws none, so only `1. ` hangs here");
    assert_eq!(
        LEVEL,
        -item.marker[0].x - marker_w("1. "),
        "and it stands one default level from the gloss",
    );

    let exact = Thresholds { marker_gap_slack_em: 0.0, ..Thresholds::DEFAULT };
    let found = marker_gap("Fixture", &doc, &s, exact);

    assert_eq!(1, found.markers, "the marker was checked, not skipped");
    assert_eq!(Vec::<Violation>::new(), found.violations);
}

/// The resolved #19 shape stays quiet, then flags when slack drops below its padding.
///
/// The fixed gap is the declared 0.5em. A checker that flags it would reject browser
/// output. This test lowers the tolerance until the checker reports that same gap. The
/// pre-fix shape had 1.9em, which this check would have reported.
#[test]
fn the_fixed_gutter_re_flags_when_its_slack_is_tightened_past_the_declaration() {
    let (doc, s) = swept(&sc(SENSE_TREE), SENSE_CSS);

    let strict = Thresholds { marker_gap_slack_em: -0.25, ..Thresholds::DEFAULT };
    let found = marker_gap("Fixture", &doc, &s, strict);

    assert_eq!(1, found.violations.len(), "tightened past the fix, the same gap is flagged");
    assert_eq!(
        Some(&format!("{:.2}", 0.5 * BOX_EM)),
        found.violations[0].measured.get("gap_px"),
        "and the number it reports is the pinned gap",
    );
}

/// Paragraph-family violations collapse into counted candidates.
///
/// One shape produces one candidate for each invariant. The candidate filename starts
/// with the invariant name, so [`is_candidate_file`] can remove stale files.
/// [`Invariant::ALL`] must include every invariant name or stale files remain. The test
/// checks both shapes.
#[test]
fn paragraph_family_violations_collapse_into_counted_candidates() {
    let glossary = sc(FOOTNOTE_TREE);
    let (doc, mut s) = swept(&glossary, "");
    let at = s.elems.iter().position(|e| e.text.contains("[1]")).expect("the sentence");
    let mut fragment = s.elems[at].clone();
    fragment.text = "[1]".to_string();
    s.elems[at].text = FOOTNOTE_SENTENCE.to_string();
    s.elems.insert(at + 1, fragment);
    let found = orphan_fragment("Fixture", &doc, SWEEP_ROLES, &s, Thresholds::DEFAULT);
    assert_eq!(1, found.violations.len(), "one orphan to file");

    let mut report = Report::default();
    for row in [11, 12] {
        report.record(found.violations[0].clone(), || Exemplar {
            row,
            term: "\u{4e00}".to_string(),
            reading: "\u{4e00}".to_string(),
            measured: BTreeMap::new(),
            glossary: glossary.clone(),
        });
    }

    assert_eq!(1, report.candidates.len(), "two entries, one shape, one candidate");
    let c = report.candidates.values().next().expect("the candidate");
    assert_eq!(2, c.occurrences);
    assert_eq!(11, c.exemplar.row, "the first sighting stays the exemplar");
    let key = c.signature.key();
    let json: serde_json::Value =
        serde_json::from_str(&candidate_json(c, &key)).expect("readable JSON");
    assert_eq!("orphan-fragment", json["invariant"]);
    assert_eq!("[1]", json["exemplar"]["measured"]["fragment"]);
    let file = candidate_file(c, &key);
    assert!(file.starts_with("orphan-fragment-fixture-"), "readable prefix: {file}");
    assert!(
        is_candidate_file(Path::new(&file)),
        "and a name this sweep can recognise as its own: {file}",
    );
}

/// An indent that no shaper can break: one span with `padding-left` wider than the panel.
///
/// `PILL_SPACER` uses U+00A0 with UAX #14 class GL. The reservation is one unbreakable
/// chunk. A measurer that cannot fit it on a line overflows and avoids a loop.
/// This models the *unbreakable line* failure with dictionary CSS.
const WIDE_PAD_TREE: &str =
    r##"{"tag":"span","data":{"content":"wide"},"content":"x"}"##;

/// The declaration: 40em of left padding in a 424-pixel panel.
const WIDE_PAD_CSS: &str = "span[data-sc-content=\"wide\"] { padding-left: 40em }";

/// A run wider than the panel is a violation. The candidate names the edge and overhang.
///
/// The test uses a real padding declaration and an unbreakable chunk. The 487.5 pixels
/// of ink extend 75.5 pixels past a 424-pixel panel. The test records that overhang.
#[test]
fn an_unbreakable_line_wider_than_the_panel_is_a_horizontal_overflow() {
    let (doc, s) = swept(&sc(WIDE_PAD_TREE), WIDE_PAD_CSS);

    let found = horizontal_overflow("Fixture", &doc, &s, Thresholds::DEFAULT);

    assert_eq!(3, found.boxes, "the headword, the dictionary label, and the padded run");
    assert_eq!(1, found.violations.len(), "one box leaves the panel");
    let v = &found.violations[0];
    assert_eq!(Invariant::HorizontalOverflow, v.signature.invariant);
    assert_eq!(Some(&"right".to_string()), v.measured.get("side"));
    assert_eq!(Some(&"<element-box>".to_string()), v.measured.get("box"));
    assert_eq!(Some(&"424.00".to_string()), v.measured.get("panel_w_px"));
    assert_eq!(
        Some(&"75.50".to_string()),
        v.measured.get("over_px"),
        "the whole defect in one number: 487.5 of ink from x=12 on a 424 panel",
    );
    assert_eq!(
        vec!["content", "span[data-sc-content]{padding-left}", "<element-box>"],
        v.signature.shape,
        "the shape names the node that declared the indent, and which of its boxes left",
    );
}

/// Boundary case: a box with its final pixel at the panel edge is inside. A box one
/// pixel wider is outside.
///
/// The test repeats one scene because the invariant uses `>`. A change to `>=` would
/// reject the widest valid box. Zero slack isolates the invariant from
/// [`OVERFLOW_SLACK_PX`].
#[test]
fn a_box_ending_exactly_at_the_panel_edge_is_no_horizontal_overflow() {
    let (doc, mut s) = swept(&sc(WIDE_PAD_TREE), WIDE_PAD_CSS);
    let tight = Thresholds { overflow_slack_px: 0.0, ..Thresholds::DEFAULT };
    let panel_w = panel_width(&s);
    let at = s.elems.iter().position(|e| e.text.ends_with('x')).expect("the padded run");
    s.elems[at].rect.w = panel_w - s.elems[at].rect.x;

    let flush = horizontal_overflow("Fixture", &doc, &s, tight);
    assert_eq!(3, flush.boxes, "every box is still asked about");
    assert!(flush.violations.is_empty(), "the last pixel of the panel is on the panel");

    s.elems[at].rect.w += 1.0;
    let over = horizontal_overflow("Fixture", &doc, &s, tight);
    assert_eq!(1, over.violations.len(), "and one pixel past it is not");
    assert_eq!(Some(&"1.00".to_string()), over.violations[0].measured.get("over_px"));
}

/// A marker in its gutter is not horizontal overflow, even when it reaches the panel
/// edge.
///
/// Markers sit outside their item boxes by definition ([`MarkerBox`]). The panel edge,
/// not the content edge, bounds this invariant. [`place_markers`] clamps this wide marker
/// to the panel's left edge. Zero slack checks the exact boundary.
#[test]
fn a_marker_clamped_to_the_panel_edge_is_no_horizontal_overflow() {
    let glossary = sc(concat!(
        r##"{"tag":"ul","data":{"content":"wide"},"##,
        r##""content":[{"tag":"li","content":"an item"}]}"##,
    ));
    let css = "ul[data-sc-content=\"wide\"] { list-style-type: \
               \"\u{2460}\u{2461}\u{2462}\u{2463}\u{2464}\u{2465}\" }";
    let (doc, s) = swept(&glossary, css);
    let tight =
        Thresholds { overflow_slack_px: 0.0, overlap_slack_px: 0.0, ..Thresholds::DEFAULT };

    let item = s.elems.iter().find(|e| e.text == "an item").expect("the item");
    let mark = item.marker.last().expect("its marker");
    assert!(mark.x < 0.0, "the marker hangs left of the pen: {}", mark.x);
    assert_eq!(0.0, item.pen.0 + mark.x, "and lands on the panel's own left edge");

    let over = horizontal_overflow("Fixture", &doc, &s, tight);
    assert_eq!(4, over.boxes, "the marker's box is one of the four asked about");
    assert!(over.violations.is_empty(), "a marker in its gutter is where a marker goes");
    let stacked = overlapping_boxes("Fixture", &doc, &s, tight);
    assert!(stacked.violations.is_empty(), "and it is on no other box's pixels");
}

/// The #19-adjacent shape: two blocks that a negative top margin pulls together.
const PULLED_TREE: &str = concat!(
    r##"{"tag":"div","content":[{"tag":"div","content":"first line here"},"##,
    r##"{"tag":"div","data":{"content":"pull"},"content":"second line here"}]}"##,
);

/// The declaration: two ems of negative margin against a four-pixel gap.
const PULLED_CSS: &str = "div[data-sc-content=\"pull\"] { margin-top: -2em }";

/// Two paragraphs on the same pixels are a violation. The candidate names both boxes.
///
/// CSS permits a negative margin, and [`box_len`] retains it. Two ems of margin pull the
/// second block 26 pixels into the first. This is the mis-stacked row that the invariant
/// detects. A reader would see one sentence over another.
///
/// [`box_len`]: super::style::box_len
#[test]
fn two_paragraphs_a_negative_margin_pulled_together_are_overlapping_boxes() {
    let (doc, s) = swept(&sc(PULLED_TREE), PULLED_CSS);

    let found = overlapping_boxes("Fixture", &doc, &s, Thresholds::DEFAULT);

    assert_eq!(1, found.pairs, "one pair of stacked boxes stood near enough to ask about");
    assert_eq!(1, found.violations.len(), "and it is a collision");
    let v = &found.violations[0];
    assert_eq!(Invariant::OverlappingBoxes, v.signature.invariant);
    assert_eq!(Some(&"first line here".to_string()), v.measured.get("upper"));
    assert_eq!(Some(&"second line here".to_string()), v.measured.get("lower"));
    assert_eq!(
        Some(&"26.00".to_string()),
        v.measured.get("overlap_h_px"),
        "the whole defect in one number: two ems of pull less the gap it ate",
    );
    assert_eq!(
        vec![
            "content",
            "div",
            "div",
            "<overlaps>",
            "content",
            "div",
            "div[data-sc-content]{margin-top}",
        ],
        v.signature.shape,
        "the shape names both nodes, upper first",
    );
}

/// Boundary case: adjacent paragraph edges form a stack, not a collision.
///
/// The test checks the `<=` boundary at zero slack. If the operator changes to `<`, the
/// check would flag every edge-to-edge paragraph in the corpus because that is normal
/// stacking.
#[test]
fn two_paragraphs_meeting_edge_to_edge_are_no_overlapping_boxes() {
    let (doc, mut s) = swept(&sc(PULLED_TREE), PULLED_CSS);
    let tight = Thresholds { overlap_slack_px: 0.0, ..Thresholds::DEFAULT };
    let first = s.elems.iter().position(|e| e.text == "first line here").expect("the first");
    let second = s.elems.iter().position(|e| e.text == "second line here").expect("the second");
    let bottom = s.elems[first].rect.y + s.elems[first].rect.h;
    s.elems[second].rect.y = bottom;

    let flush = overlapping_boxes("Fixture", &doc, &s, tight);
    assert!(flush.violations.is_empty(), "one box's bottom edge is the next box's top edge");

    s.elems[second].rect.y = bottom - 1.0;
    let over = overlapping_boxes("Fixture", &doc, &s, tight);
    assert_eq!(1, over.violations.len(), "and one pixel of interpenetration is not");
    assert_eq!(Some(&"1.00".to_string()), over.violations[0].measured.get("overlap_h_px"));
}

/// Boundary case: two cells can share all y pixels without overlap.
///
/// The check must test x as well as y. Table cells start at the row top, so a y-only
/// check would flag every table.
#[test]
fn two_cells_sharing_a_row_are_no_overlapping_boxes() {
    let glossary = sc(concat!(
        r##"{"tag":"table","content":[{"tag":"tr","content":["##,
        r##"{"tag":"td","content":"left cell"},{"tag":"td","content":"right cell"}]}]}"##,
    ));
    let (doc, s) = swept(&glossary, "");
    let tight = Thresholds { overlap_slack_px: 0.0, ..Thresholds::DEFAULT };

    let cells: Vec<&SceneElem> =
        s.elems.iter().filter(|e| e.text.ends_with(" cell")).collect();
    assert_eq!(2, cells.len(), "one paragraph per cell");
    assert_eq!(cells[0].rect.y, cells[1].rect.y, "both start at their row's top");

    let found = overlapping_boxes("Fixture", &doc, &s, tight);
    assert_eq!(1, found.pairs, "the two were compared");
    assert!(found.violations.is_empty(), "and they stand side by side, not on each other");
}

/// A reading above its base is not overlap, even when its box lies inside the paragraph.
///
/// A reading uses space that its base reserves. [`RubyBox`] stays inside the paragraph
/// box, but it is not an element rectangle. The check excludes it, so no ruby appears as
/// a collision.
#[test]
fn a_reading_over_its_base_is_no_overlapping_box() {
    let glossary = sc(concat!(
        r##"{"tag":"ruby","content":[{"tag":"span","content":"\u65e5"},"##,
        r##"{"tag":"rt","content":"\u306b\u307b\u3093\u3054"}]}"##,
    ));
    let (doc, s) = swept(&glossary, "");
    let tight =
        Thresholds { overflow_slack_px: 0.0, overlap_slack_px: 0.0, ..Thresholds::DEFAULT };

    let base = s.elems.iter().find(|e| !e.ruby.is_empty()).expect("the ruby paragraph");
    let read = &base.ruby[0];
    let (x, y) = (base.pen.0 + read.x, base.pen.1 + read.y);
    assert!(
        x >= base.rect.x
            && x + read.w <= base.rect.x + base.rect.w
            && y >= base.rect.y
            && y + read.h <= base.rect.y + base.rect.h,
        "the reading's box lies inside its paragraph's: {:?} in {:?}",
        (x, y, read.w, read.h),
        base.rect,
    );

    let found = overlapping_boxes("Fixture", &doc, &s, tight);
    assert!(found.violations.is_empty(), "a reading is not a box the flow stacked");
    let over = horizontal_overflow("Fixture", &doc, &s, tight);
    assert_eq!(4, over.boxes, "the reading's box is asked about all the same");
    assert!(over.violations.is_empty(), "and it is on the panel");
}

/// Tests two assets on one line and then moves one asset over the other.
///
/// This covers the double-drawn element case of *No overlapping boxes*. An image
/// overlaps its spacer paragraph by design. The spacer reserves line space. A flow box
/// cannot detect two assets at one location, so only another asset can report this defect.
#[test]
fn two_assets_composited_on_one_place_are_overlapping_boxes() {
    let glossary = sc(concat!(
        r##"{"tag":"div","content":[{"tag":"img","path":"g/a.png"},"##,
        r##"{"tag":"img","path":"g/b.png"}]}"##,
    ));
    let art = recorded(MediaFormat::Png, 20.0, 20.0);
    let media = vec![("g/a.png".to_string(), art), ("g/b.png".to_string(), art)];
    let (doc, mut s) = swept_media(&glossary, "", media);
    let tight = Thresholds { overlap_slack_px: 0.0, ..Thresholds::DEFAULT };
    let assets: Vec<usize> = s
        .elems
        .iter()
        .enumerate()
        .filter(|(_, e)| e.kind == ElemKind::Image)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(2, assets.len(), "one element per asset");

    let found = overlapping_boxes("Fixture", &doc, &s, tight);
    assert_eq!(1, found.pairs, "the two assets were compared, and nothing else was");
    assert!(found.violations.is_empty(), "side by side on the line that bought their room");

    s.elems[assets[1]].rect.x = s.elems[assets[0]].rect.x;
    let drawn_twice = overlapping_boxes("Fixture", &doc, &s, tight);
    assert_eq!(1, drawn_twice.violations.len(), "one asset over another is one violation");
    let v = &drawn_twice.violations[0];
    assert_eq!(Some(&"Image".to_string()), v.measured.get("upper_kind"));
    assert_eq!(Some(&"Image".to_string()), v.measured.get("lower_kind"));
    assert_eq!(Some(&"20.00".to_string()), v.measured.get("overlap_w_px"));
}

/// Tests candidate creation for the box family.
///
/// It applies the rule from
/// [`paragraph_family_violations_collapse_into_counted_candidates`]. One shape creates
/// one candidate for each invariant. The filename starts with the invariant name, so
/// [`is_candidate_file`] removes stale files. Both violations enter the report, so the
/// shapes stay separate.
#[test]
fn box_family_violations_collapse_into_counted_candidates() {
    let wide = sc(WIDE_PAD_TREE);
    let (doc, s) = swept(&wide, WIDE_PAD_CSS);
    let overflow = horizontal_overflow("Fixture", &doc, &s, Thresholds::DEFAULT);
    assert_eq!(1, overflow.violations.len(), "one overflowing box to file");
    let pulled = sc(PULLED_TREE);
    let (doc, s) = swept(&pulled, PULLED_CSS);
    let overlap = overlapping_boxes("Fixture", &doc, &s, Thresholds::DEFAULT);
    assert_eq!(1, overlap.violations.len(), "one collision to file");

    let mut report = Report::default();
    let quote = |glossary: &str, row: i64| {
        let glossary = glossary.to_string();
        move || Exemplar {
            row,
            term: "\u{4e00}".to_string(),
            reading: "\u{4e00}".to_string(),
            measured: BTreeMap::new(),
            glossary,
        }
    };
    for row in [7, 8, 9] {
        report.record(overflow.violations[0].clone(), quote(&wide, row));
    }
    report.record(overlap.violations[0].clone(), quote(&pulled, 11));

    assert_eq!(2, report.candidates.len(), "four violations, two shapes, two candidates");
    let filed: Vec<(String, u64, i64, String)> = report
        .candidates
        .iter()
        .map(|(key, c)| {
            let invariant = c.signature.invariant.as_str().to_string();
            (invariant, c.occurrences, c.exemplar.row, candidate_file(c, key))
        })
        .collect();
    assert_eq!(
        vec![
            ("horizontal-overflow".to_string(), 3, 7),
            ("overlapping-boxes".to_string(), 1, 11),
        ],
        filed.iter().map(|(i, n, row, _)| (i.clone(), *n, *row)).collect::<Vec<_>>(),
        "one candidate each, counted, and the first sighting stays the exemplar",
    );
    for (invariant, _, _, file) in &filed {
        assert!(file.starts_with(&format!("{invariant}-fixture-")), "readable prefix: {file}");
        assert!(
            is_candidate_file(Path::new(file)),
            "and a name this sweep can recognise as its own: {file}",
        );
    }

    let (key, c) = report.candidates.iter().next().expect("the overflow candidate");
    let json: serde_json::Value =
        serde_json::from_str(&candidate_json(c, key)).expect("readable JSON");
    assert_eq!("horizontal-overflow", json["invariant"]);
    assert_eq!("75.50", json["exemplar"]["measured"]["over_px"]);
}

/// Defines prose that wraps in the narrow panel and uses fewer lines in the wide panel.
const WRAPPED_TREE: &str = concat!(
    r##"{"tag":"div","data":{"content":"gloss"},"content":""##,
    "He still holds the heavyweight title, and nobody in the division ",
    r##"has come close to taking it from him."}"##,
);

/// Checks that the two sweep widths are the default cap and the settings ceiling.
///
/// The code derives [`SWEEP_WIDE_W`] from the default cap. This test protects that
/// relation. If both widths became equal, the invariant would compare one scene with
/// itself and could report false monotonicity.
#[test]
fn the_swept_widths_run_from_the_default_cap_to_the_ceiling() {
    let default = crate::config::Config::default().popup.max_width_percent;
    assert_eq!(
        f32::from(default),
        SWEEP_W_PERCENT,
        "SWEEP_W stands for the default width cap",
    );
    assert!(default < MAX_WIDTH_RANGE.1, "and the ceiling is wider than the default");
    const { assert!(SWEEP_WIDE_W > SWEEP_W, "so the second layout is a wider panel") };
}

/// A wider panel with taller content is a *Width monotonicity* violation.
///
/// The candidate records both widths, both heights, and the grown shape. The test
/// mutates a real scene pair, like
/// [`a_scene_that_lost_a_paragraph_is_a_dropped_text_violation`]. The renderer keeps
/// monotonicity for this fixture. The added line models a wrap rule that uses more width.
#[test]
fn a_wider_panel_that_drew_taller_content_is_a_width_monotonicity_violation() {
    let glossary = sc(WRAPPED_TREE);
    let (doc, narrow, mut wide) = swept_pair(&glossary, "");
    assert!(
        wide.content_h < narrow.content_h,
        "the wider panel wraps the gloss into fewer lines: {} vs {}",
        wide.content_h,
        narrow.content_h,
    );
    let gloss = |s: &PopupScene| {
        s.elems.iter().position(|e| e.text.contains("heavyweight")).expect("the gloss")
    };
    // Add one paragraph of unwanted height to the paragraph and `content_h`.
    // A bad wrap rule changes both values together.
    let taller = narrow.elems[gloss(&narrow)].rect.h;
    let at = gloss(&wide);
    wide.elems[at].rect.h += taller;
    wide.content_h = narrow.content_h + taller;

    let found = width_monotonicity("Fixture", &doc, &narrow, &wide, Thresholds::DEFAULT);

    assert_eq!(1, found.widths, "one entry, one comparison");
    assert_eq!(1, found.violations.len(), "one entry that grew, one violation");
    let v = &found.violations[0];
    assert_eq!(Invariant::WidthMonotonicity, v.signature.invariant);
    let measured = |key: &str| v.measured.get(key).expect(key).clone();
    assert_eq!(format!("{SWEEP_W:.2}"), measured("narrow_w_px"));
    assert_eq!(format!("{SWEEP_WIDE_W:.2}"), measured("wide_w_px"));
    assert_eq!(format!("{:.2}", narrow.content_h), measured("narrow_h_px"));
    assert_eq!(format!("{:.2}", wide.content_h), measured("wide_h_px"));
    assert_eq!(
        format!("{:.2}", wide.content_h - narrow.content_h),
        measured("taller_px"),
        "both widths and both heights, so an adjudicator needs no second run",
    );
    assert_eq!(
        Some(&"div[data-sc-content]".to_string()),
        v.signature.shape.last(),
        "and the shape names the node whose own boxes grew: {:?}",
        v.signature.shape,
    );
}

/// Tests the exact boundary for *Width monotonicity*.
///
/// Equal content heights are monotone. One extra pixel is not. The test checks one pair
/// three times because if `<=` changes to `<`, every entry whose height cannot shrink
/// would fail. A one-line gloss, headword, or image can have that shape. Zero slack
/// separates the invariant boundary from [`MONOTONIC_SLACK_PX`].
#[test]
fn a_wider_panel_no_taller_than_the_narrow_one_is_no_width_monotonicity() {
    let (doc, narrow, mut wide) = swept_pair(&sc(WRAPPED_TREE), "");
    let tight = Thresholds { monotonic_slack_px: 0.0, ..Thresholds::DEFAULT };

    let shorter = width_monotonicity("Fixture", &doc, &narrow, &wide, tight);
    assert_eq!(1, shorter.widths, "the real pair was compared");
    assert!(shorter.violations.is_empty(), "and a wider panel that wrapped shorter is monotone");

    wide.content_h = narrow.content_h;
    let equal = width_monotonicity("Fixture", &doc, &narrow, &wide, tight);
    assert!(
        equal.violations.is_empty(),
        "equal heights are monotone: the property is not a strict decrease",
    );

    wide.content_h = narrow.content_h + 1.0;
    let over = width_monotonicity("Fixture", &doc, &narrow, &wide, tight);
    assert_eq!(1, over.violations.len(), "one pixel taller is not");
}

/// Checks that only *Width monotonicity* creates the second layout and reports its cost.
///
/// This invariant doubles layout work. A disabled invariant must add no cost. The test
/// checks zero width comparisons and zero second-layout time when it is disabled. It
/// uses a fixture archive instead of the environment, as
/// [`a_narrowed_run_checks_only_the_invariants_it_names`] does.
#[test]
fn the_second_layout_is_paid_for_only_when_width_monotonicity_is_checked() {
    let zip = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yomitan/terms.zip");

    let mut off = Report { only: named_invariants("dropped-text"), ..Report::default() };
    sweep_archive(&zip, 1, None, &mut off);
    let d = &off.dicts[0];
    assert!(d.entries > 0 && d.strings > 0, "the archive was swept");
    assert_eq!(0, d.widths, "and no entry was compared at two widths");
    assert_eq!(Duration::ZERO, d.wide_cost, "so no second panel was ever laid out");
    assert!(
        !off.summary().contains("width-monotonicity"),
        "and a run that checked it not at all claims no share of the time",
    );

    let mut on = Report { only: named_invariants("width-monotonicity"), ..Report::default() };
    sweep_archive(&zip, 1, None, &mut on);
    let w = &on.dicts[0];
    assert_eq!(w.entries, w.widths, "every entry was compared at two widths");
    assert!(w.wide_cost > Duration::ZERO, "and every comparison paid for a second layout");
    assert!(w.wide_cost < w.elapsed, "which is a share of the run and not the whole of it");
    assert_eq!(0, w.strings, "the other five were not asked anything");
    let summary = on.summary();
    let cost = summary
        .lines()
        .find(|l| l.contains("width-monotonicity  second layout"))
        .unwrap_or_else(|| panic!("the run reports its time share:\n{summary}"));
    assert!(cost.contains("% of the run"), "as a share: {cost}");
    assert!(cost.contains(&format!("over {} entries", w.widths)), "over its own entries: {cost}");
}

/// Tests candidate creation for *Width monotonicity*.
///
/// One shape creates one candidate. If the sweep cannot assign a shape, it uses the
/// entry shape instead. The filename starts with the invariant name, so
/// [`is_candidate_file`] can remove stale files.
#[test]
fn width_monotonicity_violations_collapse_into_counted_candidates() {
    let glossary = sc(WRAPPED_TREE);
    let (doc, narrow, mut wide) = swept_pair(&glossary, "");
    let at = wide.elems.iter().position(|e| e.text.contains("heavyweight")).expect("the gloss");
    wide.elems[at].rect.h += 100.0;
    wide.content_h = narrow.content_h + 100.0;
    let attributed = width_monotonicity("Fixture", &doc, &narrow, &wide, Thresholds::DEFAULT);
    assert_eq!(1, attributed.violations.len(), "one entry that grew inside one node");

    // The height rose, but no drawn shape grew. Thus, the entry is the shape.
    // Adjudication concerns the space between the boxes.
    let (doc2, narrow2, mut wide2) = swept_pair(&glossary, "");
    wide2.content_h = narrow2.content_h + 100.0;
    let entry = width_monotonicity("Fixture", &doc2, &narrow2, &wide2, Thresholds::DEFAULT);
    assert_eq!(1, entry.violations.len(), "one entry that grew between its nodes");
    assert_eq!(vec!["<entry>".to_string()], entry.violations[0].signature.shape);

    let mut report = Report::default();
    let quote = |row: i64| {
        let glossary = glossary.clone();
        move || Exemplar {
            row,
            term: "\u{4e00}".to_string(),
            reading: "\u{4e00}".to_string(),
            measured: BTreeMap::new(),
            glossary,
        }
    };
    for row in [3, 4] {
        report.record(attributed.violations[0].clone(), quote(row));
    }
    report.record(entry.violations[0].clone(), quote(9));

    assert_eq!(2, report.candidates.len(), "three violations, two shapes, two candidates");
    for (key, c) in &report.candidates {
        assert_eq!(Invariant::WidthMonotonicity, c.signature.invariant);
        let file = candidate_file(c, key);
        assert!(file.starts_with("width-monotonicity-fixture-"), "readable prefix: {file}");
        assert!(
            is_candidate_file(Path::new(&file)),
            "and a name this sweep can recognise as its own: {file}",
        );
    }
    let counted: Vec<(u64, i64)> =
        report.candidates.values().map(|c| (c.occurrences, c.exemplar.row)).collect();
    assert!(
        counted.contains(&(2, 3)) && counted.contains(&(1, 9)),
        "counted, and the first sighting stays the exemplar: {counted:?}",
    );
    let (key, c) = report.candidates.iter().next().expect("a candidate");
    let json: serde_json::Value =
        serde_json::from_str(&candidate_json(c, key)).expect("readable JSON");
    assert_eq!("width-monotonicity", json["invariant"]);
    assert!(json["exemplar"]["measured"]["wide_h_px"].is_string(), "{json}");
}

/// Checks that a narrowed run checks only the named invariants.
///
/// The test uses a fixture archive, not the process environment, so one test does not
/// affect another. Each name starts its check and excludes other checks. A narrowed
/// candidate count is then accurate. An unknown name stops the run instead of selecting
/// no invariant.
#[test]
fn a_narrowed_run_checks_only_the_invariants_it_names() {
    let zip = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yomitan/terms.zip");

    let only = named_invariants("horizontal-overflow, overlapping-boxes");
    let mut boxes_only = Report { only, ..Report::default() };
    sweep_archive(&zip, 1, None, &mut boxes_only);
    let d = &boxes_only.dicts[0];
    assert!(d.boxes > 0, "the box family ran");
    assert_eq!(
        (0, 0, 0, 0),
        (d.strings, d.fragments, d.markers, d.widths),
        "and the other four were not asked anything",
    );

    let mut whole = Report::default();
    sweep_archive(&zip, 1, None, &mut whole);
    let all = &whole.dicts[0];
    assert!(all.strings > 0, "the default run still checks all six");
    assert_eq!(
        (all.boxes, all.pairs),
        (d.boxes, d.pairs),
        "and the narrowed run made every check the whole one made for these two",
    );

    let refused = std::panic::catch_unwind(|| named_invariants("no-such-invariant"));
    assert!(refused.is_err(), "a name this build cannot check stops the run");
}

/// Runs the local corpus sweep. This test is the primary target of this module.
///
/// ```text
/// CHIBIPOP_SWEEP_CORPUS=~/.local/share/chibipop/library \
///   cargo test -p chibipop --lib corpus_render_sweep -- --ignored --nocapture
/// ```
///
/// Use `--nocapture` to show the summary. Set `CHIBIPOP_SWEEP_ROWS` to limit rows per
/// Dictionary. Set `CHIBIPOP_SWEEP_ONLY` to select render invariants. Set
/// `CHIBIPOP_SWEEP_OUT` to select the candidate directory.
///
/// The test carries `#[ignore]` when no corpus variable is set. A normal `cargo test`
/// then reports the ignored test. It never passes a test that did no work.
///
/// The test loads the committed suppression list before the first archive. A parse error
/// stops the run. This prevents repeated reports for shapes that already have review.
#[test]
#[ignore = "needs a local corpus: set CHIBIPOP_SWEEP_CORPUS and run with --ignored --nocapture"]
fn corpus_render_sweep() {
    let dir = std::env::var_os(CORPUS_ENV)
        .unwrap_or_else(|| panic!("set {CORPUS_ENV} to a directory of Yomitan .zip archives"));
    let dir = PathBuf::from(dir);
    let archives = corpus_archives(&dir);
    assert!(!archives.is_empty(), "no term archives under {}", dir.display());
    let cap = row_cap();
    let suppress = Suppressions::load(&suppression_file())
        .unwrap_or_else(|e| panic!("the suppression list: {e:#}"));

    let mut report =
        Report { suppress, only: invariant_filter(), ..Report::default() };
    for (i, zip) in archives.iter().enumerate() {
        sweep_archive(zip, i as i64 + 1, cap, &mut report);
    }

    println!("{}", report.summary());
    let out = candidate_dir();
    let written = report.write(&out).unwrap_or_else(|e| panic!("writing candidates: {e}"));
    println!("sweep  wrote {} candidate files to {}", written.len(), out.display());
}

/// Proves the sweep machinery with a committed fixture and no local corpus.
///
/// CI sweeps the three-row fixture archive. The test checks:
///
/// - The row cap limits rows read.
/// - An absent cap reads all rows.
/// - Two runs produce the same candidate set.
/// - Each candidate reaches disk as readable JSON with its count and exemplar.
///
/// Zero candidates is valid. The fixture has no known defect. A required candidate would
/// bind the sweep to the fixture's current render.
#[test]
fn a_row_capped_sweep_of_the_fixture_archive_reports_every_entry() {
    let zip = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yomitan/terms.zip");

    let mut capped = Report::default();
    sweep_archive(&zip, 1, Some(2), &mut capped);
    assert_eq!(1, capped.dicts.len(), "one archive, one summary line");
    assert_eq!("FixtureTerms", capped.dicts[0].dictionary);
    assert_eq!(2, capped.dicts[0].entries, "the cap bounds the rows");
    assert_eq!(0, capped.dicts[0].errors, "no fixture row panics");
    assert!(capped.dicts[0].strings >= 2, "each swept entry offered the invariant a string");

    let mut whole = Report::default();
    sweep_archive(&zip, 1, None, &mut whole);
    assert_eq!(3, whole.dicts[0].entries, "unset sweeps the whole archive");
    assert!(
        whole.dicts[0].strings > capped.dicts[0].strings,
        "the third entry brought strings of its own: {} vs {}",
        whole.dicts[0].strings,
        capped.dicts[0].strings,
    );
    assert_eq!(
        whole.dicts[0].candidates as usize,
        whole.candidates.len(),
        "one shape, one candidate",
    );

    let mut again = Report::default();
    sweep_archive(&zip, 1, None, &mut again);
    let shapes = |r: &Report| r.candidates.keys().cloned().collect::<Vec<_>>();
    assert_eq!(shapes(&whole), shapes(&again), "the same archive reports the same shapes");

    let out = std::env::temp_dir().join(format!("chibipop-sweep-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).expect("the candidate directory");
    let neighbour = out.join("someone-elses.json");
    std::fs::write(&neighbour, "{}").expect("a neighbour's file");
    let written = whole.write(&out).expect("writing candidates");
    assert_eq!(whole.candidates.len(), written.len(), "one file per candidate");
    assert!(neighbour.exists(), "a run clears its own candidates and nothing else");
    for path in &written {
        let text = std::fs::read_to_string(path).expect("a candidate file");
        let json: serde_json::Value = serde_json::from_str(&text).expect("readable JSON");
        assert!(json["occurrences"].as_u64().is_some_and(|n| n >= 1), "{path:?}: {text}");
        assert!(json["exemplar"]["glossary"].is_string(), "{path:?}: {text}");
        assert!(json["shape"].as_array().is_some_and(|s| !s.is_empty()), "{path:?}: {text}");
    }
    // The second write leaves only one run's candidates.
    let twice = whole.write(&out).expect("writing candidates again");
    let mine = std::fs::read_dir(&out)
        .expect("the candidate directory")
        .filter_map(std::result::Result::ok)
        .filter(|e| is_candidate_file(&e.path()))
        .count();
    assert_eq!(twice.len(), mine, "a rerun replaces its own output");

    // The manifest distinguishes a clean corpus from a sweep that did not run.
    // It remains after stale-candidate removal and reports the latest run.
    let text = std::fs::read_to_string(out.join(RUN_MANIFEST)).expect("the run manifest");
    let run: serde_json::Value = serde_json::from_str(&text).expect("readable JSON");
    assert_eq!(Some(1), run["dictionaries"].as_u64(), "one archive was swept");
    assert_eq!(Some(3), run["entries"].as_u64(), "every row of the fixture");
    assert_eq!(
        Some(whole.candidates.len() as u64),
        run["candidates"].as_u64(),
        "the manifest agrees with the report it came from",
    );
    assert_eq!(
        Some(true),
        run["whole_corpus"].as_bool(),
        "an uncapped run of every invariant is a whole one",
    );
    std::fs::remove_dir_all(&out).expect("cleaning up");
}

/// The media archive contains ruby, images, SVG, and unreadable assets.
///
/// `terms.zip` has three simple rows. This archive includes each media shape from the
/// census. One row contains only an image, so it renders no text. The test confirms that
/// the sweep accepts shapes that could break a text-only walk. `renders_text` excludes
/// that row from the entry count.
#[test]
fn a_sweep_of_the_media_fixture_renders_every_row_that_holds_text() {
    let zip = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media/media.zip");

    let mut report = Report::default();
    sweep_archive(&zip, 1, None, &mut report);
    let d = &report.dicts[0];

    assert_eq!("FixtureMedia", d.dictionary);
    assert_eq!(0, d.errors, "no media shape panics the renderer");
    assert_eq!(4, d.entries, "five rows, and the image-only row is not an entry");
    assert!(d.strings > 0, "the invariant was stated over real strings");
}

/// The committed format has one shape signature and one-line reason for each non-bug.
///
/// Every other case is an error in a committed file, not a warning. A reason-free entry
/// suppresses a shape that nobody can review. A duplicate hides one approved reason. A
/// misspelled key causes both defects.
#[test]
fn a_suppression_entry_is_a_signature_and_a_one_line_reason() {
    let one = concat!(
        "[[suppress]]\n",
        "signature = \"dropped-text | Some Dict | content > div > text\"\n",
        "reason = \"The author's own blank line; a reader loses nothing.\"\n",
    );
    let list = Suppressions::parse(one).expect("a well-formed list");

    assert_eq!(1, list.entries.len());
    let e = &list.entries["dropped-text | Some Dict | content > div > text"];
    assert_eq!(Some(Invariant::DroppedText), e.invariant);
    assert_eq!("The author's own blank line; a reader loses nothing.", e.reason);
    assert_eq!(0, e.absorbed, "an entry absorbs nothing until a run hands it something");

    let refused = |text: &str| match Suppressions::parse(text) {
        Err(e) => format!("{e:#}"),
        Ok(_) => panic!("a defective list parsed"),
    };
    assert!(
        refused(&one.replace("The author's own blank line; a reader loses nothing.", ""))
            .contains("no reason"),
        "an exemption with no stated reason",
    );
    assert!(
        refused(&format!("{one}{one}")).contains("listed twice"),
        "one signature, two reasons, and no way to tell which was reviewed",
    );
    assert!(
        Suppressions::parse(&one.replace("reason =", "resaon =")).is_err(),
        "a misspelled key is not a silent exemption",
    );
    assert!(
        Suppressions::parse(&one.replace(
            "\"The author's own blank line; a reader loses nothing.\"",
            "\"\"\"\nnot\none line\n\"\"\"",
        ))
        .is_err(),
        "a reason is one line, so a reviewer reads a list rather than an essay",
    );
    assert!(Suppressions::parse("").expect("no exemptions is a list").entries.is_empty());
}

/// The summary names each exemption that the sweep cannot use.
///
/// A committed entry can be stale in two ways. `UNKNOWN` names a signature for an
/// invariant that this build cannot produce. No violation can match it. `UNUSED` names a
/// valid signature that no violation matched. It shows that the exemption outlived the
/// defect it suppressed.
#[test]
fn an_unknown_or_unused_suppression_is_named_in_the_summary() {
    let text = concat!(
        "[[suppress]]\n",
        "signature = \"dropped-text | Some Dict | content > text\"\n",
        "reason = \"A shape no archive in this run held.\"\n",
        "\n",
        "[[suppress]]\n",
        "signature = \"dropped-glyph | Some Dict | content > text\"\n",
        "reason = \"An invariant no build checks; the name was never one.\"\n",
    );
    let list = || Suppressions::parse(text).expect("a well-formed list");

    let full = Report { suppress: list(), ..Report::default() }.summary();
    assert!(verdict_for(&full, "dropped-text |").contains("UNUSED"), "{full}");
    assert!(verdict_for(&full, "dropped-glyph |").contains("UNKNOWN"), "{full}");
    assert!(full.contains("entries=2  absorbed=0  unused=1  unchecked=0  unknown=1"), "{full}");

    // A partial run left rows unread. It cannot call an entry `UNUSED`.
    // An `UNKNOWN` entry stays `UNKNOWN` for every row count.
    let partial = Report { suppress: list(), partial: true, ..Report::default() }.summary();
    assert!(!partial.contains("UNUSED"), "{partial}");
    assert!(partial.contains("unused=0  unchecked=0  unknown=1"), "{partial}");
    assert!(verdict_for(&partial, "dropped-glyph |").contains("UNKNOWN"), "{partial}");

    // A run that did not check *No dropped text* cannot call its exemption `UNUSED`.
    // The run offered no violation to that entry.
    let narrowed = Report {
        suppress: list(),
        only: named_invariants("horizontal-overflow"),
        ..Report::default()
    }
    .summary();
    assert!(verdict_for(&narrowed, "dropped-text |").contains("unchecked"), "{narrowed}");
    assert!(narrowed.contains("unused=0  unchecked=1  unknown=1"), "{narrowed}");
    assert!(
        narrowed.contains("this run checked horizontal-overflow"),
        "and the summary says which invariants it ran: {narrowed}",
    );
}

/// Returns the summary line for one suppression entry.
///
/// Both suppression tests read a verdict from the printed report, not a field. Review
/// can then audit a count that the run prints.
fn verdict_for<'a>(summary: &'a str, needle: &str) -> &'a str {
    summary
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line for {needle} in:\n{summary}"))
}

/// Checks that the build reads the committed suppression list.
///
/// A developer edits this file by hand between sweeps. Without this test, a typo remains
/// hidden until the next sweep, which can start hours later. This is the only CI check
/// for a dictionary file that CI does not otherwise read.
#[test]
fn the_committed_suppression_list_parses() {
    let path = suppression_file();
    Suppressions::load(&path).unwrap_or_else(|e| panic!("{e:#}"));
}

/// Tests the complete suppression path with a real archive.
///
/// `sweep.zip` renders four rows as two distinct violation shapes.
/// `sweep-suppressions.toml` suppresses one shape and names a third shape that no row
/// produces. One run shows every suppression result. One exemption absorbs violations
/// and reports its count. One violation remains a candidate. One exemption absorbs
/// nothing and receives `UNUSED`.
///
/// One row declares a runaway indent. *No horizontal overflow* checks this shape. A
/// browser places the shape the same way. This shape replaces a `<ruby>` without a base.
/// Issue 21 recorded a dropped reading for the earlier shape. The two ruby rows remain
/// controls. If the renderer loses a reading again, this test increases the violation
/// count.
#[test]
fn a_suppressed_fixture_shape_absorbs_its_violations_and_writes_no_candidate() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yomitan");
    let zip = dir.join("sweep.zip");
    let suppressed =
        "horizontal-overflow | FixtureSweep | content > div{padding-left} > <element-box>";
    let open = "horizontal-overflow | FixtureSweep | \
                content > div[data-sc-content]{padding-left} > <element-box>";
    let never = "horizontal-overflow | FixtureSweep | \
                 content > div[data-sc-never]{padding-left} > <element-box>";
    let list = || {
        Suppressions::load(&dir.join("sweep-suppressions.toml")).expect("the fixture list")
    };

    let mut report = Report { suppress: list(), ..Report::default() };
    sweep_archive(&zip, 1, None, &mut report);

    let d = &report.dicts[0];
    assert_eq!("FixtureSweep", d.dictionary);
    assert_eq!(0, d.errors, "no fixture row panics");
    assert_eq!(4, d.entries, "two readings that render, two indents that overflow");
    assert_eq!(
        2, d.violations,
        "an exemption may never lower the violation count, or nobody can audit it",
    );
    assert_eq!(1, d.suppressed, "one of the two shapes is exempt");
    assert_eq!(1, d.candidates, "the other still needs adjudicating");
    assert_eq!(vec![open], report.candidates.keys().collect::<Vec<_>>());

    let summary = report.summary();
    assert!(verdict_for(&summary, suppressed).contains("absorbed=1"), "{summary}");
    assert!(!verdict_for(&summary, suppressed).contains("UNUSED"), "{summary}");
    assert!(verdict_for(&summary, never).contains("absorbed=0"), "{summary}");
    assert!(verdict_for(&summary, never).contains("UNUSED"), "{summary}");
    assert!(
        summary.contains("entries=2  absorbed=1  unused=1  unchecked=0  unknown=0"),
        "{summary}",
    );

    // The directory contains the candidate that needs review.
    // It contains no file for the shape that a human judged.
    let out = std::env::temp_dir()
        .join(format!("chibipop-sweep-suppress-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    let written = report.write(&out).expect("writing candidates");
    assert_eq!(1, written.len(), "a suppressed shape produces no candidate file");
    let text = std::fs::read_to_string(&written[0]).expect("the candidate file");
    let json: serde_json::Value = serde_json::from_str(&text).expect("readable JSON");
    assert_eq!(open, json["signature"], "{text}");
    std::fs::remove_dir_all(&out).expect("cleaning up");

    // A capped run reads one row. It cannot call an unreached shape `UNUSED`.
    let mut capped = Report { suppress: list(), ..Report::default() };
    sweep_archive(&zip, 1, Some(1), &mut capped);
    let capped = capped.summary();
    assert!(!capped.contains("UNUSED"), "{capped}");
}


