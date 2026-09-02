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
/// unreadable archive supplies no role, as [`crate::library::kind_of`] reports.
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
    let mut table = PitchTable::new();
    archive::for_each_meta_row(archive, |row| {
        merge_pitch_row(&mut table, &row);
        Ok(())
    })?;
    Ok(table)
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

/// Returns true for a pitch row.
fn is_pitch_row(row: &Value) -> bool {
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

    /// The corpus has 48.0% heiban accents. This is the first value that a
    /// renderer must handle. The row comes from NHK, verbatim.
    #[test]
    fn a_single_heiban_accent_parses_to_downstep_zero() {
        let table =
            parsed(json!(["ああ", "pitch", {"pitches": [{"position": 0, "devoice": [], "nasal": []}], "reading": "ああ"}]));

        assert_eq!(vec![heiban()], downsteps(&table, "ああ", "ああ"));
    }

    /// A row from 新明解第八版. Its key order differs from the prior row.
    /// The parser must not depend on key order.
    #[test]
    fn a_single_atamadaka_accent_parses_to_downstep_one() {
        let table =
            parsed(json!(["あ", "pitch", {"pitches": [{"nasal": [], "devoice": [], "position": 1}], "reading": "あ"}]));

        assert_eq!(vec![Position::Downstep(1)], downsteps(&table, "あ", "あ"));
    }

    /// A row from 大辞林第四版. It has two accents for one reading.
    /// Keep their archive order.
    #[test]
    fn two_accents_in_one_row_keep_the_order_the_archive_wrote_them() {
        let table = parsed(json!(["アーカイブ", "pitch", {"reading": "アーカイブ", "pitches": [{"position": 3, "devoice": [], "nasal": []}, {"devoice": [], "nasal": [], "position": 1}]}]));

        assert_eq!(
            vec![Position::Downstep(3), Position::Downstep(1)],
            downsteps(&table, "アーカイブ", "アーカイブ")
        );
    }

    /// A row from 大辞泉 with three accents.
    #[test]
    fn three_accents_in_one_row_all_parse() {
        let table = parsed(json!(["アーカイブ", "pitch", {"reading": "アーカイブ", "pitches": [{"devoice": [], "position": 1, "nasal": []}, {"nasal": [], "position": 3, "devoice": []}, {"devoice": [], "position": 0, "nasal": []}]}]));

        assert_eq!(
            vec![Position::Downstep(1), Position::Downstep(3), heiban()],
            downsteps(&table, "アーカイブ", "アーカイブ")
        );
    }

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

    /// NHK's `合鍵` has a nasal marker on a heiban accent. The marker appears
    /// in 2.03% of corpus accents, all from NHK. This is a renderer input.
    #[test]
    fn a_nasal_marker_is_kept_as_a_one_based_mora_index() {
        let table = parsed(json!(["合鍵", "pitch", {"reading": "あいかぎ", "pitches": [{"devoice": [], "position": 0, "nasal": [4]}]}]));

        let found = accents(&table, "合鍵", "あいかぎ");
        assert_eq!(vec![4], found[0].nasal);
        assert!(found[0].devoice.is_empty(), "an empty devoice names no mora");
    }

    /// NHK's `アーク灯` puts the marker on mora three in a five-character
    /// reading. The marker is a mora index, not a character index.
    #[test]
    fn a_devoice_marker_is_kept_as_a_one_based_mora_index() {
        let table = parsed(json!(["アーク灯", "pitch", {"reading": "アークとう", "pitches": [{"nasal": [], "devoice": [3], "position": 0}]}]));

        let found = accents(&table, "アーク灯", "アークとう");
        assert_eq!(vec![3], found[0].devoice);
        assert!(found[0].nasal.is_empty(), "an empty nasal names no mora");
    }

    /// NHK's `扱い` has two accents with the same devoice marker.
    #[test]
    fn both_markers_survive_on_a_two_accent_row() {
        let table = parsed(json!(["扱い", "pitch", {"pitches": [{"devoice": [2], "nasal": [], "position": 0}, {"position": 3, "nasal": [], "devoice": [2]}], "reading": "あつかい"}]));

        let found = accents(&table, "扱い", "あつかい");
        assert_eq!(vec![heiban(), Position::Downstep(3)],
            found.iter().map(|a| a.position.clone()).collect::<Vec<_>>());
        assert!(found.iter().all(|a| a.devoice == vec![2]), "{found:?}");
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

    /// 三省堂's `あまり` uses three rows with one accent in each row.
    #[test]
    fn three_rows_for_one_reading_merge_into_three_accents() {
        let mut table = PitchTable::new();
        for row in [
            json!(["あまり", "pitch", {"reading": "あまり", "pitches": [{"nasal": [], "devoice": [], "position": 3}]}]),
            json!(["あまり", "pitch", {"reading": "あまり", "pitches": [{"devoice": [], "nasal": [], "position": 0}]}]),
            json!(["あまり", "pitch", {"reading": "あまり", "pitches": [{"devoice": [], "nasal": [], "position": 1}]}]),
        ] {
            merge_pitch_row(&mut table, &row);
        }

        assert_eq!(
            vec![Position::Downstep(3), heiban(), Position::Downstep(1)],
            downsteps(&table, "あまり", "あまり")
        );
    }

    /// 大辞泉's `一体` is one of 11 corpus rows that repeat an accent in one
    /// `pitches` list.
    #[test]
    fn one_row_repeating_an_accent_stores_it_once() {
        let table = parsed(json!(["一体", "pitch", {"reading": "いったい", "pitches": [{"position": 0, "devoice": [], "nasal": []}, {"nasal": [], "position": 1, "devoice": []}, {"nasal": [], "position": 0, "devoice": []}]}]));

        assert_eq!(vec![heiban(), Position::Downstep(1)], downsteps(&table, "一体", "いったい"));
    }

    /// NHK's `自動車損害賠償責任保険` has the longest reading and highest
    /// downstep in the corpus. Its row also has a nasal marker.
    #[test]
    fn the_longest_reading_and_highest_downstep_in_the_corpus_parse() {
        let table = parsed(json!(["自動車損害賠償責任保険", "pitch", {"pitches": [{"nasal": [7], "devoice": [], "position": 17}], "reading": "じどうしゃそんがいばいしょうせきにんほけん"}]));

        let found = accents(&table, "自動車損害賠償責任保険", "じどうしゃそんがいばいしょうせきにんほけん");
        assert_eq!(Position::Downstep(17), found[0].position);
        assert_eq!(vec![7], found[0].nasal);
        assert_eq!(
            19,
            morae("じどうしゃそんがいばいしょうせきにんほけん").len(),
            "the census's 19-mora maximum, counted the way Yomitan counts"
        );
    }

    /// 大辞林第四版's `築後` has a downstep past the last mora. The schema
    /// permits this data error. Both accents survive, and the out-of-range value
    /// renders without a panic.
    ///
    /// `isMoraPitchHigh` handles this case by its index rules. The census calls
    /// it "odaka". With three moras and downstep 5, the mora after the last is
    /// also high. No mora has a fall, so the row shows a rise without a tick.
    #[test]
    fn a_downstep_past_the_last_mora_is_stored_and_draws_without_a_tick() {
        let table = parsed(json!(["築後", "pitch", {"reading": "ちくご", "pitches": [{"nasal": [], "devoice": [], "position": 3}, {"position": 5, "devoice": [], "nasal": []}]}]));

        assert_eq!(
            vec![Position::Downstep(3), Position::Downstep(5)],
            downsteps(&table, "築後", "ちくご")
        );
        let marked = marked_morae("ちくご", &Position::Downstep(5));
        assert_eq!(vec![false, true, true], marked.iter().map(|m| m.high).collect::<Vec<_>>());
        assert!(
            marked.iter().all(|m| !m.fall),
            "the mora past the last is high too, so the pitch falls nowhere: {marked:?}"
        );
        // The in-range accent is odaka. Downstep 3 puts the tick after the last mora.
        let odaka = marked_morae("ちくご", &Position::Downstep(3));
        assert_eq!(vec![false, false, true], odaka.iter().map(|m| m.fall).collect::<Vec<_>>());
    }

    /// 三省堂's `扱い` uses a reading that no Dictionary in the terms role
    /// produces. The corpus has 122 such rows. They parse but never match a
    /// headword.
    #[test]
    fn a_reading_no_headword_will_match_still_parses() {
        for (term, reading, row) in [
            ("扱い", "〜あつかい", json!(["扱い", "pitch", {"pitches": [{"devoice": [], "nasal": [], "position": 2}], "reading": "〜あつかい"}])),
            ("或いは", "あるいは（ワ）", json!(["或いは", "pitch", {"pitches": [{"nasal": [], "devoice": [], "position": 2}], "reading": "あるいは（ワ）"}])),
            ("削がれる", "そが◦れる", json!(["削がれる", "pitch", {"reading": "そが◦れる", "pitches": [{"devoice": [], "nasal": [], "position": 3}]}])),
        ] {
            let table = parsed(row);
            assert_eq!(
                vec![Position::Downstep(if reading == "そが◦れる" { 3 } else { 2 })],
                downsteps(&table, term, reading),
                "{reading} parses under its own odd reading"
            );
        }
    }

    /// The sixth `[Pitch]`-named archive stores its accent as glossary text.
    /// Its `term_bank_` row does not reach the pitch parser. This census case
    /// shows that a term row is not a pitch row.
    #[test]
    fn a_term_bank_row_is_not_a_pitch_row() {
        let table = parsed(json!(["帯広", "おびひろ", "名詞 地名", "", 0, ["おびひろ【帯広】（北海道）\n ・［0］オビヒロ"], 0, ""]));

        assert!(table.is_empty(), "a term row carries no pitch, whatever its archive is named");
    }

    /// A `"freq"` row can share a bank with pitch rows. It belongs to the other
    /// role.
    #[test]
    fn a_freq_tagged_row_produces_an_empty_pitch_table() {
        assert!(parsed(json!(["猫", "freq", {"reading": "ねこ", "frequency": 42}])).is_empty());
        assert!(parsed(json!(["食べる", "freq", 7])).is_empty());
    }

    /// This is the other mode in the closed term-meta enum. Match by name, so a
    /// future Yomitan mode does not gain the pitch role.
    #[test]
    fn an_ipa_row_produces_an_empty_pitch_table() {
        let table = parsed(json!(["猫", "ipa", {"reading": "ねこ", "transcriptions": [{"ipa": "neko"}]}]));

        assert!(table.is_empty());
    }

    // ---- four schema shapes absent from both corpora ----

    /// The schema permits an empty `pitches` list, but the corpus has 0 of
    /// 466 990 rows with this shape. This construction names a reading with no
    /// accent. It must claim no key, or a card would show an empty row.
    #[test]
    fn an_empty_pitches_list_claims_no_key() {
        let table = parsed(json!(["れい", "pitch", {"reading": "れい", "pitches": []}]));

        assert!(table.is_empty());
    }

    /// The schema permits the `^[HL]+$` form of `position`. Neither corpus uses
    /// it. This construction tests that form.
    #[test]
    fn the_hl_string_form_of_position_parses_and_indexes_from_zero() {
        let table = parsed(json!(["れい", "pitch", {"reading": "れい", "pitches": [{"position": "LHHL"}]}]));

        assert_eq!(vec![Position::Pattern("LHHL".to_string())], downsteps(&table, "れい", "れい"));
        let pattern = Position::Pattern("LHHL".to_string());
        assert_eq!(
            vec![false, true, true, false],
            (0..4).map(|i| is_mora_high(i, &pattern)).collect::<Vec<_>>(),
            "the string form is 0-based and positional, unlike the integer"
        );
    }

    /// The schema permits scalar marker values. `_toNumberArray` normalizes
    /// them to one-element lists. This construction tests that form.
    #[test]
    fn scalar_nasal_and_devoice_markers_normalise_to_one_element_lists() {
        let table = parsed(json!(["れい", "pitch", {"reading": "れい", "pitches": [{"position": 2, "nasal": 3, "devoice": 1}]}]));

        let found = accents(&table, "れい", "れい");
        assert_eq!(vec![3], found[0].nasal);
        assert_eq!(vec![1], found[0].devoice);
    }

    /// The two corpora have 511 488 accents, and none has `tags`. The schema
    /// permits the field. This construction checks that the parser keeps it.
    #[test]
    fn a_tags_list_is_kept_verbatim() {
        let table = parsed(json!(["れい", "pitch", {"reading": "れい", "pitches": [{"position": 2, "tags": ["名"]}]}]));

        assert_eq!(vec!["名".to_string()], accents(&table, "れい", "れい")[0].tags);
    }

    /// `additionalProperties: false` means Yomitan would reject the source archive.
    /// This parser ignores the unknown key and keeps the accent.
    #[test]
    fn an_unknown_accent_key_is_ignored_and_the_accent_still_parses() {
        let table = parsed(json!(["れい", "pitch", {"reading": "れい", "pitches": [{"position": 1, "bogus": 1}]}]));

        assert_eq!(vec![Position::Downstep(1)], downsteps(&table, "れい", "れい"));
    }

    /// A `position` with another type is not an accent. The parser keeps valid
    /// accents in the same list.
    #[test]
    fn an_unreadable_position_drops_its_accent_and_keeps_the_others() {
        let table = parsed(json!(["れい", "pitch", {"reading": "れい", "pitches": [{"position": true}, {"position": "xyz"}, {"position": -1}, {"position": 2}]}]));

        assert_eq!(vec![Position::Downstep(2)], downsteps(&table, "れい", "れい"));
    }

    /// The schema needs both payload keys. A row without either key cannot
    /// identify an accent.
    #[test]
    fn a_payload_missing_its_reading_or_its_pitches_claims_no_key() {
        assert!(parsed(json!(["れい", "pitch", {"pitches": [{"position": 1}]}])).is_empty());
        assert!(parsed(json!(["れい", "pitch", {"reading": "れい"}])).is_empty());
        assert!(parsed(json!(["れい", "pitch", 3])).is_empty());
        assert!(parsed(json!(["れい", "pitch"])).is_empty());
        assert!(parsed(json!({"reading": "れい"})).is_empty());
    }

    // ---- mora calculations ----

    /// Yomitan's `SMALL_KANA_SET` joins a small kana to the prior mora.
    /// `ッ` and `ー` are not in that set, so each forms its own mora.
    /// Therefore `いっぽん` has four moras and `アーム` has three. A mora index
    /// is not a character index.
    #[test]
    fn small_kana_join_the_preceding_mora_while_sokuon_and_chouon_stand_alone() {
        let cases = [
            ("きょう", vec!["きょ", "う"]),
            ("いっぽん", vec!["い", "っ", "ぽ", "ん"]),
            ("アーム", vec!["ア", "ー", "ム"]),
            ("じどうしゃ", vec!["じ", "ど", "う", "しゃ"]),
            ("ふぎり", vec!["ふ", "ぎ", "り"]),
        ];
        for (reading, want) in cases {
            let found: Vec<&str> = morae(reading).into_iter().map(|m| m.text).collect();
            assert_eq!(want, found, "{reading}");
        }
    }

    /// A mora offset uses UTF-16 units from the start of the reading. The
    /// measurement seam uses the same coordinate for a text run.
    #[test]
    fn a_moras_offset_counts_utf16_units_and_not_characters() {
        let found = morae("きょうと");

        assert_eq!(vec![0, 2, 3], found.iter().map(|m| m.at).collect::<Vec<_>>());
        assert_eq!(vec![2, 1, 1], found.iter().map(|m| m.units).collect::<Vec<_>>());
    }

    /// A leading small kana must not cause a panic or disappear. With no prior
    /// mora, it forms a mora of its own.
    #[test]
    fn a_leading_small_kana_opens_its_own_mora() {
        let found: Vec<&str> = morae("ゃあ").into_iter().map(|m| m.text).collect();

        assert_eq!(vec!["ゃ", "あ"], found);
    }

    #[test]
    fn an_empty_reading_has_no_morae() {
        assert!(morae("").is_empty());
    }

    /// Heiban covers 48% of the corpus. The first mora is low, and every later
    /// mora is high. No fall occurs inside the word because the rise reaches the
    /// next particle.
    #[test]
    fn heiban_marks_every_mora_after_the_first_high_and_falls_nowhere() {
        let marked = marked_morae("ざつだん", &heiban());

        assert_eq!(vec![false, true, true, true], marked.iter().map(|m| m.high).collect::<Vec<_>>());
        assert!(marked.iter().all(|m| !m.fall), "heiban falls nowhere: {marked:?}");
    }

    /// Atamadaka has only the first mora high. The fall follows that mora.
    #[test]
    fn atamadaka_marks_the_first_mora_high_and_falls_after_it() {
        let marked = marked_morae("ねこ", &Position::Downstep(1));

        assert_eq!(vec![true, false], marked.iter().map(|m| m.high).collect::<Vec<_>>());
        assert_eq!(vec![true, false], marked.iter().map(|m| m.fall).collect::<Vec<_>>());
    }

    /// Nakadaka has high moras between the first mora and the downstep. The fall
    /// occurs at the downstep.
    #[test]
    fn nakadaka_marks_the_moras_before_the_downstep_high() {
        let marked = marked_morae("あつかい", &Position::Downstep(3));

        assert_eq!(vec![false, true, true, false], marked.iter().map(|m| m.high).collect::<Vec<_>>());
        assert_eq!(vec![false, false, true, false], marked.iter().map(|m| m.fall).collect::<Vec<_>>());
    }

    /// In odaka, the downstep equals the mora count. The word ends high, and the
    /// fall occurs on the next particle. The renderer draws a tick after the last
    /// mora.
    #[test]
    fn odaka_falls_after_the_last_mora() {
        let marked = marked_morae("おとこ", &Position::Downstep(3));

        assert_eq!(vec![false, true, true], marked.iter().map(|m| m.high).collect::<Vec<_>>());
        assert_eq!(vec![false, false, true], marked.iter().map(|m| m.fall).collect::<Vec<_>>());
    }

    /// A one-mora reading accounts for 0.3% of corpus rows.
    #[test]
    fn a_one_mora_atamadaka_is_high_and_falls_at_its_end() {
        let marked = marked_morae("あ", &Position::Downstep(1));

        assert_eq!(1, marked.len());
        assert!(marked[0].high && marked[0].fall);
    }

    /// A pattern can state the next particle's level with one extra character.
    /// A high final mora then has no fall.
    #[test]
    fn a_pattern_with_a_trailing_level_moves_where_the_pitch_falls() {
        let ends_high = marked_morae("おとこ", &Position::Pattern("LHHH".to_string()));
        assert!(ends_high.iter().all(|m| !m.fall), "the particle is high, so nothing falls");

        let ends_low = marked_morae("おとこ", &Position::Pattern("LHHL".to_string()));
        assert_eq!(vec![false, false, true], ends_low.iter().map(|m| m.fall).collect::<Vec<_>>());
    }
}
