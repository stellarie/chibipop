//! Yomitan archive reading.

use anyhow::{Context, Result};
use serde::de::{DeserializeSeed, IgnoredAny, SeqAccess, Visitor};
use serde_json::value::RawValue;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::Read;
use std::marker::PhantomData;
use std::path::Path;
use zip::read::{ZipFile, ZipReadOptions};
use zip::ZipArchive;

const MAX_BANK: usize = 256 << 20;

/// The largest single asset the build will extract.
///
/// A declared uncompressed size is third-party bytes, so the cap is what
/// keeps a zip bomb from deciding the build's peak RSS. 16 MiB is far above
/// any dictionary asset the census found - the gaiji that dominate the
/// corpus are about a kilobyte each - so it refuses corruption and never
/// content.
const MAX_ASSET: u64 = 16 << 20;

/// The largest stylesheet the build will store.
///
/// Same reasoning as [`MAX_ASSET`], and a much wider margin: the corpus's
/// largest `styles.css` is 37 048 bytes, so 4 MiB refuses only corruption.
const MAX_STYLESHEET: u64 = 4 << 20;

/// One row from a term bank, borrowed from the bank's own text.
///
/// Nothing here is copied that the bank did not already spell out: the
/// three short fields borrow unless the JSON escaped them, and `glossary`
/// is the archive's own bytes. The builder stores those bytes, so a term
/// row costs one parse of the bank and no round trip through a
/// `serde_json::Value` tree and a serializer - which on jitendex is two of
/// every six seconds an import used to spend.
pub struct TermRow<'a> {
    pub term: Cow<'a, str>,
    pub reading: Cow<'a, str>,
    pub rules: Cow<'a, str>,
    /// The structured-content glossary, verbatim JSON.
    pub glossary: &'a str,
}

/// An archive's index.json.
pub fn read_index(zip: &Path) -> Result<Value> {
    let mut archive = open_archive(zip)?;
    read_json(&mut archive, "index.json")
}

/// Streams term bank rows.
///
/// One archive, one thread, banks in order. The builder drives
/// [`TermBanks`] directly instead, because the per-row work parallelises
/// and this does not; this is the shape every other reader wants.
pub fn for_each_term(
    zip: &Path,
    mut on_term: impl FnMut(TermRow) -> Result<()>,
) -> Result<()> {
    let mut banks = TermBanks::open(zip)?;
    for i in 0..banks.len() {
        let text = banks.read(i)?;
        for_each_row(&text, banks.name(i), &mut on_term)?;
    }
    Ok(())
}

/// One archive's term banks, in order, readable one at a time.
///
/// Split out of [`for_each_term`] so the builder can hand banks to a pool
/// of threads: a bank is the natural unit of parallel work, because it is
/// self-contained JSON and the expensive part of an import - the row
/// parse, the glossary parse, the emptiness test - is per row and shares
/// nothing. Each thread opens its own handle, so the central directory is
/// read once per thread and never contended.
pub struct TermBanks {
    archive: ZipArchive<File>,
    names: Vec<String>,
}

impl TermBanks {
    pub fn open(zip: &Path) -> Result<TermBanks> {
        let archive = open_archive(zip)?;
        let names = sorted_banks(&archive_names(&archive), "term_bank_");
        Ok(TermBanks { archive, names })
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub fn name(&self, i: usize) -> &str {
        &self.names[i]
    }

    /// The `i`th bank's text.
    pub fn read(&mut self, i: usize) -> Result<String> {
        let name = self.names[i].clone();
        read_entry(&mut self.archive, &name)
    }
}

/// Streams one already-read bank's rows.
///
/// A row that is not an array, or whose first field is not a string, is
/// skipped - the same tolerance the `Value` reader had, for the same
/// reason: an archive is third-party bytes.
pub fn for_each_row(
    text: &str,
    name: &str,
    mut on_row: impl FnMut(TermRow) -> Result<()>,
) -> Result<()> {
    stream_rows(text, name, &mut |row: MaybeRow| match row.0 {
        Some(row) => on_row(row).map(|()| true),
        None => Ok(true),
    })
    .map(|_| ())
}

/// Streams term-meta bank rows.
///
/// Every row of every `term_meta_bank_`, whatever mode it is tagged with:
/// the frequency loader keeps the `"freq"` rows and the pitch loader keeps
/// the `"pitch"` ones, and an archive carrying both is read once by each.
pub fn for_each_meta_row(zip: &Path, mut on_row: impl FnMut(Value) -> Result<()>) -> Result<()> {
    for_each_bank_row(zip, "term_meta_bank_", &mut |row| on_row(row).map(|()| true))
        .map(|_| ())
}

/// Does any term-meta row satisfy `pred`?
///
/// The same walk [`for_each_meta_row`] takes, stopped at the first row that
/// answers yes - which is what makes a role predicate affordable. A pitch
/// archive answers from the first row of its first bank instead of being
/// read whole: ticket 01's census measured 48.7 MB of bank JSON across the
/// five pitch archives in one library.
///
/// `false` for an archive with no matching row, and an `Err` for one this
/// build cannot open or parse - the caller decides what an unreadable
/// archive supplies.
pub fn any_meta_row(zip: &Path, mut pred: impl FnMut(&Value) -> bool) -> Result<bool> {
    let mut found = false;
    for_each_bank_row(zip, "term_meta_bank_", &mut |row| {
        found = pred(&row);
        Ok(!found)
    })?;
    Ok(found)
}

/// The dictionary's own `styles.css`, or `None` when it ships none.
///
/// Yomitan scopes such a stylesheet to that dictionary's own entries, and it
/// is the second place a dictionary draws a box - the only place, for the 13
/// `css-only` dictionaries the census found
/// (`docs/research/dict-shapes.md`).
///
/// Root, or one directory deep: the census found it in both places, because
/// some archives are zipped with a wrapper folder. The shallowest wins, so a
/// root stylesheet is never shadowed by one inside a subdirectory.
///
/// Read whole or not at all, and never streamed: the whole corpus carries
/// 174 KB of CSS across 14 dictionaries, and 173 964 of those bytes are what
/// [`MAX_STYLESHEET`] is a hundredfold above.
pub fn read_styles_css(zip: &Path) -> Result<Option<String>> {
    let mut archive = open_archive(zip)?;
    let names = archive_names(&archive);
    let Some(name) = shallowest(&names, "styles.css") else { return Ok(None) };
    let entry = archive
        .by_name(name)
        .with_context(|| format!("reading {name} from {}", zip.display()))?;
    if entry.size() > MAX_STYLESHEET {
        return Ok(None);
    }
    let mut raw = Vec::new();
    entry
        .take(MAX_STYLESHEET + 1)
        .read_to_end(&mut raw)
        .with_context(|| format!("reading {name} from {}", zip.display()))?;
    // A lying declared size is the zip-bomb shape, so the cap is enforced
    // against what was actually read as well.
    if raw.len() as u64 > MAX_STYLESHEET {
        return Ok(None);
    }
    // Lossy, and deliberately: a stylesheet that is not clean UTF-8 still
    // holds readable rules, and the compiler drops what it cannot read. A
    // decode error here would cost a whole dictionary its boxes.
    let mut text = String::from_utf8_lossy(&raw).into_owned();
    if text.starts_with('\u{feff}') {
        text.remove(0);
    }
    Ok(Some(text))
}

/// The shallowest archive entry with this file name, at most one directory
/// deep.
fn shallowest<'a>(names: &'a [String], want: &str) -> Option<&'a str> {
    let mut best: Option<(usize, &'a str)> = None;
    for name in names {
        let flat = name.replace('\\', "/");
        if flat.rsplit('/').next() != Some(want) {
            continue;
        }
        let depth = flat.trim_matches('/').matches('/').count();
        if depth > 1 {
            continue;
        }
        if best.is_none_or(|(d, _)| depth < d) {
            best = Some((depth, name.as_str()));
        }
    }
    best.map(|(_, name)| name)
}

/// Reads the referenced assets out of an archive, in archive order.
///
/// Only the referenced ones: an image dictionary ships every glyph its
/// author had, and the census's heaviest, 字通, references a few thousand
/// distinct gaiji across 139 138 image nodes. So the caller hands over the
/// set its parsed trees actually named, and everything else in the zip is
/// never decompressed.
///
/// Archive order rather than the caller's, because the local headers are
/// laid out in it: one pass, in the order the file stores its entries,
/// instead of a seek per wanted path. The central directory is already in
/// memory, so skipping an unwanted entry costs a name comparison.
///
/// Returns the wanted paths this archive holds no usable bytes for -
/// absent, or over [`MAX_ASSET`]. The build reports the count and writes no
/// row for them, which is what makes ticket 12's `alt`-text fallback fire.
pub fn for_each_media<'a>(
    zip: &Path,
    wanted: &'a BTreeSet<String>,
    mut on_asset: impl FnMut(&'a str, &[u8]) -> Result<()>,
) -> Result<Vec<&'a str>> {
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    // Several authored paths can normalise to one archive entry (`./x` and
    // `x`), and each still deserves its own media row, so the value is a
    // list rather than a single path.
    let mut by_entry: HashMap<&'a str, Vec<&'a str>> = HashMap::new();
    for path in wanted {
        by_entry.entry(archive_relative(path)).or_default().push(path);
    }

    let mut archive = open_archive(zip)?;
    let names = archive_names(&archive);
    let mut buf = Vec::new();
    let mut unusable: Vec<&'a str> = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let Some(paths) = by_entry.remove(archive_relative(name)) else {
            continue;
        };
        {
            let entry = member(&mut archive, i)
                .with_context(|| format!("reading {name} from {}", zip.display()))?;
            if entry.size() > MAX_ASSET {
                unusable.extend(paths);
                continue;
            }
            buf.clear();
            entry
                .take(MAX_ASSET + 1)
                .read_to_end(&mut buf)
                .with_context(|| format!("reading {name} from {}", zip.display()))?;
        }
        // A lying declared size is the zip-bomb shape, so the cap is
        // enforced against what was actually read as well.
        if buf.len() as u64 > MAX_ASSET {
            unusable.extend(paths);
            continue;
        }
        for path in paths {
            on_asset(path, &buf)?;
        }
    }
    unusable.extend(by_entry.into_values().flatten());
    unusable.sort_unstable();
    Ok(unusable)
}

/// An authored `path` as the archive stores it.
///
/// Yomitan resolves an image node's `path` against the archive root, and a
/// dictionary that writes `./gaiji/x.svg` or `/gaiji/x.svg` means the same
/// entry. Normalising here and only here is deliberate: the media key keeps
/// the authored string verbatim, so the key a scene element carries and the
/// key the build wrote can never disagree.
fn archive_relative(path: &str) -> &str {
    let mut at = path.trim_start_matches('/');
    while let Some(rest) = at.strip_prefix("./") {
        at = rest.trim_start_matches('/');
    }
    at
}

/// Does this archive supply the terms role?
///
/// Its own banks, never its filename: a `term_bank_` member is what a
/// glossary lives in, and an archive that ships none supplies no
/// definitions whatever it is called. The same question
/// [`crate::dict::frequency::supplies_frequency`] and
/// [`crate::dict::pitch::supplies_pitch`] answer for the other two roles,
/// asked the same way (ADR-0014).
///
/// The central directory answers it, so this costs one open and no
/// inflation: a term bank is present or it is not, and a bank member with
/// no readable row in it is a broken archive rather than a different role.
///
/// `false` for an archive this build cannot open - unreadable supplies no
/// role at all, which is the answer [`crate::library::roles_of`] gives such
/// an archive for every one of the three.
pub fn supplies_terms(zip: &Path) -> bool {
    TermBanks::open(zip).is_ok_and(|banks| !banks.is_empty())
}

/// Opens a zip for reading.
fn open_archive(zip: &Path) -> Result<ZipArchive<File>> {
    let file = File::open(zip).with_context(|| format!("opening {}", zip.display()))?;
    ZipArchive::new(file).with_context(|| format!("reading zip {}", zip.display()))
}

/// Every entry name in a zip.
fn archive_names(archive: &ZipArchive<File>) -> Vec<String> {
    archive.file_names().map(String::from).collect()
}

/// One member of an open zip, read without checking its CRC-32.
///
/// The one place a member's bytes are reached, so the reasoning lives once.
/// **The check is off deliberately.** Every one of the five pitch archives
/// ticket 01 censused stores CRC-32 values that do not match its own
/// payload - 48 of their 54 members - and this reader refused every one of
/// them with `Invalid checksum` while Yomitan imported them cleanly, never
/// passing `checkSignature` or `checkCrc32` to its own zip reader. So the
/// check was not defending a user against corruption; it was refusing five
/// working dictionaries (`docs/research/pitch-accent-shapes.md`).
///
/// What still holds is what actually catches a damaged archive: a member
/// has to inflate, it has to stay under the caller's own cap, and a bank
/// has to parse as the JSON its schema says. A truncated member fails all
/// three.
fn member(archive: &mut ZipArchive<File>, index: usize) -> Result<ZipFile<'_, File>> {
    Ok(archive.by_index_with_options(index, ZipReadOptions::new().ignore_crc32(true))?)
}

/// One zip entry as JSON.
fn read_json(archive: &mut ZipArchive<File>, name: &str) -> Result<Value> {
    let text = read_entry(archive, name)?;
    serde_json::from_str(&text).with_context(|| format!("parsing {name}"))
}

/// One zip entry's text.
fn read_entry(archive: &mut ZipArchive<File>, name: &str) -> Result<String> {
    let index = archive
        .index_for_name(name)
        .with_context(|| format!("reading {name}: no such entry"))?;
    let entry = member(archive, index).with_context(|| format!("reading {name}"))?;
    let mut buf = Vec::new();
    entry
        .take(MAX_BANK as u64 + 1)
        .read_to_end(&mut buf)
        .with_context(|| format!("reading {name}"))?;
    if buf.len() > MAX_BANK {
        anyhow::bail!("{name}: over {MAX_BANK} bytes");
    }
    String::from_utf8(buf).with_context(|| format!("decoding {name}"))
}

/// Streams one archive's rows, until a caller says stop.
///
/// `on_row` returning `false` ends the walk, and the return value says
/// whether every row was read: a predicate over the rows can answer from
/// the first bank instead of inflating all sixteen of them.
fn for_each_bank_row(
    zip: &Path,
    prefix: &str,
    on_row: &mut impl FnMut(Value) -> Result<bool>,
) -> Result<bool> {
    let mut archive = open_archive(zip)?;
    for bank in sorted_banks(&archive_names(&archive), prefix) {
        let text = read_entry(&mut archive, &bank)?;
        if !stream_rows(&text, &bank, on_row)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Streams one bank's rows, until a caller says stop.
///
/// Generic in the row type so the meta reader keeps its `Value` and the
/// term reader can take a [`TermRow`] that borrows straight out of `text`.
/// `false` means `on_row` stopped the walk, in which case the rest of the
/// bank is never parsed - and neither is the array's own tail, so the
/// trailing-bytes check is skipped along with it.
fn stream_rows<'de, T, F>(text: &'de str, name: &str, on_row: &mut F) -> Result<bool>
where
    T: serde::Deserialize<'de>,
    F: FnMut(T) -> Result<bool>,
{
    let mut failed = None;
    let mut stopped = false;
    let mut de = serde_json::Deserializer::from_str(text);
    let outcome = Rows { on_row, failed: &mut failed, stopped: &mut stopped, row: PhantomData }
        .deserialize(&mut de);
    if let Some(err) = failed {
        return Err(err);
    }
    if stopped {
        return Ok(false);
    }
    outcome.with_context(|| format!("parsing {name}"))?;
    de.end().with_context(|| format!("parsing {name}"))?;
    Ok(true)
}

/// Visits array elements.
///
/// `failed` and `stopped` are two different things and the difference is
/// the caller's answer: a callback that returned an error abandons the walk
/// *and* the read, and one that returned `false` abandons only the walk.
/// Both leave the array part-consumed, which serde can only express as an
/// error, so both come back out through one.
struct Rows<'a, T, F> {
    on_row: &'a mut F,
    failed: &'a mut Option<anyhow::Error>,
    stopped: &'a mut bool,
    row: PhantomData<fn() -> T>,
}

impl<'de, T, F> DeserializeSeed<'de> for Rows<'_, T, F>
where
    T: serde::Deserialize<'de>,
    F: FnMut(T) -> Result<bool>,
{
    type Value = ();

    fn deserialize<D>(self, de: D) -> std::result::Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        de.deserialize_seq(self)
    }
}

impl<'de, T, F> Visitor<'de> for Rows<'_, T, F>
where
    T: serde::Deserialize<'de>,
    F: FnMut(T) -> Result<bool>,
{
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON array")
    }

    fn visit_seq<A>(self, mut seq: A) -> std::result::Result<(), A::Error>
    where
        A: SeqAccess<'de>,
    {
        while let Some(row) = seq.next_element::<T>()? {
            match (self.on_row)(row) {
                Ok(true) => {}
                Ok(false) => {
                    *self.stopped = true;
                    return Err(serde::de::Error::custom("walk stopped"));
                }
                Err(err) => {
                    *self.failed = Some(err);
                    return Err(serde::de::Error::custom("row rejected"));
                }
            }
        }
        Ok(())
    }
}

/// One term-bank row, or nothing this build can read as one.
///
/// The wrapper exists because a row that is not an array, or whose first
/// field is not a string, is *skipped* rather than fatal - and a
/// `Deserialize` impl has to return something.
struct MaybeRow<'a>(Option<TermRow<'a>>);

impl<'de> serde::Deserialize<'de> for MaybeRow<'de> {
    fn deserialize<D>(de: D) -> std::result::Result<MaybeRow<'de>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        de.deserialize_any(RowVisitor)
    }
}

struct RowVisitor;

impl<'de> Visitor<'de> for RowVisitor {
    type Value = MaybeRow<'de>;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a term-bank row")
    }

    /// Yomitan's positional row: term, reading, definition tags, rules,
    /// score, glossary, sequence, term tags. Only four are read, and every
    /// one is captured as raw JSON first so that a field of the wrong type
    /// costs that field and never the row.
    fn visit_seq<A>(self, mut seq: A) -> std::result::Result<MaybeRow<'de>, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let term: Option<&'de RawValue> = seq.next_element()?;
        let reading: Option<&'de RawValue> = seq.next_element()?;
        seq.next_element::<IgnoredAny>()?;
        let rules: Option<&'de RawValue> = seq.next_element()?;
        seq.next_element::<IgnoredAny>()?;
        let glossary: Option<&'de RawValue> = seq.next_element()?;
        while seq.next_element::<IgnoredAny>()?.is_some() {}

        // No term, no row - exactly what the `Value` reader did.
        let Some(term) = term.and_then(text_field) else { return Ok(MaybeRow(None)) };
        Ok(MaybeRow(Some(TermRow {
            term,
            reading: reading.and_then(text_field).unwrap_or(Cow::Borrowed("")),
            rules: rules.and_then(text_field).unwrap_or(Cow::Borrowed("")),
            // An absent glossary is an empty one, as `Value::take` made it.
            glossary: glossary.map_or("[]", RawValue::get),
        })))
    }

    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<MaybeRow<'de>, E> {
        Ok(MaybeRow(None))
    }

    fn visit_bool<E: serde::de::Error>(self, _: bool) -> std::result::Result<MaybeRow<'de>, E> {
        Ok(MaybeRow(None))
    }

    fn visit_i64<E: serde::de::Error>(self, _: i64) -> std::result::Result<MaybeRow<'de>, E> {
        Ok(MaybeRow(None))
    }

    fn visit_u64<E: serde::de::Error>(self, _: u64) -> std::result::Result<MaybeRow<'de>, E> {
        Ok(MaybeRow(None))
    }

    fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<MaybeRow<'de>, E> {
        Ok(MaybeRow(None))
    }

    fn visit_str<E: serde::de::Error>(self, _: &str) -> std::result::Result<MaybeRow<'de>, E> {
        Ok(MaybeRow(None))
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<MaybeRow<'de>, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(MaybeRow(None))
    }
}

/// A raw field as text, borrowed unless the JSON escaped it.
///
/// `None` for anything that is not a JSON string, which is what makes a
/// numeric `reading` cost the reading and not the entry.
fn text_field(raw: &RawValue) -> Option<Cow<'_, str>> {
    let json = raw.get();
    if let Ok(borrowed) = serde_json::from_str::<&str>(json) {
        return Some(Cow::Borrowed(borrowed));
    }
    serde_json::from_str::<String>(json).ok().map(Cow::Owned)
}

/// Matching banks, sorted.
fn sorted_banks(names: &[String], prefix: &str) -> Vec<String> {
    let mut picked: Vec<String> =
        names.iter().filter(|n| n.starts_with(prefix) && n.ends_with(".json")).cloned().collect();
    sort_banks(&mut picked, prefix);
    picked
}

/// Sorts bank names numerically.
fn sort_banks(names: &mut [String], prefix: &str) {
    names.sort_by_key(|n| bank_number(n, prefix));
}

/// A bank name's numeric key.
fn bank_number(name: &str, prefix: &str) -> u64 {
    let rest = name.strip_prefix(prefix).unwrap_or(name);
    first_digit_run(rest).unwrap_or(0)
}

/// First run of digits.
fn first_digit_run(s: &str) -> Option<u64> {
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let digits: String = s[start..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yomitan").join(name)
    }

    #[test]
    fn the_index_carries_the_dictionary_title() {
        let idx = read_index(&fixture("terms.zip")).unwrap();
        assert_eq!(Some("FixtureTerms"), idx["title"].as_str());
    }

    #[test]
    fn every_term_row_is_read_with_its_rules_field() {
        let mut terms = Vec::new();
        for_each_term(&fixture("terms.zip"), |t| {
            terms.push((t.term.into_owned(), t.reading.into_owned(), t.rules.into_owned()));
            Ok(())
        })
        .unwrap();
        assert_eq!(3, terms.len());
        let taberu = terms.iter().find(|t| t.0 == "食べる").expect("食べる present");
        assert_eq!("たべる", taberu.1);
        assert_eq!("v1", taberu.2);
    }

    /// The role a filename cannot answer: `freq.zip` and `pitch.zip` carry
    /// nothing but a term-meta bank, so neither supplies definitions, and
    /// `terms.zip` does whatever its name says.
    #[test]
    fn the_terms_role_is_read_from_the_banks_and_not_from_the_name() {
        assert!(supplies_terms(&fixture("terms.zip")));
        assert!(supplies_terms(&fixture("both.zip")));
        assert!(!supplies_terms(&fixture("freq.zip")));
        assert!(!supplies_terms(&fixture("pitch.zip")));
    }

    #[test]
    fn term_meta_rows_come_back_raw_for_the_parser() {
        let mut rows = Vec::new();
        for_each_meta_row(&fixture("freq.zip"), |row| {
            rows.push(row);
            Ok(())
        })
        .unwrap();
        assert_eq!(3, rows.len());
        assert!(rows.iter().all(|r| r.is_array()));
    }

    #[test]
    fn a_rejected_term_row_stops_the_stream_with_its_own_error() {
        let mut seen = 0;
        let err = for_each_term(&fixture("terms.zip"), |_| {
            seen += 1;
            anyhow::bail!("caller said no")
        })
        .unwrap_err();
        assert_eq!(1, seen, "stopped at the first row");
        assert_eq!("caller said no", format!("{err}"));
    }

    #[test]
    fn a_rejected_term_meta_row_stops_the_stream_with_its_own_error() {
        let err = for_each_meta_row(&fixture("freq.zip"), |_| anyhow::bail!("caller said no"))
            .unwrap_err();
        assert_eq!("caller said no", format!("{err}"));
    }

    #[test]
    fn a_missing_bank_prefix_streams_nothing_rather_than_failing() {
        let mut seen = 0;
        for_each_meta_row(&fixture("terms.zip"), |_| {
            seen += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(0, seen);
    }

    /// The whole point of the stop: a predicate over the rows must not
    /// inflate every bank of an archive to answer from the first row.
    #[test]
    fn a_satisfied_predicate_stops_the_walk_at_the_row_that_answered_it() {
        let mut seen = 0;
        let found = any_meta_row(&fixture("freq.zip"), |row| {
            seen += 1;
            row[0].as_str() == Some("食べる")
        })
        .unwrap();

        assert!(found);
        assert_eq!(1, seen, "the fixture's first row answers, so no second row is parsed");
    }

    #[test]
    fn an_unsatisfied_predicate_reads_every_row_and_answers_no() {
        let mut seen = 0;
        let found = any_meta_row(&fixture("freq.zip"), |_| {
            seen += 1;
            false
        })
        .unwrap();

        assert!(!found);
        assert_eq!(3, seen);
    }

    /// An archive with no term-meta bank has no row to satisfy anything,
    /// and answering that costs its central directory and no inflate.
    #[test]
    fn an_archive_with_no_term_meta_bank_answers_no_to_every_predicate() {
        assert!(!any_meta_row(&fixture("terms.zip"), |_| true).unwrap());
    }

    #[test]
    fn banks_sort_numerically_so_bank_10_follows_bank_9() {
        let mut names = vec![
            "term_bank_10.json".to_string(),
            "term_bank_2.json".to_string(),
            "term_bank_1.json".to_string(),
        ];
        sort_banks(&mut names, "term_bank_");
        assert_eq!(
            vec!["term_bank_1.json", "term_bank_2.json", "term_bank_10.json"],
            names
        );
    }

    /// The four fields a row is read for, defaults included.
    fn fields(bank: &str) -> Vec<(String, String, String, String)> {
        let mut out = Vec::new();
        for_each_row(bank, "term_bank_1.json", |t| {
            out.push((
                t.term.into_owned(),
                t.reading.into_owned(),
                t.rules.into_owned(),
                t.glossary.to_string(),
            ));
            Ok(())
        })
        .unwrap();
        out
    }

    #[test]
    fn ragged_row_with_only_term_reads_defaults() {
        assert_eq!(
            vec![("word".into(), String::new(), String::new(), "[]".into())],
            fields(r#"[["word"]]"#)
        );
    }

    #[test]
    fn ragged_row_with_term_and_reading_only() {
        assert_eq!(
            vec![("食べる".into(), "たべる".into(), String::new(), "[]".into())],
            fields(r#"[["食べる","たべる"]]"#)
        );
    }

    #[test]
    fn ragged_row_has_rules_but_no_glossary() {
        assert_eq!(
            vec![("飲む".into(), "のむ".into(), "v5".into(), "[]".into())],
            fields(r#"[["飲む","のむ","","v5"]]"#)
        );
    }

    #[test]
    fn field_with_wrong_type_falls_back_to_default() {
        assert_eq!(
            vec![("走る".into(), String::new(), String::new(), "[]".into())],
            fields(r#"[["走る",123,"",999]]"#)
        );
    }

    #[test]
    fn glossary_with_wrong_type_is_kept_verbatim() {
        assert_eq!(
            vec![("読む".into(), "よむ".into(), "v5".into(), r#""not an array""#.into())],
            fields(r#"[["読む","よむ","","v5",0,"not an array"]]"#)
        );
    }

    /// A row this build cannot read as one is skipped, never fatal: an
    /// archive is third-party bytes and one bad row must not cost the
    /// dictionary.
    #[test]
    fn a_row_that_is_not_an_array_of_a_term_is_skipped() {
        assert!(fields(r#"[null,42,"x",{"a":1},[123],[]]"#).is_empty());
    }

    /// The glossary reaches the builder exactly as the archive spelled it -
    /// key order and all - because that is what the `entry` record stores.
    #[test]
    fn the_glossary_is_the_archives_own_bytes() {
        let bank = r#"[["語","ご","","",0,[{"type": "text", "text": "hi"}]]]"#;
        assert_eq!(r#"[{"type": "text", "text": "hi"}]"#, fields(bank)[0].3);
    }

    /// An escaped field cannot borrow, and must still come out decoded.
    #[test]
    fn an_escaped_field_is_unescaped() {
        assert_eq!("a\"b\nc", fields(r#"[["a\"b\nc"]]"#)[0].0);
    }

    #[test]
    fn glossary_missing_falls_back_to_empty_array() {
        assert_eq!("[]", fields(r#"[["書く","かく","","v5"]]"#)[0].3);
    }

    // ---- media extraction (ticket 03) ----

    fn media_zip() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media/media.zip")
    }

    fn wanted(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|p| (*p).to_string()).collect()
    }

    /// The whole point of taking a wanted set: an image dictionary ships
    /// every glyph its author had, and only the paths its nodes name can
    /// ever be painted.
    #[test]
    fn only_the_wanted_entries_are_read_and_the_rest_are_never_touched() {
        let want = wanted(&["gaiji/one.png", "gaiji/two.svg"]);
        let mut seen: Vec<(String, usize)> = Vec::new();
        let missing = for_each_media(&media_zip(), &want, |path, bytes| {
            seen.push((path.to_string(), bytes.len()));
            Ok(())
        })
        .unwrap();

        seen.sort();
        assert_eq!(vec!["gaiji/one.png", "gaiji/two.svg"], seen.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>());
        assert!(seen.iter().all(|(_, len)| *len > 0), "the bytes are real: {seen:?}");
        assert!(missing.is_empty());
    }

    /// A referenced path the archive does not hold is reported, not
    /// guessed at: it is what makes the build write no media row and the
    /// `alt`-text fallback fire.
    #[test]
    fn a_path_the_archive_does_not_hold_comes_back_as_unusable() {
        let want = wanted(&["gaiji/one.png", "gaiji/nope.png", "elsewhere/x.svg"]);
        let mut seen = Vec::new();
        let missing = for_each_media(&media_zip(), &want, |path, _| {
            seen.push(path.to_string());
            Ok(())
        })
        .unwrap();

        assert_eq!(vec!["gaiji/one.png"], seen);
        assert_eq!(vec!["elsewhere/x.svg", "gaiji/nope.png"], missing);
    }

    /// Yomitan resolves an image node's `path` against the archive root,
    /// so a dictionary writing `./x` or `/x` means the same entry - and
    /// each authored spelling still gets its own media row, because the
    /// key a scene element carries is the authored string.
    #[test]
    fn an_authored_path_is_matched_against_the_archive_root() {
        let want = wanted(&["./gaiji/one.png", "/gaiji/one.png", "gaiji/one.png"]);
        let mut seen = Vec::new();
        let missing = for_each_media(&media_zip(), &want, |path, _| {
            seen.push(path.to_string());
            Ok(())
        })
        .unwrap();

        seen.sort();
        assert_eq!(vec!["./gaiji/one.png", "/gaiji/one.png", "gaiji/one.png"], seen);
        assert!(missing.is_empty(), "one entry answers all three spellings");
    }

    #[test]
    fn an_empty_wanted_set_never_opens_the_archive() {
        // A path that does not exist: opening it would be an error, so a
        // build over a dictionary with no image nodes provably costs no
        // second pass over the zip.
        let nothing = BTreeSet::new();
        let missing =
            for_each_media(Path::new("/nonexistent/archive.zip"), &nothing, |_, _| Ok(()))
                .expect("no wanted paths means no work");
        assert!(missing.is_empty());
    }

    /// The peak-RSS bound. A declared uncompressed size is third-party
    /// bytes, and an archive that deflates 17 MiB of zeros into 17 KB is
    /// exactly what the cap is for: the entry is refused on its header,
    /// before anything is read, and reported like any other path with no
    /// usable bytes.
    #[test]
    fn an_asset_over_the_cap_is_refused_before_it_is_read() {
        let zip = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/media/oversized.zip");
        let want = wanted(&["gaiji/huge.png"]);
        let mut seen = Vec::new();
        let missing = for_each_media(&zip, &want, |path, _| {
            seen.push(path.to_string());
            Ok(())
        })
        .unwrap();

        assert!(seen.is_empty(), "17 MiB must never reach the caller: {seen:?}");
        assert_eq!(vec!["gaiji/huge.png"], missing);
    }

    // ---- a dictionary's own styles.css (ticket 17) ----

    /// The census found `styles.css` at the archive root and one directory
    /// deep, because some archives are zipped with a wrapper folder. Two
    /// directories deep is not a dictionary's stylesheet, and the shallowest
    /// of two candidates is the real one.
    #[test]
    fn the_stylesheet_is_found_at_the_root_or_one_directory_deep() {
        let cases: [(&[&str], Option<&str>); 7] = [
            (&["index.json", "styles.css"], Some("styles.css")),
            (&["dict/index.json", "dict/styles.css"], Some("dict/styles.css")),
            // The shallowest wins, whichever order the entries arrive in.
            (&["dict/styles.css", "styles.css"], Some("styles.css")),
            (&["styles.css", "dict/styles.css"], Some("styles.css")),
            (&["a/b/styles.css"], None),
            (&["index.json"], None),
            // A zip written on Windows separates with a backslash.
            (&["dict\\styles.css"], Some("dict\\styles.css")),
        ];
        for (names, want) in cases {
            let owned: Vec<String> = names.iter().map(|s| s.to_string()).collect();
            assert_eq!(want, shallowest(&owned, "styles.css"), "{names:?}");
        }
    }

    /// A name that merely ends in the same letters is not the file.
    #[test]
    fn a_lookalike_name_is_not_the_stylesheet() {
        let names = vec!["mystyles.css".to_string(), "styles.css.bak".to_string()];
        assert_eq!(None, shallowest(&names, "styles.css"));
    }

    /// One zip, written here, read back through the real reader. A BOM and
    /// CRLF because a hand-authored stylesheet has both, and neither may
    /// reach the compiler.
    #[test]
    fn a_stylesheet_reads_back_with_its_bom_stripped() {
        let dir = std::env::temp_dir().join("chibipop_styles_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("styled_{}.zip", std::process::id()));
        write_zip(&path, &[("wrap/index.json", b"{\"title\":\"X\"}")], "\u{feff}span { color: red }\r\n");
        let css = read_styles_css(&path).unwrap().expect("the archive ships one");
        assert_eq!("span { color: red }\r\n", css);
        let _ = std::fs::remove_file(&path);
    }

    /// An archive with none reports none, rather than failing the build.
    #[test]
    fn an_archive_without_a_stylesheet_reports_none() {
        assert_eq!(None, read_styles_css(&fixture("terms.zip")).unwrap());
    }

    fn write_zip(path: &Path, members: &[(&str, &[u8])], css: &str) {
        use std::io::Write as _;
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        for (name, bytes) in members {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.start_file("wrap/styles.css", opts).unwrap();
        zip.write_all(css.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
}
