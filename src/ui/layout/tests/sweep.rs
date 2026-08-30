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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::*;
use crate::dict::archive::{for_each_term, is_frequency_archive, read_index, read_styles_css};
use crate::dict::gloss::{plain_items, renders_text, GlossDoc, Kind, NodeId, StyleKey};
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
}

impl Invariant {
    /// Every invariant this build checks.
    ///
    /// Read only by [`is_candidate_file`], so a candidate this sweep did not
    /// write is never mistaken for a stale one of its own. An invariant added
    /// without joining this list would leave its files behind after a run
    /// that stopped flagging them.
    const ALL: [Invariant; 1] = [Invariant::DroppedText];

    fn as_str(self) -> &'static str {
        match self {
            Invariant::DroppedText => "dropped-text",
        }
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
    /// The shape signature as one line: what one candidate is keyed by, and
    /// what the filename's digest is taken over.
    fn key(&self) -> String {
        format!(
            "{} | {} | {}",
            self.invariant.as_str(),
            self.dictionary,
            self.shape.join(" > ")
        )
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
/// The string count rides along because zero violations has to be
/// distinguishable from zero checks: a walk that stopped finding text would
/// otherwise report a clean corpus, which is the one failure a sweep must
/// never be able to hide.
struct Checked {
    /// Visible strings the invariants were stated over.
    strings: u64,
    violations: Vec<Violation>,
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
    let mut checked = Checked { strings: 0, violations: Vec::new() };
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
    /// Visible strings the invariants were stated over.
    strings: u64,
    violations: u64,
    candidates: u64,
    /// Entries whose parse or layout panicked, plus a walk this archive
    /// refused: an unreadable bank file is one error for the archive.
    errors: u64,
}

/// What one sweep saw.
#[derive(Default)]
struct Report {
    dicts: Vec<DictSummary>,
    /// By [`Signature::key`], so the report is ordered and one shape is one
    /// entry however many entries showed it.
    candidates: BTreeMap<String, Candidate>,
}

impl Report {
    /// Files one violation, collapsing it into the candidate for its shape.
    fn record(&mut self, v: Violation, exemplar: impl FnOnce() -> Exemplar) -> bool {
        let key = v.signature.key();
        match self.candidates.get_mut(&key) {
            Some(c) => {
                c.occurrences += 1;
                false
            }
            None => {
                let mut exemplar = exemplar();
                exemplar.measured = v.measured;
                self.candidates
                    .insert(key, Candidate { signature: v.signature, occurrences: 1, exemplar });
                true
            }
        }
    }

    /// One line per dictionary, then the run's own.
    fn print(&self) {
        let mut total = DictSummary { dictionary: "TOTAL".into(), ..DictSummary::default() };
        for d in &self.dicts {
            println!("{}", summary_line(d));
            total.entries += d.entries;
            total.strings += d.strings;
            total.violations += d.violations;
            total.errors += d.errors;
        }
        total.candidates = self.candidates.len() as u64;
        println!("{}", summary_line(&total));
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
        "sweep  {:<40} entries={:<8} strings={:<9} violations={:<8} candidates={:<5} errors={}",
        d.dictionary, d.entries, d.strings, d.violations, d.candidates, d.errors,
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

/// One term-bank row through the whole renderer.
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
    Some(dropped_text(&r.dict, &doc, SWEEP_ROLES, &s))
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
        for v in checked.violations {
            sum.violations += 1;
            let fresh = report.record(v, || Exemplar {
                row: r.row,
                term: r.term.clone(),
                reading: r.reading.clone(),
                measured: BTreeMap::new(),
                glossary: r.glossary.clone(),
            });
            if fresh {
                sum.candidates += 1;
            }
        }
        Ok(())
    });
    if let Err(err) = walk {
        if !err.chain().any(|e| e.is::<RowCap>()) {
            sum.errors += 1;
            eprintln!("sweep  {title}: {err:#}");
        }
    }
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
        let fresh = report.record(found.violations[0].clone(), || Exemplar {
            row,
            term: term.to_string(),
            reading: term.to_string(),
            measured: BTreeMap::new(),
            glossary: glossary.clone(),
        });
        assert_eq!(row == 7, fresh, "only the first sighting of a shape is a new candidate");
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
#[test]
#[ignore = "needs a local corpus: set CHIBIPOP_SWEEP_CORPUS and run with --ignored --nocapture"]
fn corpus_render_sweep() {
    let dir = std::env::var_os(CORPUS_ENV)
        .unwrap_or_else(|| panic!("set {CORPUS_ENV} to a directory of Yomitan .zip archives"));
    let dir = PathBuf::from(dir);
    let archives = corpus_archives(&dir);
    assert!(!archives.is_empty(), "no term archives under {}", dir.display());
    let cap = row_cap();

    let mut report = Report::default();
    for (i, zip) in archives.iter().enumerate() {
        sweep_archive(zip, i as i64 + 1, cap, &mut report);
    }

    report.print();
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
