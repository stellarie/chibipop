//! The corpus sweep: real archives through the real renderer.
//!
//! A sweep is a local-only run that pushes every entry of a corpus directory
//! through the real structured-content parser, the real `styles.css` cascade,
//! and [`FakeMeasure`] - the same seam the rest of this module tests against -
//! and checks a render invariant over every scene it gets. It lives here
//! rather than in a tool because the invariants are the layout suite's own
//! property checks turned on input nobody wrote down: the only thing the
//! corpus adds is shapes. The spec is `.scratch/render-sweep/spec.md`.
//!
//! Nothing here runs in CI. [`corpus_render_sweep`] is `#[ignore]`d and asks
//! for a corpus directory by environment variable, and the corpus never enters
//! the repo. What CI does run is the invariant's own unit tests and
//! [`a_row_capped_sweep_of_the_fixture_archive_reports_every_entry`], a sweep
//! of the committed three-row fixture archive - so the machinery is proven
//! without the corpus.
//!
//! One thing a sweep does commit: its [`Suppressions`], the adjudicated
//! non-bugs at `tests/render-sweep/suppressions.toml`. A candidate quotes a
//! dictionary verbatim and stays out of the repo; a verdict about a shape is
//! exactly what the repo should remember, so the sweep's judgment
//! accumulates and a re-run stays quiet about what a human already decided.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;

use super::*;
use crate::dict::archive::{for_each_term, is_frequency_archive, read_index, read_styles_css};
use crate::dict::gloss::{plain_items, renders_text, GlossDoc, Kind, NodeId, NodePath, StyleKey};
use crate::dict::sheet::{self, Sheet};
use crate::present::Presentation;

/// The panel width every swept entry is laid out at.
///
/// The width the rest of this suite measures at, so a candidate's numbers
/// read against the same arithmetic every other test in the file states.
const SWEEP_W: f32 = 424.0;

/// Tall enough that no entry in the corpus is clamped.
///
/// A scene keeps every element it stacked whatever this is - the clamp only
/// sets `view_h` - but a sweep that scrolled would still be reading a panel
/// no reader has, so it asks for the whole thing.
const SWEEP_H: f32 = 100_000.0;

/// Every editorial role reaches the panel.
///
/// A sweep asks what the renderer can *lose*, so it turns off the one filter
/// a reader's settings would otherwise apply. Two reasons, both about noise:
/// a part-of-speech label the default filter lifts into the card's own field
/// is not a dropped string, and a subtree the sweep never asked for could not
/// be a candidate anyway - so the widest filter is both the quietest and the
/// one that renders the most shapes.
const SWEEP_ROLES: RoleFilter =
    RoleFilter { examples: true, attributions: true, part_of_speech: true };

/// What a swept entry is drawn with.
///
/// Every knob that would hide a dictionary's own work is on: its list
/// stacking, its declarations, its images. The sweep is looking for defects
/// in what the renderer does with a dictionary's markup, and a setting that
/// dropped the markup would hide them.
fn sweep_settings() -> RenderSettings {
    RenderSettings { stack_items: true, styling: true, images: true, roles: SWEEP_ROLES }
}

/// Which render invariant a violation broke.
///
/// [`as_str`](Self::as_str) is the candidate file's own name for it, so one
/// shape reads the same in a filename, in a signature, and in the summary.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Invariant {
    /// A visible string the parsed entry holds that the scene does not draw.
    DroppedText,
    /// A small marked fragment trailing prose that the scene draws on a
    /// paragraph of its own.
    OrphanFragment,
    /// More gutter between a list marker's box and its item's first glyph
    /// than the tree asked for.
    MarkerGap,
}

impl Invariant {
    /// Every invariant this build checks.
    ///
    /// Read only by [`is_candidate_file`], so a candidate this sweep did not
    /// write is never mistaken for a stale one of its own. An invariant added
    /// without joining this list would leave its files behind after a run
    /// that stopped flagging them.
    const ALL: [Invariant; 3] =
        [Invariant::DroppedText, Invariant::OrphanFragment, Invariant::MarkerGap];

    fn as_str(self) -> &'static str {
        match self {
            Invariant::DroppedText => "dropped-text",
            Invariant::OrphanFragment => "orphan-fragment",
            Invariant::MarkerGap => "marker-gap",
        }
    }

    /// The invariant a name spells, or `None` when this build checks no
    /// such thing.
    ///
    /// The inverse of [`as_str`](Self::as_str), and the whole of what makes
    /// a suppression entry *unknown*: a committed exemption naming an
    /// invariant that has since been renamed or removed can never absorb
    /// anything, so the run says so rather than letting it rot.
    fn named(name: &str) -> Option<Invariant> {
        Invariant::ALL.into_iter().find(|i| i.as_str() == name)
    }
}

/// The fingerprint that makes two violations the same candidate.
///
/// The violated invariant, the dictionary, and the shape around the
/// violation - and the shape is one string per node from the glossary item
/// down to the node that broke the rule, each carrying both the selector a
/// stylesheet would key on and the properties that actually resolved there.
/// That is the spec's "structural path and resolved selectors" as one value,
/// because in this tree they are one walk: `dict::sheet` selects on tags and
/// `data-*` hooks, and what it wins folds into the node's own style record.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Signature {
    invariant: Invariant,
    dictionary: String,
    /// Glossary item first, violating node last.
    shape: Vec<String>,
}

impl Signature {
    /// What [`key`](Self::key) puts between its three fields.
    ///
    /// Named because two things read the format: a candidate file writes it
    /// and the suppression list matches against it, and a separator the two
    /// disagreed about would make every exemption silently miss.
    const FIELDS: &'static str = " | ";

    /// The shape signature as one line: what one candidate is keyed by, and
    /// what the filename's digest is taken over.
    fn key(&self) -> String {
        let sep = Self::FIELDS;
        let shape = self.shape.join(" > ");
        format!("{}{sep}{}{sep}{shape}", self.invariant.as_str(), self.dictionary)
    }

    /// The invariant a written signature names, or `None` when this build
    /// cannot have produced it.
    ///
    /// `None` covers both ways a committed suppression can be stale: a
    /// signature naming an invariant nothing checks, and one with too few
    /// fields to be a signature at all. The dictionary title is not
    /// validated - the corpus is local and unbounded, so a name this run saw
    /// no archive for is an *unused* entry, not a malformed one.
    fn named_invariant(key: &str) -> Option<Invariant> {
        let mut fields = key.split(Self::FIELDS);
        let name = fields.next()?;
        // A dictionary and a shape have to follow it. A title holding the
        // separator only splits into more fields, never fewer.
        let (_dictionary, _shape) = (fields.next()?, fields.next()?);
        Invariant::named(name)
    }
}

/// One invariant violation, before dedup.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Violation {
    signature: Signature,
    /// What the check measured, for the candidate file to carry.
    measured: BTreeMap<String, String>,
}

/// One visible string a parsed entry holds, and the shape around it.
struct Visible {
    text: String,
    shape: Vec<String>,
}

/// One string with every whitespace run collapsed to one space, trimmed.
///
/// Both sides of every text comparison go through it, because a renderer
/// folding a dictionary's own newlines and double spaces is doing its job:
/// only a string that is *gone* is a violation.
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

/// One node as a selector, with what resolved on it.
///
/// The two halves of a shape signature in one string: the tag and every
/// `data-sc-*` hook a dictionary's own stylesheet could select on, then the
/// properties that actually resolved there. Both lists are sorted, because a
/// signature may not depend on the order a dictionary happened to write its
/// `data` fields or its declarations in.
///
/// A hook's *name* is shape and its *value* is not. The `data` key namespace
/// is per-dictionary and unbounded (`docs/research/dict-shapes.md`), and
/// Jitendex proves what that means for a fingerprint: its example boxes carry
/// `data.sentence-key` and `data.source`, both holding a per-entry
/// identifier, so a signature over values gave 26 186 violations 6 892
/// distinct shapes - one per entry, which is no dedupe at all. Nothing is
/// lost by dropping them, because the resolved property list already says
/// whether the stylesheet keyed on this node and what it won there: two
/// `data.content` values a sheet styles identically *are* one shape to the
/// renderer, and the exemplar quotes the entry verbatim either way.
fn step(doc: &GlossDoc, id: NodeId) -> String {
    let mut out = match doc.tag_name(id) {
        // A bare glossary string and a `content`-only wrapper object both
        // parse to no tag, and they are not the same shape.
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

/// An untagged node's name, for a signature to carry.
///
/// Exhaustive for the same reason [`style_key_name`] is: a kind added to the
/// tree must be named here rather than silently sharing a shape with
/// something it is not.
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

/// A resolved property's name, for a signature to carry.
///
/// An exhaustive `match` rather than `Debug`, so adding a property to
/// [`StyleKey`] is a compile error here and not a silent hole in every
/// signature that touches it.
///
/// Deliberately not `gloss::html`'s `css_name`, which spells
/// [`StyleKey::TextDecorationLine`] as the `text-decoration` shorthand
/// because the longhand is younger than some renderers an Anki note template
/// runs in. That is a statement about browsers; a signature names the
/// property the cascade resolved, and the two answers must not be forced to
/// agree.
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

/// Every visible string a parsed entry holds, with the shape around each.
///
/// The oracle half of *no dropped text*: what the renderer is answerable
/// for. Three subtrees are the renderer's by right and not counted - a role
/// the filter drops, an image node (whose text is its `alt`, and an image
/// that resolves draws the asset instead), and `<rp>`, which exists only for
/// a renderer that cannot draw ruby, and this one can.
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

/// Every string a scene draws, folded, in draw order.
///
/// A run's own text, plus the markers and readings that sit out of its flow:
/// all three are glyphs a reader sees, so all three answer for a string the
/// parsed entry held. Joined with a space rather than concatenated, so a
/// match can never be sewn together across two paragraphs that never touch
/// on screen.
fn drawn_text(s: &PopupScene) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(s.elems.len());
    for e in &s.elems {
        parts.push(e.text.as_str());
        parts.extend(e.marker.iter().map(|m| m.text.as_str()));
        parts.extend(e.ruby.iter().map(|r| r.text.as_str()));
    }
    folded(&parts.join(" "))
}

/// What one entry's invariants saw.
///
/// The check counts ride along because zero violations has to be
/// distinguishable from zero checks: a walk that stopped finding text would
/// otherwise report a clean corpus, which is the one failure a sweep must
/// never be able to hide. One count per invariant rather than one total, so
/// no invariant's silence can hide behind another's work.
#[derive(Default)]
struct Checked {
    /// Visible strings *no dropped text* was stated over.
    strings: u64,
    /// Trailing fragments *no orphan trailing fragment* was stated over.
    fragments: u64,
    /// Marker boxes *bounded marker gap* was stated over.
    markers: u64,
    /// Checks an invariant declined to make.
    ///
    /// A fragment no paragraph draws as one string, or a marker no ancestor
    /// list of the paragraph owns: in both, the sweep has no ground to judge
    /// on and says nothing. Counted because saying nothing quietly is how a
    /// checker stops working without anybody noticing - a decline that grew
    /// into the thousands would be a shape this invariant needs to learn,
    /// not a clean corpus.
    declined: u64,
    violations: Vec<Violation>,
}

impl Checked {
    /// Every invariant's answer for one entry, as one.
    fn merge(&mut self, other: Checked) {
        self.strings += other.strings;
        self.fragments += other.fragments;
        self.markers += other.markers;
        self.declined += other.declined;
        self.violations.extend(other.violations);
    }
}

/// *No dropped text*: every visible string the parsed entry holds reaches
/// the scene.
///
/// The first invariant, and the one that needs no threshold: a string is
/// either drawn or it is not. It is stated as containment rather than
/// equality because a paragraph legitimately holds more than one node's text
/// and a renderer legitimately folds whitespace - see
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

/// A trailing fragment is *small* at or below this many drawn characters.
///
/// A long marked node on its own line reads as a sense separation, which is
/// what the marker line break is *for*, so the cap is what keeps this
/// invariant pointed at fragments instead of at every break in every entry.
/// Twelve clears Jitendex's footnote mark - `[1]` through `[12]`, the whole
/// of the #18 class - with room for a mark that carries its own
/// punctuation.
///
/// Measured over the local archive: of the 65 004 marked nodes that trail
/// prose in Jitendex, this cap checks 64 980. It excludes 24, and it is
/// there for the other 96 archives rather than for this one - a cap that
/// admitted every marked node would state the invariant over phrases, where
/// standing on a line of one's own is correct.
const ORPHAN_MAX_CHARS: usize = 12;

/// How much other text must share the paragraph a trailing fragment landed
/// in, in characters.
///
/// One, because the whole of the #18 class is a fragment drawn with
/// *nothing else* in its paragraph: the renderer opened a line for the mark
/// alone, under the sentence it belongs to. Company rather than lead,
/// measured against the corpus: Jitendex's `example-keyword` (51 062 nodes)
/// is often the first word of its own sentence, so a fragment with no prose
/// *ahead* of it is routine and correct - it is a fragment with no prose
/// beside it at all that is the defect. A larger number asks the renderer
/// to hold a fragment back until some quota of sentence is drawn, which is
/// no rule a browser has, so it belongs only in the test that tightens it
/// ([`the_fixed_footnote_re_flags_when_its_company_threshold_is_tightened`]).
const ORPHAN_MIN_COMPANY_CHARS: usize = 1;

/// Gutter tolerated between a marker box and its item's first glyph beyond
/// what the tree asks for, as a fraction of the item's own em.
///
/// The #19 class is a whole default list level of surplus -
/// [`LIST_INDENT_EM`], 1.4em - and the fixed shape's gap is the 0.5em its
/// dictionary declared, so any tolerance under a level and over rounding
/// separates the two. Half an em is also what absorbs the difference
/// between the em a declared padding resolved against and the em this check
/// spends it in: a length is read as a multiple of its own node's size, and
/// that node may sit at a smaller size than the paragraph the gap ends at.
const MARKER_GAP_SLACK_EM: f32 = 0.5;

/// The tuned numbers the paragraph-family invariants read.
///
/// Passed in rather than read from the constants directly, because a
/// detector nobody can tighten is a detector nobody can trust: the two
/// resolved tickets are checked by rendering their verbatim shapes, asking
/// for zero candidates, and then tightening a threshold past the fix until
/// the same geometry is flagged again. That is the only evidence available
/// that these checkers would have caught the defects they were written for,
/// since the fixes are in the build that runs them.
#[derive(Clone, Copy, Debug)]
struct Thresholds {
    orphan_max_chars: usize,
    orphan_min_company: usize,
    marker_gap_slack_em: f32,
}

impl Thresholds {
    /// What a sweep runs with: the tuned constants above.
    const DEFAULT: Thresholds = Thresholds {
        orphan_max_chars: ORPHAN_MAX_CHARS,
        orphan_min_company: ORPHAN_MIN_COMPANY_CHARS,
        marker_gap_slack_em: MARKER_GAP_SLACK_EM,
    };
}

/// One small marked fragment trailing prose, and the shape around it.
struct Fragment {
    text: String,
    shape: Vec<String>,
}

/// Every small marked fragment that trails prose inside its own parent.
///
/// The oracle half of *no orphan trailing fragment*: the marker line
/// break's own exemption, stated from outside the walk. A marked inline
/// node whose parent already held prose is markup *inside* a sentence
/// ([`GlossDoc::inline_prose`]), so the renderer owes it the sentence's
/// paragraph. What the walk would legitimately break before is excluded
/// here rather than judged and forgiven later: a marked node with no prose
/// ahead of it separates senses, and one holding a block of its own opens a
/// line by tag and not by mark.
fn trailing_fragments(doc: &GlossDoc, roles: RoleFilter, t: Thresholds) -> Vec<Fragment> {
    let mut out = Vec::new();
    let mut shape = Vec::new();
    // A top-level item has no parent to hold prose, so nothing trails
    // anything until a node's own children are walked.
    for item in doc.items() {
        walk_fragments(doc, item, roles, false, t, &mut shape, &mut out);
    }
    out
}

/// `prose` is what the walk's own accumulator holds at this node: prose
/// already seen among the parent's children, in the parent's order. Order is
/// load-bearing exactly as it is in [`Paragraphs::children`] - a sense head
/// is marked pills *followed* by a prose fragment, so prose behind a marked
/// node is no evidence at all.
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

/// Is this node one run of inline content - nothing under it that opens a
/// line of its own?
///
/// A marked node holding a `div`, a list, a table or an image is drawn on
/// its own line because of what it *holds*, and a renderer that kept it in
/// the sentence would be the defect. So such a node is never a fragment,
/// however small its text.
fn inline_only(doc: &GlossDoc, id: NodeId) -> bool {
    let node = doc.node(id);
    !node.tag.is_block()
        && !matches!(
            node.kind,
            Kind::List | Kind::ListItem | Kind::Table | Kind::Row | Kind::Cell | Kind::Image
        )
        && doc.children(id).all(|child| inline_only(doc, child))
}

/// One node's text as a run concatenates it, with nothing in it a reader
/// cannot see.
///
/// Joined with nothing, unlike [`drawn_text`]'s walk over whole elements: a
/// run's spans sit character after character in one string, so this is the
/// needle that string is searched for. Three things a run does not hold are
/// dropped here, or the needle would never match the haystack:
///
/// - `<rt>`, a reading, which does not flow and rides the element as a
///   [`RubyBox`] instead ([`measure_readings`]);
/// - `<rp>`, for the reason [`visible_strings`] drops it;
/// - an image, whose text is its `alt`.
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

/// One string with every glyph gone that the renderer writes into a run and
/// a reader never sees.
///
/// [`RUBY_FILLER`] is the word joiner every ruby base wears, and it sits
/// *between* the base's own characters - so a needle taken from the tree
/// cannot match a drawn run holding one unless both sides drop it. Compared
/// against the filler itself rather than against a second copy of its
/// codepoint, so the two cannot drift apart. The zero-width space rides
/// along because a shaper gives it no advance either
/// ([`zero_advance`](super::tests::zero_advance)), and which of the two a
/// pass reached for is not shape.
fn unglued(text: &str) -> String {
    let mut buf = [0u8; 4];
    text.chars()
        .filter(|c| c.encode_utf8(&mut buf) != RUBY_FILLER && *c != '\u{200b}')
        .collect()
}

/// *No orphan trailing fragment*: a small marked fragment trailing prose is
/// drawn in the paragraph it trails, not on a line of its own.
///
/// Stated over the drawn paragraphs rather than over the walk's decisions,
/// so it holds whatever rule put the fragment where it is: the measured
/// number is how much text the renderer drew beside the fragment in the
/// paragraph it landed in, and the #18 class is that number being zero.
/// A fragment the scene draws nowhere is *no dropped text*'s business and
/// not this one's.
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
        // Counted only once the fragment is one this invariant can speak
        // about: a fragment the scene draws nowhere as one string was never
        // stated over, and a count that rose for it would be the
        // reassurance ticket 01's string count exists to refuse.
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

/// The most text any one paragraph drew beside `fragment`, or `None` when
/// no paragraph drew the fragment at all.
///
/// The best case rather than the first, because a short fragment's own text
/// may appear in more than one paragraph of an entry and the question is
/// whether the renderer kept it in a sentence *somewhere*: one paragraph
/// that did is the answer, and flagging the coincidence would be noise.
fn best_company(drawn: &[String], fragment: &str) -> Option<usize> {
    let own = fragment.chars().count();
    drawn
        .iter()
        .filter(|text| text.contains(fragment))
        .map(|text| text.chars().count().saturating_sub(own))
        .max()
}

/// *Bounded marker gap*: what stands between a marker box and its item's
/// first glyph is what the tree asked for.
///
/// The gap is read off the placed box - [`place_markers`] right-aligns it
/// against the content edge of the list that owed it, so what is left of
/// the paragraph's pen is exactly the indent the levels *below* that list
/// added. The oracle is the browser rule the #19 fix states: each level
/// costs [`LIST_INDENT_EM`] unless the list declares its own left padding,
/// in which case the declaration replaces it, and each node's own declared
/// left margin, border and padding costs what it declares.
///
/// Only the innermost marker is asked about. An item whose whole content is
/// a nested list hangs two markers on one line, and the outer one is
/// *meant* to stand a level away from the text.
fn marker_gap(dictionary: &str, doc: &GlossDoc, s: &PopupScene, t: Thresholds) -> Checked {
    let mut checked = Checked::default();
    for e in &s.elems {
        let Some(mark) = e.marker.last() else { continue };
        // A paragraph with no address, or a list this sweep cannot line the
        // marker up with, is one it declines to judge rather than one it
        // flags on a guess - and a decline is counted as itself, never as a
        // check. A run of them has to be able to make `markers=` fall: an
        // origin path that stopped resolving would otherwise leave the count
        // where it was while every judgment quietly stopped.
        let Some(chain) = e.origin.and_then(|o| o.path).and_then(|p| chain_of(doc, p)) else {
            checked.declined += 1;
            continue;
        };
        let Some(at) = marker_list(doc, &chain) else {
            checked.declined += 1;
            continue;
        };
        checked.markers += 1;
        // The marker's own trailing gap is inside its width ([`MARKER_GAP`]),
        // so this is what stands between the box and the pen: the indent the
        // levels below the marker's list added, and nothing the marker paid
        // for itself.
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

/// The nodes a path addresses, glossary item first and the addressed node
/// last.
///
/// The arena keeps first-child and next-sibling links and no parent link,
/// so a scene element's own [`NodePath`] is the only way back up its tree -
/// which is what a gap needs, since the room between a marker and a glyph
/// is owed by the ancestors between them.
///
/// [`NodePath::resolve`] walks the same route and keeps only its end, and a
/// path holds no prefix constructor to hand this the levels one at a time.
/// So the descent is written twice on purpose rather than widening a
/// dictionary API for a test's need - if the route ever stops being "the
/// nth item, then the nth child", both change together.
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

/// Where in `chain` the innermost drawn marker's own list sits.
///
/// The deepest list ancestor that draws a marker for the item beneath it on
/// this chain, which is the list whose gutter the innermost [`MarkerBox`]
/// hangs in ([`Paragraphs::list`]). `None` when no ancestor list draws one:
/// the marker on this paragraph then belongs to a list the paragraph is not
/// inside, which this sweep does not judge.
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

/// Does `list` draw a marker beside `item`?
///
/// `list-style-type` is inherited and the marker is drawn at the item, so
/// the resolution is the list's own over its tag's initial value and then
/// the item's over that - the two calls [`Paragraphs::list`] makes, in its
/// order. The ordinal is irrelevant to whether a marker exists at all, so
/// the first item's is asked for.
///
/// [`Paragraphs::list`]: super::marker::Paragraphs::list
fn marker_draws(doc: &GlossDoc, list: NodeId, item: NodeId) -> bool {
    let ordered = doc.node(list).tag == Tag::Ol;
    let inherited = styled_marker(doc, list, initial_marker(ordered), true);
    styled_marker(doc, item, inherited, true).label(1).is_some()
}

/// The left indent the nodes below a marker's list are owed, in ems.
///
/// One term per node: what it declares, plus a default list level for a
/// list that declared no left padding of its own. That second clause is the
/// browser rule the #19 fix states - an author's `padding-left` on a list
/// *replaces* the UA gutter rather than adding to it - and stating it here
/// rather than reading the walk's arithmetic is what makes this an oracle
/// instead of a copy.
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

/// One node's declared left edge of one box property, in ems of its own
/// size.
///
/// The wider of the shorthand's left edge and the longhand, where the
/// cascade would take whichever the dictionary wrote last. Deliberately
/// generous: this is the room a gap is *allowed*, so reading a declaration
/// too widely costs a candidate nobody had to adjudicate, and reading it
/// too narrowly costs a false one somebody does.
///
/// Resolved against a unit em, which makes the answer the multiple itself:
/// the gap it is compared against is measured at the paragraph's own size,
/// and [`MARKER_GAP_SLACK_EM`] is what covers a node that declared its
/// padding at some other one.
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

// ---- the suppression list ----

/// The committed memory of adjudicated non-bugs.
///
/// A sweep's judgment has to accumulate, or every run re-presents the shapes
/// the last one already decided were fine. This is the whole of that memory:
/// shape signature to one-line reason, read from a file a human reviews in a
/// diff. Nothing here quotes a dictionary, which is what makes it the one
/// part of a sweep that may be committed.
///
/// Keyed by signature, so absorbing a violation is one probe rather than a
/// scan of the list, and the summary prints in a stable order whatever order
/// the file happened to list.
#[derive(Default)]
struct Suppressions {
    entries: BTreeMap<String, Suppression>,
}

/// One adjudicated non-bug, and what this run handed it.
struct Suppression {
    /// Why the shape is not a bug, as the file states it.
    reason: String,
    /// `None` when this build checks no such invariant: the entry can never
    /// absorb anything, so a run reports it rather than counting it unused
    /// for a reason nobody could act on.
    invariant: Option<Invariant>,
    /// Violations this entry absorbed in this run.
    ///
    /// Zero after a full run is the whole of *unused*: an exemption that
    /// stopped applying is one nobody would otherwise notice, and a list
    /// that only ever grows is how exemptions widen.
    absorbed: u64,
}

/// The suppression list as the committed file spells it.
///
/// `deny_unknown_fields` because the two keys are the whole format: a
/// misspelled `resaon` would otherwise commit an exemption with no stated
/// reason, which is the one thing this file exists to prevent.
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

    /// One entry per shape, or an error naming the entry that stopped it.
    ///
    /// Every defect here is a defect in a *committed* file, so each is an
    /// error rather than a warning: an entry with no reason exempts a shape
    /// nobody can review, and a duplicate signature silently drops one of
    /// the two reasons a reviewer approved.
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

    /// Absorbs one violation, if an entry claims its shape.
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

/// One deduplicated violation awaiting adjudication.
///
/// One shape, one exemplar, an occurrence count. The exemplar is the *first*
/// entry the shape appeared in rather than the worst, because a sweep that
/// re-chose an exemplar would rewrite a candidate file every run and an
/// adjudicator would lose the diff.
struct Candidate {
    signature: Signature,
    occurrences: u64,
    exemplar: Exemplar,
}

/// The one entry a candidate quotes.
struct Exemplar {
    /// The row's one-based ordinal in its archive.
    ///
    /// Named for what it is: the sweep reads archives, so this is not the
    /// `entry_id` a built `chibipop.sqlite` would give the row, and an
    /// adjudicator who treated it as one would open the wrong record. The
    /// headword beside it is how the row is actually found again.
    row: i64,
    term: String,
    reading: String,
    measured: BTreeMap<String, String>,
    /// The term-bank row's glossary, verbatim.
    ///
    /// Verbatim because adjudication reasons about what the dictionary author
    /// wrote, and a summarised tree is a second opinion about it. This is
    /// also the only licensing-relevant string in the whole report, and it is
    /// why candidates are never committed.
    glossary: String,
}

/// What one dictionary's sweep saw.
#[derive(Default)]
struct DictSummary {
    dictionary: String,
    entries: u64,
    /// Visible strings *no dropped text* was stated over.
    strings: u64,
    /// Trailing fragments *no orphan trailing fragment* was stated over.
    fragments: u64,
    /// Marker boxes *bounded marker gap* was stated over.
    markers: u64,
    /// Checks an invariant declined to make. See [`Checked::declined`].
    declined: u64,
    violations: u64,
    /// Violations a committed suppression absorbed.
    ///
    /// Counted beside `violations` rather than taken out of it: an exemption
    /// that made the violation count fall would be an exemption nobody could
    /// audit, which is the one thing a suppression list must not become.
    suppressed: u64,
    candidates: u64,
    /// Entries whose parse or layout panicked, plus a walk this archive
    /// refused: an unreadable bank file is one error for the archive.
    errors: u64,
}

/// What filing one violation did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Filed {
    /// A committed suppression claimed its shape; no candidate exists.
    Suppressed,
    /// A shape already seen: its candidate's count went up.
    Repeat,
    /// A shape not seen before: a candidate was opened for it.
    Fresh,
}

/// What one sweep saw.
#[derive(Default)]
struct Report {
    dicts: Vec<DictSummary>,
    /// By [`Signature::key`], so the report is ordered and one shape is one
    /// entry however many entries showed it.
    candidates: BTreeMap<String, Candidate>,
    /// The exemptions this run applied, and what each absorbed.
    suppress: Suppressions,
    /// Did this run leave rows unchecked?
    ///
    /// Learned from the walk itself - a row cap that fired, an unreadable
    /// bank file, an entry that panicked - rather than from the intent to
    /// cap, so it says what actually happened. It is what stops such a run
    /// from calling an exemption unused: the rows that would have used it
    /// may simply never have been read.
    partial: bool,
}

impl Report {
    /// Files one violation: absorbed by an exemption, or collapsed into the
    /// candidate for its shape.
    ///
    /// Suppression comes first, so a suppressed shape costs no exemplar and
    /// reaches no candidate file at all.
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

    /// The whole run summary, as a `--nocapture` run prints it.
    ///
    /// A string rather than a walk of `println!`s because the absorbed
    /// counts are the suppression list's own acceptance criterion, and a
    /// criterion no test can read is a wish.
    fn summary(&self) -> String {
        let mut out = String::new();
        let mut total = DictSummary { dictionary: "TOTAL".into(), ..DictSummary::default() };
        for d in &self.dicts {
            out.push_str(&summary_line(d));
            out.push('\n');
            total.entries += d.entries;
            total.strings += d.strings;
            total.fragments += d.fragments;
            total.markers += d.markers;
            total.declined += d.declined;
            total.violations += d.violations;
            total.suppressed += d.suppressed;
            total.errors += d.errors;
        }
        total.candidates = self.candidates.len() as u64;
        out.push_str(&summary_line(&total));
        out.push('\n');
        out.push_str(&self.suppression_summary());
        out
    }

    /// Every exemption this run carried: what it absorbed, and the verdict
    /// on an exemption that absorbed nothing.
    ///
    /// Printed whether or not the list is empty, because "no exemptions" is
    /// itself the answer to whether one widened.
    fn suppression_summary(&self) -> String {
        let mut out = String::new();
        let (mut absorbed, mut unused, mut unknown) = (0u64, 0u64, 0u64);
        for (signature, e) in &self.suppress.entries {
            absorbed += e.absorbed;
            let verdict = if e.invariant.is_none() {
                unknown += 1;
                "UNKNOWN"
            } else if e.absorbed == 0 && !self.partial {
                unused += 1;
                "UNUSED"
            } else {
                "ok"
            };
            out.push_str(&format!(
                "sweep  suppression  absorbed={:<8} {verdict:<8} {signature}  # {}\n",
                e.absorbed, e.reason,
            ));
        }
        let entries = self.suppress.entries.len();
        out.push_str(&format!(
            "sweep  suppressions  entries={entries}  absorbed={absorbed}  \
             unused={unused}  unknown={unknown}",
        ));
        if self.partial {
            out.push_str(
                "\nsweep  suppressions  rows went unread, so no entry is called unused",
            );
        }
        out
    }

    /// Writes one JSON file per candidate into `dir`, and returns them.
    ///
    /// Clears the sweep's own stale candidate files first, so what is on disk
    /// after a run is exactly that run's candidates and a shape that stopped
    /// appearing stops being adjudicated. Only files this sweep could have
    /// written are removed - `CHIBIPOP_SWEEP_OUT` can name any directory, and
    /// eating a neighbour's JSON would be a poor way to report a clean
    /// corpus.
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
        Ok(written)
    }
}

/// One summary row, dictionary or total.
fn summary_line(d: &DictSummary) -> String {
    format!(
        "sweep  {:<40} entries={:<8} strings={:<9} fragments={:<8} markers={:<8} \
         declined={:<7} violations={:<8} suppressed={:<8} candidates={:<5} errors={}",
        d.dictionary,
        d.entries,
        d.strings,
        d.fragments,
        d.markers,
        d.declined,
        d.violations,
        d.suppressed,
        d.candidates,
        d.errors,
    )
}

/// A candidate's filename: readable prefix, stable suffix.
fn candidate_file(c: &Candidate, key: &str) -> String {
    format!(
        "{}-{}-{:016x}.json",
        c.signature.invariant.as_str(),
        slug(&c.signature.dictionary),
        digest(key),
    )
}

/// Could this sweep have written this file?
///
/// The invariant name a candidate leads with is the whole test: it is what
/// makes clearing stale candidates safe in a directory the sweep does not
/// own.
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

/// FNV-1a over a signature's canonical text.
///
/// A short, stable name for a shape, so a candidate keeps its filename
/// across runs and a reviewer's diff shows a changed count rather than a
/// deleted file beside a new one.
fn digest(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ---- the sweep itself ----

/// One term-bank row as the sweep reads it.
///
/// The clump these six values are: they travelled as six parameters through
/// two functions and the order flipped between them, which is one transposed
/// argument away from a candidate that quotes the wrong entry.
///
/// `dict` and `row` are the sweep's *own* numbering - it reads archives, not
/// the built store, so neither is a `chibipop.sqlite` id and a candidate
/// names the headword as well as the ordinal.
struct Row {
    dict: String,
    dict_id: i64,
    /// One-based, in archive order.
    row: i64,
    term: String,
    reading: String,
    /// The row's glossary, serialised exactly as the builder stores it.
    glossary: String,
}

/// One corpus row as a whole popup: one card, one block, one entry.
///
/// A standalone panel with no side column, because a sweep is asking about
/// one entry's own render and a second dictionary in the scene would only
/// make every shape signature depend on what happened to sit beside it.
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
                // The row ordinal stands in for the store's own id: no
                // database is involved, and every sweep scene holds exactly
                // one entry, so nothing in the scene needs to tell two apart.
                entry_id: r.row,
                glosses: plain_items(doc),
                tags: Vec::new(),
                doc: Arc::clone(doc),
                media: Vec::new(),
            }],
        }],
        match_len: r.term.chars().count(),
    };
    Presentation {
        top: Some(card.clone()),
        collapsed: Vec::new(),
        all_cards: vec![card],
        sentence: None,
    }
}

/// One term-bank row through the whole renderer, checked by every
/// invariant.
///
/// `None` when the row renders no text at all: the dictionary builder gives
/// such a row no `entry` record, so no reader can ever hover it and it is not
/// the sweep's business either. The two calls around the scene are the hover
/// path's own, in its order - parse the stored text, fold the dictionary's
/// stylesheet into the tree, render - so a candidate is a defect a reader
/// could actually see.
fn sweep_entry(r: &Row, sheet: &Sheet) -> Option<Checked> {
    let mut doc = GlossDoc::parse(&r.glossary);
    if !renders_text(&doc) {
        return None;
    }
    sheet::apply(&mut doc, sheet);
    let doc = Arc::new(doc);
    let p = sweep_card(r, &doc);
    let theme = Theme::dark();
    let mut m = FakeMeasure::default();
    let s = scene(
        &SceneRequest {
            presentation: &p,
            theme: &theme,
            max_w: SWEEP_W,
            max_h: SWEEP_H,
            show_back: false,
            side_panel: false,
            render: sweep_settings(),
            anki: None,
        },
        &mut m,
    )
    .expect("FakeMeasure never refuses a run");
    let mut found = dropped_text(&r.dict, &doc, SWEEP_ROLES, &s);
    found.merge(orphan_fragment(&r.dict, &doc, SWEEP_ROLES, &s, Thresholds::DEFAULT));
    found.merge(marker_gap(&r.dict, &doc, &s, Thresholds::DEFAULT));
    Some(found)
}

/// The row cap, raised as an error because the archive walk is a stream and
/// stops on one.
///
/// `for_each_term` hands the caller one row at a time and aborts on the first
/// `Err`, which is the only way to stop before the last bank file - and
/// stopping is the whole point of the cap, since deserialising every row of a
/// 37 MB archive is most of what a capped run is trying to skip.
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
/// Every entry is rendered inside [`catch_unwind`](std::panic::catch_unwind):
/// a panic in the parser or the walk is this archive's own defect to report,
/// not a reason the other 96 archives go unswept. The default panic hook
/// still prints, which is what an adjudicator wants - a counted panic with no
/// backtrace is a worse bug report than a noisy one.
///
/// `cap` bounds the *rows read*, not the entries rendered, so an archive of
/// image-only gaiji rows costs a capped run no more than any other. That is
/// what makes the cap a cost bound rather than a sample size.
fn sweep_archive(zip: &Path, dict_id: i64, cap: Option<u64>, report: &mut Report) {
    let title = archive_title(zip);
    let css = read_styles_css(zip).ok().flatten().unwrap_or_default();
    let sheet = Sheet::compile(&css);
    let mut sum = DictSummary { dictionary: title.clone(), ..DictSummary::default() };
    let mut rows: i64 = 0;
    let walk = for_each_term(zip, |t| {
        if cap.is_some_and(|c| rows as u64 >= c) {
            return Err(anyhow::Error::new(RowCap));
        }
        rows += 1;
        let r = Row {
            dict: title.clone(),
            dict_id,
            row: rows,
            term: t.term.clone(),
            reading: t.reading.clone(),
            glossary: serde_json::to_string(&t.glossary)?,
        };
        let found =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sweep_entry(&r, &sheet)));
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
            // Rows this archive holds and this run never read.
            report.partial = true;
        } else {
            sum.errors += 1;
            eprintln!("sweep  {title}: {err:#}");
        }
    }
    // A panicked entry and an unreadable bank file are rows that went
    // unchecked as surely as capped ones, so neither may leave an exemption
    // looking stale.
    report.partial |= sum.errors > 0;
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

/// Every term archive in a corpus directory, in a stable order.
///
/// Frequency archives are skipped by the same test the builder uses: they
/// carry no glossary, so there is nothing in one to render.
fn corpus_archives(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading the corpus at {}: {e}", dir.display()))
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")))
        .filter(|p| !is_frequency_archive(p))
        .collect();
    // `read_dir` order is not defined, and a sweep numbers its dictionaries.
    out.sort();
    out
}

// ---- the environment ----

/// The corpus directory of Yomitan `.zip` archives.
const CORPUS_ENV: &str = "CHIBIPOP_SWEEP_CORPUS";
/// Term-bank rows read per dictionary. Unset reads every row of every
/// archive.
const ROWS_ENV: &str = "CHIBIPOP_SWEEP_ROWS";
/// Where candidate files land.
const OUT_ENV: &str = "CHIBIPOP_SWEEP_OUT";

/// The row cap, or `None` for the whole archive.
///
/// Unset is uncapped by design: the full run is the primary target and the
/// cap is the iteration loop, so forgetting to set it sweeps everything
/// rather than silently sweeping a sample.
fn row_cap() -> Option<u64> {
    let raw = std::env::var(ROWS_ENV).ok()?;
    Some(raw.parse().unwrap_or_else(|_| panic!("{ROWS_ENV} must be a row count, got {raw:?}")))
}

/// Where candidates are written.
///
/// Under `.scratch/`, which this repo does not track: a candidate quotes a
/// dictionary's own content verbatim and none of it may ever be committed.
fn candidate_dir() -> PathBuf {
    match std::env::var_os(OUT_ENV) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".scratch/render-sweep/candidates"),
    }
}

/// The committed suppression list.
///
/// A fixed repo path and no environment override, unlike the three knobs
/// above: those move where a *local* run reads and writes, and this file is
/// the run's committed memory. A sweep that could be pointed at a different
/// list would be a sweep whose exemptions nobody reviewed.
fn suppression_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/render-sweep/suppressions.toml")
}

/// A scene that lost a string the parsed entry holds is a violation, and the
/// candidate names both the string and the shape it sat in.
///
/// The scene is a real one, mutated: dropping a whole paragraph is exactly
/// what the #18 class of defect does to a fragment, and building the scene by
/// hand would only assert against a fixture of this test's own arithmetic.
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

/// One shape is one candidate, however many entries showed it.
///
/// The whole reason a signature exists: Jitendex's own footnote defect stood
/// 9 784 times, and an adjudication batch of 9 784 identical items is not a
/// batch. The exemplar is the first entry, so a candidate file's diff between
/// two runs is a changed count and not a swapped-out quotation.
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
    // The filename is a shape's stable name, so a rerun rewrites one file
    // rather than leaving a stale one beside a new one.
    assert_eq!(candidate_file(c, &key), candidate_file(c, &key));
    assert!(
        candidate_file(c, &key).starts_with("dropped-text-fixture-"),
        "readable prefix: {}",
        candidate_file(c, &key),
    );
}

/// The boundary: a renderer folding a dictionary's own whitespace and
/// splitting one item across lines has dropped nothing.
///
/// Every string here reaches the panel, but none of them reaches it
/// character-for-character - `  spaced\n   out  ` is drawn `spaced out`, and
/// the `br` puts `after` in a paragraph of its own. An invariant that
/// compared the two verbatim would flag all three and drown the adjudication
/// batch in its own noise.
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

/// One fixture glossary and its dictionary's own CSS, through the same two
/// calls [`sweep_entry`] makes.
///
/// The tree comes back beside the scene because the paragraph-family
/// invariants are stated over both: a checker reads the very tree the walk
/// read, folded stylesheet and all, so its oracle cannot drift from the
/// render it judges.
fn swept(glossary: &str, css: &str) -> (Arc<GlossDoc>, PopupScene) {
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
            media: Vec::new(),
        }],
    }]);
    let s = shown(&p, sweep_settings());
    (doc, s)
}

/// The #18 shape, verbatim from the corpus: Jitendex's example translation
/// and the footnote mark that trails it.
const FOOTNOTE_TREE: &str = concat!(
    r##"{"tag":"div","data":{"content":"example-sentence-b"},"content":["##,
    r##"{"tag":"span","lang":"en","content":"He still holds the heavyweight title."},"##,
    r##"{"tag":"span","data":{"content":"attribution-footnote"},"content":"[1]"}]}"##,
);

/// The sentence the mark trails, as the panel draws it.
const FOOTNOTE_SENTENCE: &str = "He still holds the heavyweight title.";

/// A fragment on a paragraph of its own is a violation, and the candidate
/// names both the fragment and the prose it was cut from.
///
/// The scene is a real one, mutated, exactly as
/// [`a_scene_that_lost_a_paragraph_is_a_dropped_text_violation`] is: the
/// footnote's fix is in this build, so the only way to put the defect back
/// is to split the paragraph the fix keeps whole - which is precisely what
/// the marker line break did to 9 784 Jitendex entries. Building the scene
/// by hand instead would assert against a fixture of this test's own
/// arithmetic.
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

/// The resolved #18 shape flags nothing, and flags again the moment the
/// threshold is tightened past the fix.
///
/// Both halves of the same sanity check. The fix is in this build, so a
/// clean run over the corpus node proves the invariant is quiet about
/// correct output; tightening [`ORPHAN_MIN_COMPANY_CHARS`] past the
/// sentence's own length proves the same checker is measuring the real
/// company of the real mark, and would have reported zero for the render
/// Jitendex readers actually saw.
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

/// The boundary: marked labels standing side by side are sense
/// separations, and a separation is not an orphan.
///
/// Jitendex's sense head, which is the shape the marker line break exists
/// for: a part-of-speech pill followed by a forms-restriction span, both
/// marked, neither trailing a sentence. Prose *behind* a marked node is no
/// evidence either - the second span here holds text and the first still
/// keeps its own line - so an invariant that read the parent
/// symmetrically would flag every sense head in the corpus.
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

/// Both orphan thresholds, at the exact value each turns on.
///
/// A threshold nothing pins is a threshold any edit can move: a `<=` that
/// became a `<` here would silently stop checking the widest fragment the
/// cap admits, and a `>=` that became a `>` would flag every fragment
/// keeping the least company there is. So one scene is stated twice - a
/// fragment of exactly [`ORPHAN_MAX_CHARS`] characters keeping exactly
/// [`ORPHAN_MIN_COMPANY_CHARS`] character of company, then the same
/// fragment one character longer, which is no longer a fragment at all.
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

/// The #19 shape, verbatim: Jitendex's sense list around the glossary list
/// that declares its own indent.
const SENSE_TREE: &str = concat!(
    r#"{"tag":"ul","data":{"content":"sense-groups"},"content":["#,
    r#"{"tag":"li","data":{"content":"sense-group"},"content":["#,
    r#"{"tag":"ol","content":["#,
    r#"{"tag":"li","data":{"content":"sense"},"#,
    r#""style":{"listStyleType":"\"\u2460\""},"content":["#,
    r#"{"tag":"ul","data":{"content":"glossary"},"#,
    r#""content":[{"tag":"li","content":"to eat"}]}]}]}]}]}"#,
);

/// That dictionary's own `styles.css`, likewise verbatim.
const SENSE_CSS: &str = "ul[data-sc-content=\"sense-groups\"] { list-style-type: \"\u{ff0a}\" }
     li[data-sc-content=\"sense-group\"] { padding-left: 0.25em }
     li[data-sc-content=\"sense\"] {
         padding-left: 0.25em;
         & ul[data-sc-content=\"glossary\"] {
             list-style-type: none;
             padding-left: 0.25em;
         }
     }";

/// A marker a whole default level from its gloss is a violation, and the
/// fixed shape of the same tree is not.
///
/// The mutation is the #19 defect exactly: the glossary list charged a full
/// [`LIST_INDENT_EM`] level *and* the two paddings its dictionary declared,
/// so the sense number stood 1.9em from the gloss where the author asked
/// for 0.5em. Moving the placed marker box left by one level reproduces
/// that geometry against the real tree and the real CSS, which is the
/// nearest a build carrying the fix can come to the render a reader of
/// あくどい saw.
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

/// The boundary: a list that declares no padding is owed its default
/// gutter, and a gap of exactly one level spends none of the tolerance.
///
/// The counterpart to the #19 rule. Jitendex's glossary list replaced the
/// default level with its own 0.25em; this one declares nothing, so the
/// level stands and the marker of the list *above* it hangs a full 1.4em
/// from the gloss - which is what a browser draws and what Yomitan's own
/// `--list-padding1` puts there. Asserted at zero slack, so the boundary is
/// the invariant's own arithmetic and not the tolerance around it.
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

/// The resolved #19 shape flags nothing, and flags again the moment the
/// tolerance is tightened past the padding its dictionary declared.
///
/// The other half of the #18 sanity check, and it has to be stated as a
/// tightened tolerance rather than a tightened rule: the gap the fix leaves
/// *is* the declared 0.5em, so a checker that flagged it would be wrong
/// about a browser. What the tightening shows is that this checker measures
/// that 0.5em - the same number the ticket pinned - and would therefore
/// have reported the 1.9em a reader saw.
#[test]
fn the_fixed_gutter_re_flags_when_its_slack_is_tightened_past_the_declaration() {
    let (doc, s) = swept(&sc(SENSE_TREE), SENSE_CSS);

    let strict = Thresholds { marker_gap_slack_em: -0.25, ..Thresholds::DEFAULT };
    let found = marker_gap("Fixture", &doc, &s, strict);

    assert_eq!(1, found.violations.len(), "tightened past the fix, the same gap is flagged");
    assert_eq!(
        Some(&format!("{:.2}", 0.5 * BOX_EM)),
        found.violations[0].measured.get("gap_px"),
        "and the number it reports is the gap the ticket pinned",
    );
}

/// The paragraph family collapses and files like the tracer invariant did.
///
/// One shape is one candidate whatever the invariant, and a candidate file
/// leads with the invariant's own name - which is what makes clearing stale
/// files safe ([`is_candidate_file`]). An invariant added without a name in
/// [`Invariant::ALL`] would leave its files behind after a run that stopped
/// flagging them, so the name is asserted from both ends.
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

/// The sweep. Local only, and the effort's primary target.
///
/// ```text
/// CHIBIPOP_SWEEP_CORPUS=~/.local/share/chibipop/library \
///   cargo test -p chibipop --lib corpus_render_sweep -- --ignored --nocapture
/// ```
///
/// `--nocapture` because the run summary is the point; `CHIBIPOP_SWEEP_ROWS`
/// caps rows per dictionary while iterating on an invariant, and
/// `CHIBIPOP_SWEEP_OUT` moves the candidate files. `#[ignore]`d rather than
/// skipped-when-unset so a normal `cargo test` counts it as ignored and says
/// so, instead of passing a test that did nothing.
///
/// The committed suppression list is loaded before the first archive, and a
/// list that will not parse stops the run: a sweep that quietly swept with
/// no exemptions would re-present every shape already adjudicated, and the
/// adjudicator would have no way to tell.
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

    let mut report = Report { suppress, ..Report::default() };
    for (i, zip) in archives.iter().enumerate() {
        sweep_archive(zip, i as i64 + 1, cap, &mut report);
    }

    println!("{}", report.summary());
    let out = candidate_dir();
    let written = report.write(&out).unwrap_or_else(|e| panic!("writing candidates: {e}"));
    println!("sweep  wrote {} candidate files to {}", written.len(), out.display());
}

/// The committed proof that the sweep machinery works, corpus or no corpus.
///
/// Runs in CI, sweeps the three-row fixture archive under a row cap, and
/// asserts the whole pipeline: the cap bounds the rows, unset sweeps them
/// all, two runs produce the same candidate set, and every candidate reaches
/// disk as readable JSON carrying its count and its exemplar. Zero candidates
/// is a pass - the fixture is not a defect, and a test that demanded one
/// would pin the sweep to whatever the fixture happens to render today.
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
    // Written twice, the directory holds one run's candidates and not two.
    let twice = whole.write(&out).expect("writing candidates again");
    let mine = std::fs::read_dir(&out)
        .expect("the candidate directory")
        .filter_map(std::result::Result::ok)
        .filter(|e| is_candidate_file(&e.path()))
        .count();
    assert_eq!(twice.len(), mine, "a rerun replaces its own output");
    std::fs::remove_dir_all(&out).expect("cleaning up");
}

/// The awkward archive: ruby, images, SVG, and assets the store cannot read.
///
/// `terms.zip` is three tidy rows. This one is every media shape the census
/// found, including a row whose only content is an image and which therefore
/// renders no text at all - so it proves the sweep survives the shapes that
/// would panic a walk that assumed text, and that the build's own
/// `renders_text` gate keeps a row no reader can hover out of the entry count.
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

/// The committed format: one shape signature, one line saying why the shape
/// is not a bug.
///
/// Every other case here is a defect in a *committed* file, so each is an
/// error and not a warning. An entry with no reason exempts a shape nobody
/// can review, a duplicate silently drops one of two reasons a reviewer
/// approved, and a misspelled key does both at once.
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

/// An exemption the sweep cannot act on is named, never obeyed in silence.
///
/// The two ways a committed entry rots. *Unknown* is a signature this build
/// could not have produced, so nothing will ever match it. *Unused* is a
/// signature it could have produced and did not, which is how an exemption
/// outlives the defect it forgave - and a run that never said so would let
/// the list only grow.
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
    assert!(full.contains("entries=2  absorbed=0  unused=1  unknown=1"), "{full}");

    // A partial run left rows unread, so it has no standing to call an entry
    // unused. An unknown one is unknown however much a run read.
    let partial = Report { suppress: list(), partial: true, ..Report::default() }.summary();
    assert!(!partial.contains("UNUSED"), "{partial}");
    assert!(partial.contains("unused=0  unknown=1"), "{partial}");
    assert!(verdict_for(&partial, "dropped-glyph |").contains("UNKNOWN"), "{partial}");
}

/// The summary's own line for one suppression entry.
///
/// Both suppression tests read a verdict off the printed report rather than
/// off a field, because the report is what a reviewer audits and a count no
/// run prints is a count nobody checks.
fn verdict_for<'a>(summary: &'a str, needle: &str) -> &'a str {
    summary
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line for {needle} in:\n{summary}"))
}

/// The committed list is readable by the build that reads it.
///
/// It is edited by hand between sweeps, and the next sweep is the only thing
/// that would otherwise notice a typo - hours into a corpus run, or worse,
/// never. This is the one thing CI can check about a file whose entries are
/// about dictionaries CI has never seen.
#[test]
fn the_committed_suppression_list_parses() {
    let path = suppression_file();
    Suppressions::load(&path).unwrap_or_else(|e| panic!("{e:#}"));
}

/// The whole suppression path over a real archive: one shape absorbed, one
/// shape still a candidate.
///
/// `sweep.zip` renders three rows into two distinct violating shapes, and
/// `sweep-suppressions.toml` exempts one of them and names a third shape no
/// row produces. So one run shows every half of the rule: an exemption that
/// absorbs and reports its count, a violation that still reaches a candidate
/// file, and an exemption that absorbed nothing and is called out for it.
#[test]
fn a_suppressed_fixture_shape_absorbs_its_violations_and_writes_no_candidate() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yomitan");
    let zip = dir.join("sweep.zip");
    let suppressed = "dropped-text | FixtureSweep | content > div > ruby > rt > text";
    let open = "dropped-text | FixtureSweep | content > div > ruby[data-sc-content] > rt > text";
    let never = "dropped-text | FixtureSweep | content > div > ruby > rt[data-sc-never] > text";
    let list = || {
        Suppressions::load(&dir.join("sweep-suppressions.toml")).expect("the fixture list")
    };

    let mut report = Report { suppress: list(), ..Report::default() };
    sweep_archive(&zip, 1, None, &mut report);

    let d = &report.dicts[0];
    assert_eq!("FixtureSweep", d.dictionary);
    assert_eq!(0, d.errors, "no fixture row panics");
    assert_eq!(3, d.entries, "one reading over a base, two with none");
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
    assert!(summary.contains("entries=2  absorbed=1  unused=1  unknown=0"), "{summary}");

    // On disk: the shape awaiting adjudication, and nothing for the one a
    // human already decided about.
    let out = std::env::temp_dir()
        .join(format!("chibipop-sweep-suppress-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    let written = report.write(&out).expect("writing candidates");
    assert_eq!(1, written.len(), "a suppressed shape produces no candidate file");
    let text = std::fs::read_to_string(&written[0]).expect("the candidate file");
    let json: serde_json::Value = serde_json::from_str(&text).expect("readable JSON");
    assert_eq!(open, json["signature"], "{text}");
    std::fs::remove_dir_all(&out).expect("cleaning up");

    // A capped run read one row, so the shape it never reached is not unused.
    let mut capped = Report { suppress: list(), ..Report::default() };
    sweep_archive(&zip, 1, Some(1), &mut capped);
    let capped = capped.summary();
    assert!(!capped.contains("UNUSED"), "{capped}");
}
