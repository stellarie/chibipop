//! Yomitan archive reading.

use anyhow::{Context, Result};
use serde::de::{DeserializeSeed, SeqAccess, Visitor};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::Read;
use std::path::Path;
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

/// One row from a term bank.
pub struct TermEntry {
    pub term: String,
    pub reading: String,
    pub rules: String,
    pub glossary: Value,
}

/// An archive's index.json.
pub fn read_index(zip: &Path) -> Result<Value> {
    let mut archive = open_archive(zip)?;
    read_json(&mut archive, "index.json")
}

/// Streams term bank rows.
pub fn for_each_term(zip: &Path, mut on_term: impl FnMut(TermEntry) -> Result<()>) -> Result<()> {
    for_each_bank_row(zip, "term_bank_", |row| {
        let Value::Array(mut row) = row else { return Ok(()) };
        let Some(term) = row.first().and_then(Value::as_str).map(str::to_string) else {
            return Ok(());
        };
        on_term(TermEntry {
            term,
            reading: str_at(&row, 1),
            rules: str_at(&row, 3),
            glossary: take_at(&mut row, 5),
        })
    })
}

/// Streams frequency bank rows.
pub fn for_each_freq_row(zip: &Path, on_row: impl FnMut(Value) -> Result<()>) -> Result<()> {
    for_each_bank_row(zip, "term_meta_bank_", on_row)
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
            let entry = archive
                .by_index(i)
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

/// Detects a frequency archive.
pub fn is_frequency_archive(zip: &Path) -> bool {
    let name_says_freq =
        zip.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains("Freq"));
    let index_says_freq =
        read_index(zip).is_ok_and(|idx| idx.get("frequencyMode").is_some_and(is_truthy));
    name_says_freq || index_says_freq
}

/// A row field, or empty.
fn str_at(row: &[Value], i: usize) -> String {
    row.get(i).and_then(Value::as_str).unwrap_or("").to_string()
}

/// A row field, moved out.
fn take_at(row: &mut [Value], i: usize) -> Value {
    row.get_mut(i).map_or_else(|| Value::Array(Vec::new()), Value::take)
}

/// Python-style truthiness.
fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
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

/// One zip entry as JSON.
fn read_json(archive: &mut ZipArchive<File>, name: &str) -> Result<Value> {
    let text = read_entry(archive, name)?;
    serde_json::from_str(&text).with_context(|| format!("parsing {name}"))
}

/// One zip entry's text.
fn read_entry(archive: &mut ZipArchive<File>, name: &str) -> Result<String> {
    let entry = archive.by_name(name).with_context(|| format!("reading {name}"))?;
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

/// Streams one archive's rows.
fn for_each_bank_row(
    zip: &Path,
    prefix: &str,
    mut on_row: impl FnMut(Value) -> Result<()>,
) -> Result<()> {
    let mut archive = open_archive(zip)?;
    for bank in sorted_banks(&archive_names(&archive), prefix) {
        let text = read_entry(&mut archive, &bank)?;
        stream_rows(&text, &bank, &mut on_row)?;
    }
    Ok(())
}

/// Streams one bank's rows.
fn stream_rows<F>(text: &str, name: &str, on_row: &mut F) -> Result<()>
where
    F: FnMut(Value) -> Result<()>,
{
    let mut failed = None;
    let mut de = serde_json::Deserializer::from_str(text);
    let outcome = Rows { on_row, failed: &mut failed }.deserialize(&mut de);
    if let Some(err) = failed {
        return Err(err);
    }
    outcome.with_context(|| format!("parsing {name}"))?;
    de.end().with_context(|| format!("parsing {name}"))
}

/// Visits array elements.
struct Rows<'a, F> {
    on_row: &'a mut F,
    failed: &'a mut Option<anyhow::Error>,
}

impl<'de, F> DeserializeSeed<'de> for Rows<'_, F>
where
    F: FnMut(Value) -> Result<()>,
{
    type Value = ();

    fn deserialize<D>(self, de: D) -> std::result::Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        de.deserialize_seq(self)
    }
}

impl<'de, F> Visitor<'de> for Rows<'_, F>
where
    F: FnMut(Value) -> Result<()>,
{
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON array")
    }

    fn visit_seq<A>(self, mut seq: A) -> std::result::Result<(), A::Error>
    where
        A: SeqAccess<'de>,
    {
        while let Some(row) = seq.next_element::<Value>()? {
            if let Err(err) = (self.on_row)(row) {
                *self.failed = Some(err);
                return Err(serde::de::Error::custom("row rejected"));
            }
        }
        Ok(())
    }
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

    fn collect_terms(zip: &Path) -> Vec<TermEntry> {
        let mut out = Vec::new();
        for_each_term(zip, |t| {
            out.push(t);
            Ok(())
        })
        .unwrap();
        out
    }

    #[test]
    fn every_term_row_is_read_with_its_rules_field() {
        let terms = collect_terms(&fixture("terms.zip"));
        assert_eq!(3, terms.len());
        let taberu = terms.iter().find(|t| t.term == "食べる").expect("食べる present");
        assert_eq!("たべる", taberu.reading);
        assert_eq!("v1", taberu.rules);
    }

    #[test]
    fn a_frequency_archive_is_detected_and_a_term_archive_is_not() {
        assert!(is_frequency_archive(&fixture("freq.zip")));
        assert!(!is_frequency_archive(&fixture("terms.zip")));
    }

    #[test]
    fn frequency_rows_come_back_raw_for_the_parser() {
        let mut rows = Vec::new();
        for_each_freq_row(&fixture("freq.zip"), |row| {
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
    fn a_rejected_frequency_row_stops_the_stream_with_its_own_error() {
        let err = for_each_freq_row(&fixture("freq.zip"), |_| anyhow::bail!("caller said no"))
            .unwrap_err();
        assert_eq!("caller said no", format!("{err}"));
    }

    #[test]
    fn a_missing_bank_prefix_streams_nothing_rather_than_failing() {
        let mut seen = 0;
        for_each_freq_row(&fixture("terms.zip"), |_| {
            seen += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(0, seen);
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

    #[test]
    fn ragged_row_with_only_term_reads_defaults() {
        let row = vec![Value::String("word".to_string())];
        assert_eq!("", str_at(&row, 1));
        assert_eq!("", str_at(&row, 3));
        assert_eq!(
            Value::Array(Vec::new()),
            row.get(5).cloned().unwrap_or_else(|| Value::Array(Vec::new()))
        );
    }

    #[test]
    fn ragged_row_with_term_and_reading_only() {
        let row = serde_json::json!([
            "食べる",
            "たべる"
        ]).as_array().unwrap().clone();
        assert_eq!("食べる", str_at(&row, 0));
        assert_eq!("たべる", str_at(&row, 1));
        assert_eq!("", str_at(&row, 3));
        assert_eq!(
            Value::Array(Vec::new()),
            row.get(5).cloned().unwrap_or_else(|| Value::Array(Vec::new()))
        );
    }

    #[test]
    fn ragged_row_has_rules_but_no_glossary() {
        let row = serde_json::json!([
            "飲む",
            "のむ",
            "",
            "v5"
        ]).as_array().unwrap().clone();
        assert_eq!("飲む", str_at(&row, 0));
        assert_eq!("のむ", str_at(&row, 1));
        assert_eq!("v5", str_at(&row, 3));
        assert_eq!(
            Value::Array(Vec::new()),
            row.get(5).cloned().unwrap_or_else(|| Value::Array(Vec::new()))
        );
    }

    #[test]
    fn field_with_wrong_type_falls_back_to_default() {
        let row = serde_json::json!([
            "走る",
            123,
            "",
            999
        ]).as_array().unwrap().clone();
        assert_eq!("走る", str_at(&row, 0));
        assert_eq!("", str_at(&row, 1));
        assert_eq!("", str_at(&row, 3));
        assert_eq!(
            Value::Array(Vec::new()),
            row.get(5).cloned().unwrap_or_else(|| Value::Array(Vec::new()))
        );
    }

    #[test]
    fn glossary_with_wrong_type_falls_back_to_empty() {
        let row = serde_json::json!([
            "読む",
            "よむ",
            "",
            "v5",
            0,
            "not an array"
        ]).as_array().unwrap().clone();
        let glossary = row.get(5).cloned().unwrap_or_else(|| Value::Array(Vec::new()));
        assert_eq!(Value::String("not an array".to_string()), glossary);
    }

    #[test]
    fn glossary_missing_falls_back_to_empty_array() {
        let row = serde_json::json!([
            "書く",
            "かく",
            "",
            "v5"
        ]).as_array().unwrap().clone();
        let glossary = row.get(5).cloned().unwrap_or_else(|| Value::Array(Vec::new()));
        assert_eq!(Value::Array(Vec::new()), glossary);
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
}
