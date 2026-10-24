//! This module reads Yomitan archives.

use crate::library::{Role, Roles};
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

/// The largest single asset that the build extracts.
///
/// The declared uncompressed size comes from third-party bytes. The cap limits
/// peak RSS from a zip bomb. The census found no dictionary asset near 16 MiB.
/// Most corpus assets are gaiji of about one kilobyte. The cap rejects data above
/// the limit, including valid data.
const MAX_ASSET: u64 = 16 << 20;

/// The largest stylesheet that the build stores.
///
/// This cap uses the same safety rule as [`MAX_ASSET`]. It leaves more space than
/// the corpus needs. The largest `styles.css` has 37 048 bytes. The cap rejects
/// data above the limit, including valid data.
const MAX_STYLESHEET: u64 = 4 << 20;

/// One row from a term bank. The row borrows text from the bank when possible.
///
/// The bank already contains the three short fields. The parser borrows them unless
/// JSON escape sequences require a copy. The `glossary` field gives the callback the
/// archive bytes. The builder minifies those bytes before it stores them. The builder
/// parses each bank once and streams its rows. It parses each glossary to test for
/// visible text. On Jitendex, this saved two seconds from a six-second import.
pub struct TermRow<'a> {
    pub term: Cow<'a, str>,
    pub reading: Cow<'a, str>,
    pub rules: Cow<'a, str>,
    /// The glossary from structured content, as verbatim JSON.
    pub glossary: &'a str,
}

/// Read the `index.json` file from an archive.
pub fn read_index(zip: &Path) -> Result<Value> {
    let mut archive = open_archive(zip)?;
    read_json(&mut archive, "index.json")
}

/// Inspect all roles with one open and one metadata walk. Independent role
/// checks decompress frequency banks again just to establish that pitch is absent.
///
/// Keep roles found before a later metadata error, as the independent checks
/// do. The index must remain readable because every Dictionary depends on it.
/// The boolean reports whether all role questions succeeded. A cache must not
/// preserve a partial result after a metadata read or parse error.
pub(crate) fn read_roles(zip: &Path) -> (Roles, bool) {
    let mut roles = Roles::default();
    let Ok(mut archive) = open_archive(zip) else { return (roles, false) };
    if read_json(&mut archive, "index.json").is_err() {
        return (roles, false);
    }
    let names = archive_names(&archive);
    if names.iter().any(|name| is_bank(name, "term_bank_")) {
        roles.insert(Role::Terms);
    }
    for bank in sorted_banks(&names, "term_meta_bank_") {
        let Ok(text) = read_entry(&mut archive, &bank) else { return (roles, false) };
        let outcome = stream_rows(&text, &bank, &mut |row: Value| {
            if super::frequency::is_freq_row(&row) {
                roles.insert(Role::Frequency);
            } else if super::pitch::is_pitch_row(&row) {
                roles.insert(Role::Pitch);
            }
            Ok(!(roles.has(Role::Frequency) && roles.has(Role::Pitch)))
        });
        match outcome {
            Ok(true) => {}
            Ok(false) => break,
            Err(_) => return (roles, false),
        }
    }
    (roles, true)
}

/// Read term bank rows in bank order.
///
/// This function uses one archive and one thread. The builder uses [`TermBanks`]
/// directly because it can parallelize per-row work, but this function does not.
/// Other readers use this simpler form.
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

/// One archive's term banks in order. Read one bank at a time.
///
/// The builder uses this type to send each bank to a thread pool. A bank is
/// self-contained JSON and an independent work unit. The costly work occurs per
/// row: parse the row, parse the glossary, and test for emptiness. Each thread
/// opens its own archive handle. Each thread reads the central directory once, so
/// threads do not contend.
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

    /// Read the text of bank `i`.
    pub fn read(&mut self, i: usize) -> Result<String> {
        let name = self.names[i].clone();
        read_entry(&mut self.archive, &name)
    }
}

/// Read rows from bank text that the caller already loaded.
///
/// Skip a row when it is not an array or its first field is not a string. This
/// matches the `Value` reader's tolerance because archive data comes from a third
/// party.
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

/// Read rows from term-meta banks.
///
/// Read every row in every `term_meta_bank_` member, regardless of its mode. The
/// frequency loader keeps the `"freq"` rows. The pitch loader keeps the `"pitch"`
/// rows. An archive with both modes gets one read per loader.
pub fn for_each_meta_row(zip: &Path, mut on_row: impl FnMut(Value) -> Result<()>) -> Result<()> {
    for_each_bank_row(zip, "term_meta_bank_", &mut |row| on_row(row).map(|()| true))
        .map(|_| ())
}

/// Folds every term-meta row into one value.
pub fn fold_meta_rows<T>(
    zip: &Path,
    mut init: T,
    mut merge: impl FnMut(&mut T, Value),
) -> Result<T> {
    for_each_meta_row(zip, |row| {
        merge(&mut init, row);
        Ok(())
    })?;
    Ok(init)
}

/// Return whether any term-meta row satisfies `pred`.
///
/// Use the same walk as [`for_each_meta_row`]. Stop after the first match. This
/// makes role detection affordable. A pitch archive can answer from the first row
/// of its first bank instead of a full read of all banks. The census measured 48.7 MB
/// bank JSON across five pitch archives in one library.
///
/// Return `false` when no row matches. Return an `Err` when this build cannot open
/// or parse an archive. The caller decides what an unreadable archive supplies.
pub fn any_meta_row(zip: &Path, mut pred: impl FnMut(&Value) -> bool) -> Result<bool> {
    let mut found = false;
    for_each_bank_row(zip, "term_meta_bank_", &mut |row| {
        found = pred(&row);
        Ok(!found)
    })?;
    Ok(found)
}

/// Return the Dictionary's own `styles.css`, or `None` when it has no file.
///
/// Yomitan applies this stylesheet only to entries from the same Dictionary. This
/// stylesheet is the second source of a Dictionary's boxes. It is the only source
/// for the 13 `css-only` Dictionaries in the census
/// (`docs/research/dict-shapes.md`).
///
/// Find the file at the archive root or one directory deep. The census found both
/// forms because some archives use a wrapper folder. Prefer the shallowest file.
/// A root stylesheet then cannot lose to one in a subdirectory.
///
/// Read the stylesheet as a whole. Do not stream it. The corpus has 174 KB of CSS
/// across 14 Dictionaries. The largest file has 37 048 bytes. [`MAX_STYLESHEET`]
/// is more than one hundred times larger.
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
    // A declared size can lie, so enforce the cap against bytes that the reader actually reads.
    if raw.len() as u64 > MAX_STYLESHEET {
        return Ok(None);
    }
    // Decode with replacement on purpose. `from_utf8_lossy` replaces invalid bytes.
    // A stylesheet with invalid UTF-8 can still contain rules that a reader can use.
    // A decode error here would discard a whole Dictionary's boxes.
    let mut text = String::from_utf8_lossy(&raw).into_owned();
    if text.starts_with('\u{feff}') {
        text.remove(0);
    }
    Ok(Some(text))
}

/// Find the shallowest archive entry with this file name. Search no more than one
/// directory deep.
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

/// Read referenced assets in archive order.
///
/// Read only paths that parsed nodes name. An image Dictionary can ship every glyph
/// its author has. The census's heaviest example, 字通, names a few thousand
/// distinct gaiji across 139 138 image nodes. Do not decompress other zip entries.
///
/// Use archive order instead of caller order. Local headers follow archive order, so
/// one pass avoids a seek for each wanted path. The central directory is already in
/// memory, so an unwanted entry needs only a name comparison.
///
/// Return wanted paths with no usable bytes. A path is unusable when it is absent or
/// its archive entry exceeds [`MAX_ASSET`]. The build reports the count and writes no
/// row for each path. This activates the `alt`-text fallback.
pub fn for_each_media<'a>(
    zip: &Path,
    wanted: &'a BTreeSet<String>,
    mut on_asset: impl FnMut(&'a str, &[u8]) -> Result<()>,
) -> Result<Vec<&'a str>> {
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    // Several authored paths can normalize to one archive entry, such as `./x` and
    // `x`. Keep a list because each path form needs its own media row.
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
        // A declared size can lie, so enforce the cap against bytes that the reader actually reads.
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

/// Return an authored `path` as the archive stores it.
///
/// Yomitan resolves an image node's `path` from the archive root. Thus, `./gaiji/x.svg`
/// and `/gaiji/x.svg` select the same entry. Normalize paths only here. Keep the
/// media key's authored string verbatim, so a scene element's key and the key that
/// the build writes always agree.
fn archive_relative(path: &str) -> &str {
    let mut at = path.trim_start_matches('/');
    while let Some(rest) = at.strip_prefix("./") {
        at = rest.trim_start_matches('/');
    }
    at
}

/// Return whether this archive supplies the terms role.
///
/// Inspect its banks, not its filename. A `term_bank_` member contains glossary
/// data. An archive without that member supplies no definitions, regardless of its
/// filename. [`crate::dict::frequency::supplies_frequency`] and
/// [`crate::dict::pitch::supplies_pitch`] use the same test for the other roles.
/// All three functions follow `ARCHITECTURE.md#dictionary-and-lookup`.
///
/// The central directory answers this question. This needs one open and no
/// inflation. A present term bank supplies the role. The function does not inspect
/// the bank rows.
///
/// Return `false` when this build cannot open an archive. An unreadable archive
/// supplies no role. [`crate::library::roles_of`] returns that result for all three
/// roles.
pub fn supplies_terms(zip: &Path) -> bool {
    TermBanks::open(zip).is_ok_and(|banks| !banks.is_empty())
}

/// Open a zip archive.
fn open_archive(zip: &Path) -> Result<ZipArchive<File>> {
    let file = File::open(zip).with_context(|| format!("opening {}", zip.display()))?;
    ZipArchive::new(file).with_context(|| format!("reading zip {}", zip.display()))
}

/// Return every entry name in a zip archive.
fn archive_names(archive: &ZipArchive<File>) -> Vec<String> {
    archive.file_names().map(String::from).collect()
}

/// Read one member from an open zip without a CRC-32 check.
///
/// Most archive readers use this function for indexed members. `read_styles_css` reads
/// its entry directly and applies its own cap.
/// **Disable the check deliberately.** The census found mismatched CRC-32 values in
/// all five pitch archives. The mismatch affects 48 of their 54 members.
/// This reader returned `Invalid checksum` for every member, but Yomitan imported
/// the archives. Yomitan did not pass `checkSignature` or `checkCrc32` to its own
/// zip reader. The check did not protect users from corruption. It rejected five
/// usable Dictionaries (`docs/research/pitch-accent-shapes.md`).
///
/// A damaged archive still fails the checks that matter. A member must inflate and
/// stay under the caller's cap. A bank must parse as the JSON that its schema
/// defines. A truncated member fails all three checks.
fn member(archive: &mut ZipArchive<File>, index: usize) -> Result<ZipFile<'_, File>> {
    Ok(archive.by_index_with_options(index, ZipReadOptions::new().ignore_crc32(true))?)
}

/// Read one zip entry as JSON.
fn read_json(archive: &mut ZipArchive<File>, name: &str) -> Result<Value> {
    let text = read_entry(archive, name)?;
    serde_json::from_str(&text).with_context(|| format!("parsing {name}"))
}

/// Read one zip entry as text.
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

/// Read rows from one archive until the caller stops.
///
/// When `on_row` returns `false`, stop the walk. Return `true` only when the walk
/// reads every row. A predicate can answer from the first bank instead of a full
/// read of all sixteen.
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

/// Read rows from one bank until the caller stops.
///
/// Use a generic row type so the meta reader keeps its `Value` and the term reader
/// can borrow a [`TermRow`] directly from `text`.
/// When `on_row` returns `false`, stop the walk. Do not parse the rest of the bank
/// or the array tail. Skip the extra-byte check in this case.
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

/// Visit array elements.
///
/// Treat `failed` and `stopped` as different states. A callback error abandons the
/// walk and the read. A callback that returns `false` abandons only the walk.
/// Both states leave the array partly consumed. Serde can report either state only
/// as an error, so this type returns both through one error path.
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

/// One term-bank row, or no row that this build can read.
///
/// The parser skips a non-array row or a row whose first field is not a string.
/// A `Deserialize` implementation must return a value.
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

    /// Parse Yomitan's positional row. Its fields are term, reading, definition tags,
    /// rules, score, glossary, sequence, and term tags. Read only four fields. Parse
    /// each selected field as raw JSON first. A wrong type in `term` drops the row.
    /// A wrong type in `reading` or `rules` uses an empty value. The `glossary` keeps
    /// its raw JSON.
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

        // No term means no row. This matches the `Value` reader.
        let Some(term) = term.and_then(text_field) else { return Ok(MaybeRow(None)) };
        Ok(MaybeRow(Some(TermRow {
            term,
            reading: reading.and_then(text_field).unwrap_or(Cow::Borrowed("")),
            rules: rules.and_then(text_field).unwrap_or(Cow::Borrowed("")),
            // An absent glossary becomes an empty one, as `Value::take` did.
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

/// Return a raw field as text. Borrow it unless JSON escapes require a copy.
///
/// Return `None` for a value that is not a JSON string. A numeric `reading` then
/// loses only the reading, not the entry.
fn text_field(raw: &RawValue) -> Option<Cow<'_, str>> {
    let json = raw.get();
    if let Ok(borrowed) = serde_json::from_str::<&str>(json) {
        return Some(Cow::Borrowed(borrowed));
    }
    serde_json::from_str::<String>(json).ok().map(Cow::Owned)
}

/// Share the name rule between role detection and bank enumeration. The terms
/// check needs only existence, not an allocated and sorted bank list.
fn is_bank(name: &str, prefix: &str) -> bool {
    name.starts_with(prefix) && name.ends_with(".json")
}

/// Return matching banks in sorted order.
fn sorted_banks(names: &[String], prefix: &str) -> Vec<String> {
    let mut picked: Vec<String> =
        names.iter().filter(|n| is_bank(n, prefix)).cloned().collect();
    sort_banks(&mut picked, prefix);
    picked
}

/// Sort bank names by their numeric suffix.
fn sort_banks(names: &mut [String], prefix: &str) {
    names.sort_by_key(|n| bank_number(n, prefix));
}

/// Return the numeric key from a bank name.
fn bank_number(name: &str, prefix: &str) -> u64 {
    let rest = name.strip_prefix(prefix).unwrap_or(name);
    first_digit_run(rest).unwrap_or(0)
}

/// Return the first run of digits.
fn first_digit_run(s: &str) -> Option<u64> {
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let digits: String = s[start..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    

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

    // ---- media extraction ----

    // ---- a dictionary's own styles.css ----

}
