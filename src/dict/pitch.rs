//! Parses Yomitan Pitch patterns and computes mora marks.
//!
//! This module handles the parts of a Pitch pattern outside storage and paint.
//! It parses `term_meta_bank_` rows tagged `"pitch"`, reports the pitch role,
//! and computes mora data for the card header and mined note HTML.
//!
//! It follows Yomitan so each mora index matches the Dictionary index.
//! `docs/research/pitch-accent-shapes.md` quotes `getKanaMorae`,
//! `isMoraPitchHigh`, `createPronunciationText` and `_toNumberArray` from
//! the pinned revision. The rules below come from those four functions.

use crate::dict::archive;
use anyhow::Result;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// Maps a Dictionary's headword and reading to its Pitch patterns.
///
/// Keep keys ordered. Keep accents in archive order, so repeated builds produce
/// the same database and cards show accents in archive order.
///
/// Several rows can name one key. The parser merges their accents and keeps
/// one copy of each accent.
pub type PitchTable = BTreeMap<(String, String), Vec<Accent>>;

/// One Pitch pattern with its fall position and mora marks.
///
/// Keep all schema fields. This module does not paint `nasal` or `devoice`, but
/// the mark code needs them. NHK has a `nasal` or `devoice` mark in 25.8% of
/// rows. If code drops them, the schema needs another change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accent {
    pub position: Position,
    /// 1-based indices for moras with a nasal sound.
    pub nasal: Vec<u32>,
    /// 1-based indices for moras with a devoiced sound.
    pub devoice: Vec<u32>,
    /// Tags on this accent. They typically name a part of speech. The schema
    /// permits them, but neither corpus has them.
    pub tags: Vec<String>,
}

/// Stores the fall position in either form allowed by the schema.
///
/// The forms use different index origins. The integer is a **1-based** count
/// of moras before the fall. The string is a **0-based** level for each mora.
/// [`is_mora_high`] is the only code that uses this difference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Position {
    /// Number of moras before the downstep. `0` is heiban and has no fall inside
    /// the word.
    ///
    /// The census contains 511 488 accents in this form.
    Downstep(u32),
    /// One `H` or `L` per mora, in order. An optional final level states the
    /// level of the next particle.
    ///
    /// The schema permits this form, but neither corpus uses it. A construction
    /// tests it.
    Pattern(String),
}

/// One Pitch claim with an accent and its Dictionary.
///
/// A pitch read returns this value. The Dictionary uses its `dict_id`, not its
/// name. A reader shows the name, but the id identifies the Dictionary.
/// This matches the split in other read paths.
#[derive(Debug, Clone, PartialEq)]
pub struct PitchClaim {
    pub dict_id: i64,
    pub accent: Accent,
}

/// One mora in a kana reading.
///
/// A mora has one or two characters. Its index is not a character index or a
/// UTF-16 offset.
/// Store both values. Mined note HTML needs the text. The card header needs the
/// UTF-16 offset. The measurement seam addresses a run by UTF-16 unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mora<'a> {
    /// Characters that form this mora.
    pub text: &'a str,
    /// UTF-16 offset of the first unit from the start of the reading.
    pub at: u32,
    /// Number of UTF-16 units in this mora.
    pub units: u32,
}

/// One mora with the marks that an accent gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkedMora<'a> {
    pub mora: Mora<'a>,
    /// Is the pitch high on this mora?
    pub high: bool,
    /// Does the pitch fall after this mora?
    ///
    /// True when this mora is high and the next one does not have high pitch.
    /// The next one after the last mora is the particle after the word. Heiban
    /// has no fall. Odaka falls after its last mora.
    pub fall: bool,
}

/// Small kana that join the prior mora.
///
/// Yomitan's `SMALL_KANA_SET`, verbatim. Other small symbols do not join.
/// `ッ` and `ー` each form their own mora. Therefore `いっぽん` has four moras
/// and `アーム` has three.
const SMALL_KANA: &str = "ぁぃぅぇぉゃゅょゎァィゥェォャュョヮ";

/// Reports whether an archive supplies the pitch role.
///
/// Read the archive's `term_meta_bank_` rows, not its filename. One of the
/// six archives named `[Pitch]` in the census has no `term_meta_bank_` rows.
/// It stores accents as glossary text, so its name gives a false result.
///
/// Match the mode by name. A mode that is not `pitch` does not supply this role.
/// The term-meta enum has `freq`, `pitch` and `ipa`. A future Yomitan mode
/// must not gain this role.
///
/// Stop at the first pitch row. Read the whole archive only when its meta
/// banks have no pitch row. If it has no meta banks, use its central directory.
///
/// Return `false` when this build cannot open or parse the archive. An
/// unreadable archive supplies no role, as [`crate::library::roles_of`] reports.
pub fn supplies_pitch(archive: &Path) -> bool {
    archive::any_meta_row(archive, is_pitch_row).unwrap_or(false)
}

/// Loads one archive's Pitch patterns.
///
/// Use the same archive walk as the frequency loader
/// ([`archive::for_each_meta_row`]). Only the row filter and destination
/// differ. A `"freq"` row is skipped here, and a `"pitch"` row is skipped by
/// the frequency loader. An archive with both rows supplies both roles.
pub fn load_pitch(archive: &Path) -> Result<PitchTable> {
    archive::fold_meta_rows(archive, PitchTable::new(), |table, row| merge_pitch_row(table, &row))
}

/// Merges one row into a `PitchTable`.
///
/// Skip any row that this build cannot read as a pitch row. Archive data comes
/// from third parties, so a malformed row does not stop the import. The schema
/// needs the `"pitch"` tag, a string headword, an object payload, a `reading`,
/// and a `pitches` list. An accent also needs a readable `position`.
///
/// An empty `pitches` list claims no key. The schema permits this shape because
/// the field has no `minItems`. It means that the reading has no accent.
pub fn merge_pitch_row(table: &mut PitchTable, row: &Value) {
    if !is_pitch_row(row) {
        return;
    }
    let Some(row) = row.as_array() else { return };
    let Some(term) = row[0].as_str() else { return };
    let Some(payload) = row[2].as_object() else { return };
    let Some(reading) = payload.get("reading").and_then(|v| v.as_str()) else { return };
    let Some(pitches) = payload.get("pitches").and_then(|v| v.as_array()) else { return };

    let found: Vec<Accent> = pitches.iter().filter_map(parse_accent).collect();
    if found.is_empty() {
        return;
    }
    let claimed = table.entry((term.to_string(), reading.to_string())).or_default();
    for accent in found {
        // Check duplicates within one row and across rows. The census has 11 rows
        // with one accent twice in `pitches`. A Dictionary must store that accent once.
        if !claimed.contains(&accent) {
            claimed.push(accent);
        }
    }
}

/// Reports whether pitch is high at this 0-based mora index.
///
/// This follows Yomitan's `isMoraPitchHigh` and defines Position semantics.
/// It does not check the index against the reading's mora count. Two census
/// rows place the downstep past the last mora. Both render as odaka without
/// a panic.
pub fn is_mora_high(index: usize, position: &Position) -> bool {
    match position {
        // The index is 0-based and positional. A level past the string is low.
        // This lets a pattern state that the word ends high.
        Position::Pattern(levels) => levels.as_bytes().get(index) == Some(&b'H'),
        // Heiban: the first mora is low. The next particle and every later mora are high.
        Position::Downstep(0) => index > 0,
        // Atamadaka: only the first mora is high.
        Position::Downstep(1) => index < 1,
        // Moras after the first and before the downstep are high.
        Position::Downstep(fall) => index > 0 && index < *fall as usize,
    }
}

/// Splits one reading into ordered moras.
///
/// This follows Yomitan's `getKanaMorae`. A small kana joins the prior
/// mora. Every other character starts a mora. A leading small kana forms a
/// mora of its own.
pub fn morae(reading: &str) -> Vec<Mora<'_>> {
    let mut out: Vec<Mora<'_>> = Vec::new();
    let mut seen = 0u32;
    for (at, c) in reading.char_indices() {
        let units = c.len_utf16() as u32;
        let end = at + c.len_utf8();
        match out.last_mut() {
            Some(open) if SMALL_KANA.contains(c) => {
                // Extend the open mora over this character. Its bytes are adjacent.
                open.text = &reading[at - open.text.len()..end];
                open.units += units;
            }
            _ => out.push(Mora { text: &reading[at..end], at: seen, units }),
        }
        seen += units;
    }
    out
}

/// Marks each mora in a reading with the result from `position`.
///
/// The card header and mined note HTML use these marks. Both outputs therefore
/// use the same mora for the overline and the tick.
pub fn marked_morae<'a>(reading: &'a str, position: &Position) -> Vec<MarkedMora<'a>> {
    let all = morae(reading);
    all.into_iter()
        .enumerate()
        .map(|(i, mora)| {
            let high = is_mora_high(i, position);
            MarkedMora { mora, high, fall: high && !is_mora_high(i + 1, position) }
        })
        .collect()
}

/// Share the row rule with the single-pass library inspector. Role detection
/// and pitch loading must accept the same metadata rows.
pub(crate) fn is_pitch_row(row: &Value) -> bool {
    row.as_array().is_some_and(|row| row.len() >= 3 && row[1].as_str() == Some("pitch"))
}

/// Parses one accent object. Returns `None` for an unsupported object.
fn parse_accent(value: &Value) -> Option<Accent> {
    let accent = value.as_object()?;
    Some(Accent {
        position: parse_position(accent.get("position")?)?,
        nasal: mora_indices(accent.get("nasal")),
        devoice: mora_indices(accent.get("devoice")),
        tags: parse_tags(accent.get("tags")),
    })
}

/// Parses a `position` in either supported form.
fn parse_position(value: &Value) -> Option<Position> {
    if let Some(fall) = value.as_u64() {
        return u32::try_from(fall).ok().map(Position::Downstep);
    }
    let levels = value.as_str()?;
    // Accept only `^[HL]+$`. Other strings do not name a level for any mora.
    if levels.is_empty() || !levels.bytes().all(|b| b == b'H' || b == b'L') {
        return None;
    }
    Some(Position::Pattern(levels.to_string()))
}

/// Returns the 1-based mora indices from a `nasal` or `devoice` field.
///
/// This follows Yomitan's `_toNumberArray`. It accepts a scalar or a list.
/// An empty list and a missing field both return no indices.
fn mora_indices(value: Option<&Value>) -> Vec<u32> {
    let Some(value) = value else { return Vec::new() };
    match value.as_array() {
        Some(list) => list.iter().filter_map(mora_index).collect(),
        None => mora_index(value).into_iter().collect(),
    }
}

/// Returns one mora index, or `None`.
fn mora_index(value: &Value) -> Option<u32> {
    u32::try_from(value.as_u64()?).ok()
}

/// Returns the tag strings from a `tags` field.
fn parse_tags(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).map(String::from).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Parses one row into a new `PitchTable`.
    fn parsed(row: Value) -> PitchTable {
        let mut table = PitchTable::new();
        merge_pitch_row(&mut table, &row);
        table
    }

    /// Returns the accents for one key.
    fn accents(table: &PitchTable, term: &str, reading: &str) -> Vec<Accent> {
        table.get(&(term.to_string(), reading.to_string())).cloned().unwrap_or_default()
    }

    /// Returns the Position values for one key. A header row draws these values.
    fn downsteps(table: &PitchTable, term: &str, reading: &str) -> Vec<Position> {
        accents(table, term, reading).into_iter().map(|a| a.position).collect()
    }

    fn heiban() -> Position {
        Position::Downstep(0)
    }

    // ---- census payloads from docs/research/pitch-accent-shapes.md ----

    /// The corpus maximum from NHK's `不義理`: four accents, each with the same
    /// nasal marker.
    #[test]
    fn the_four_accent_row_parses_all_four_with_their_shared_nasal_marker() {
        let table = parsed(json!(["不義理", "pitch", {"reading": "ふぎり", "pitches": [{"position": 3, "devoice": [], "nasal": [2]}, {"position": 0, "devoice": [], "nasal": [2]}, {"position": 1, "devoice": [], "nasal": [2]}, {"position": 2, "nasal": [2], "devoice": []}]}]));

        let found = accents(&table, "不義理", "ふぎり");
        assert_eq!(4, found.len());
        assert!(
            found.iter().all(|a| a.nasal == vec![2]),
            "every accent carries the marker the archive wrote on it: {found:?}"
        );
        assert_eq!(
            vec![
                Position::Downstep(3),
                heiban(),
                Position::Downstep(1),
                Position::Downstep(2)
            ],
            found.into_iter().map(|a| a.position).collect::<Vec<_>>()
        );
    }

    /// 三省堂国語辞典第八番 puts its second accent in a second row. A
    /// `PitchTable` keyed by expression and reading must merge those rows.
    /// The census lists 3 614 such pairs in one Dictionary.
    #[test]
    fn two_rows_for_one_expression_and_reading_merge_rather_than_overwrite() {
        let mut table = PitchTable::new();
        merge_pitch_row(
            &mut table,
            &json!(["ああ", "pitch", {"reading": "ああ", "pitches": [{"position": 0, "devoice": [], "nasal": []}]}]),
        );
        merge_pitch_row(
            &mut table,
            &json!(["ああ", "pitch", {"reading": "ああ", "pitches": [{"devoice": [], "nasal": [], "position": 1}]}]),
        );

        assert_eq!(vec![heiban(), Position::Downstep(1)], downsteps(&table, "ああ", "ああ"));
    }

    // ---- four schema shapes absent from both corpora ----

    // ---- mora calculations ----

}
