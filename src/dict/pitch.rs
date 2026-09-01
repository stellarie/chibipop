//! Pitch patterns: the Yomitan payload, and the moras a mark lands on.
//!
//! Everything about a Pitch pattern that is neither storage nor drawing:
//! the parser over a `term_meta_bank_` row tagged `"pitch"`, the predicate
//! that reports whether an archive supplies the pitch role at all, and the
//! mora arithmetic both renderers spend - the card header's marked kana and
//! the mined note's HTML field.
//!
//! Ported from Yomitan rather than reinvented, because a mora index this
//! module counts has to be the index a dictionary meant:
//! `docs/research/pitch-accent-shapes.md` quotes `getKanaMorae`,
//! `isMoraPitchHigh`, `createPronunciationText` and `_toNumberArray` from
//! the revision it pins, and every rule below is one of those four.

use crate::dict::archive;
use anyhow::Result;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// One dictionary's Pitch patterns: headword plus reading to the accents it
/// gave them.
///
/// Ordered, and the accents inside one entry keep the order the archive
/// wrote them: a build has to produce the same database twice from the same
/// bytes, and a card draws several accents in the order they arrived.
///
/// One key may be named by several rows - 3 614 expression+reading pairs in
/// ticket 01's census are - so the parser merges rather than overwrites, and
/// an accent already claimed for a key is not claimed twice.
pub type PitchTable = BTreeMap<(String, String), Vec<Accent>>;

/// One Pitch pattern: where the pitch falls, and the moras the dictionary
/// marked alongside it.
///
/// Every field the schema permits, including the two this ticket does not
/// draw. Ticket 06 draws them, and 25.8% of NHK's rows carry one, so
/// dropping them on the way in would have cost that ticket a second schema
/// bump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accent {
    pub position: Position,
    /// 1-based indices of the moras with a nasal sound.
    pub nasal: Vec<u32>,
    /// 1-based indices of the moras with a devoiced sound.
    pub devoice: Vec<u32>,
    /// The accent's own tags, which typically name a part of speech. Never
    /// present in either corpus ticket 01 read, and schema-legal.
    pub tags: Vec<String>,
}

/// Where the pitch falls, in the two forms the schema permits.
///
/// They do not share an indexing origin, which is the trap ticket 01
/// recorded: the integer is a **1-based** count of moras before the fall
/// and the string is a **0-based** level per mora. [`is_mora_high`] is the
/// only place that difference is spent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Position {
    /// Moras before the downstep. `0` is heiban - no fall inside the word.
    ///
    /// Every one of the 511 488 accents ticket 01 censused is this form.
    Downstep(u32),
    /// One `H` or `L` per mora, in order, optionally with one more level at
    /// the end to state the following particle's.
    ///
    /// Schema-legal and used by nothing in either corpus, so it is accepted
    /// on the schema's authority and tested against a construction.
    Pattern(String),
}

/// One claim: an accent, and the dictionary that made it.
///
/// What a pitch read hands back. The dictionary is its `dict_id` rather
/// than its name because the name is what a reader sees and the id is what
/// identifies it - the same split every other read path takes.
#[derive(Debug, Clone, PartialEq)]
pub struct PitchClaim {
    pub dict_id: i64,
    pub accent: Accent,
}

/// One mora of a kana reading.
///
/// A mora is one or two characters, so a mora index is never a character
/// index and never a UTF-16 offset. Both are carried because both are
/// wanted: the mined note's HTML needs the text and the card header needs
/// the offset, the measurer addressing a run by UTF-16 unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mora<'a> {
    /// The mora's own characters.
    pub text: &'a str,
    /// UTF-16 offset of its first unit, from the start of the reading.
    pub at: u32,
    /// UTF-16 units in it.
    pub units: u32,
}

/// One mora, and what an accent says about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkedMora<'a> {
    pub mora: Mora<'a>,
    /// Is the pitch high on this mora?
    pub high: bool,
    /// Does the pitch fall after it?
    ///
    /// True exactly when this mora is high and the next one is not, the
    /// next one past the last being the particle that would follow the
    /// word - so heiban falls nowhere and odaka falls after its last mora.
    pub fall: bool,
}

/// The small kana that join the mora before them.
///
/// Yomitan's `SMALL_KANA_SET`, verbatim. What is *not* in it is the point:
/// `ッ` and `ー` are each a mora of their own, so `いっぽん` is four moras
/// and `アーム` is three.
const SMALL_KANA: &str = "ぁぃぅぇぉゃゅょゎァィゥェォャュョヮ";

/// Does this archive supply the pitch role?
///
/// Its own `term_meta_bank_` rows, never its filename: one of the six
/// archives named `[Pitch]` in ticket 01's census carries no
/// `term_meta_bank_` at all and writes its accents as text inside a
/// glossary, so the name is wrong about that archive in both directions.
///
/// The mode is matched by name rather than as "not `freq`". The term-meta
/// enum is closed today at `freq`, `pitch` and `ipa`, and a fourth mode
/// Yomitan adds later must not silently acquire this role.
///
/// Stops at the first pitch row, which is what makes it affordable: a pitch
/// archive answers from the first row of its first bank, and only an
/// archive that has meta banks with no pitch in them is read whole. An
/// archive with no meta banks at all is answered by its central directory.
///
/// `false` for an archive this build cannot open or whose banks it cannot
/// parse: unreadable supplies no role, which is the answer
/// [`crate::library::kind_of`] already gives such an archive.
pub fn supplies_pitch(archive: &Path) -> bool {
    archive::any_meta_row(archive, is_pitch_row).unwrap_or(false)
}

/// One archive's Pitch patterns.
///
/// The same streaming walk the frequency loader takes
/// ([`archive::for_each_meta_row`]); only the row filter and the
/// destination differ, which is the right amount of sharing. A `"freq"` row
/// in the same bank is skipped here exactly as a `"pitch"` row is skipped
/// there, so an archive carrying both contributes to both.
pub fn load_pitch(archive: &Path) -> Result<PitchTable> {
    let mut table = PitchTable::new();
    archive::for_each_meta_row(archive, |row| {
        merge_pitch_row(&mut table, &row);
        Ok(())
    })?;
    Ok(table)
}

/// One row into a table.
///
/// Anything that is not a pitch row this build can read is skipped rather
/// than fatal, for the reason every other archive reader gives: an archive
/// is third-party bytes. The `"pitch"` tag, a string headword, an object
/// payload, a `reading` and a `pitches` list are all required by the
/// schema, and an accent with no readable `position` is not an accent.
///
/// A reading named with an empty `pitches` list claims no key. That shape is
/// schema-legal - the field is required and has no `minItems` - and it means
/// "this reading has no accent", which is exactly what an absent key says.
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
        // Within one row as well as across rows: 11 rows in ticket 01's
        // census list one accent twice in one `pitches` list, and a
        // dictionary that claimed an accent once did not claim it twice.
        if !claimed.contains(&accent) {
            claimed.push(accent);
        }
    }
}

/// Is the pitch high on the mora at this 0-based index?
///
/// Yomitan's `isMoraPitchHigh`, and the whole semantics of a position. Note
/// what it does *not* do: it never bounds-checks the index against the
/// reading's mora count, because two rows in ticket 01's census put the
/// downstep past the last mora and both render as odaka rather than
/// panicking.
pub fn is_mora_high(index: usize, position: &Position) -> bool {
    match position {
        // 0-based and positional, so a level past the end of the string is
        // low - which is how a pattern states that the word ends high.
        Position::Pattern(levels) => levels.as_bytes().get(index) == Some(&b'H'),
        // Heiban: the first mora is low and every later one is high,
        // including the particle that follows the word.
        Position::Downstep(0) => index > 0,
        // Atamadaka: the first mora alone is high.
        Position::Downstep(1) => index < 1,
        // The moras between the first and the fall.
        Position::Downstep(fall) => index > 0 && index < *fall as usize,
    }
}

/// One reading's moras, in order.
///
/// Yomitan's `getKanaMorae`: a small kana joins the mora before it, and
/// everything else opens one. A small kana with nothing before it opens a
/// mora of its own rather than being dropped.
pub fn morae(reading: &str) -> Vec<Mora<'_>> {
    let mut out: Vec<Mora<'_>> = Vec::new();
    let mut seen = 0u32;
    for (at, c) in reading.char_indices() {
        let units = c.len_utf16() as u32;
        let end = at + c.len_utf8();
        match out.last_mut() {
            Some(open) if SMALL_KANA.contains(c) => {
                // Extend the mora already open over its own bytes and this
                // character's, which sit immediately after them.
                open.text = &reading[at - open.text.len()..end];
                open.units += units;
            }
            _ => out.push(Mora { text: &reading[at..end], at: seen, units }),
        }
        seen += units;
    }
    out
}

/// One reading's moras, each marked with what `position` says about it.
///
/// The card header's marked kana and the mined note's HTML are two
/// renderings of exactly this, so the two cannot disagree about which mora
/// carries the overline or where the tick goes.
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

/// Is this row a pitch row?
fn is_pitch_row(row: &Value) -> bool {
    row.as_array().is_some_and(|row| row.len() >= 3 && row[1].as_str() == Some("pitch"))
}

/// One accent object, or nothing this build can read as one.
fn parse_accent(value: &Value) -> Option<Accent> {
    let accent = value.as_object()?;
    Some(Accent {
        position: parse_position(accent.get("position")?)?,
        nasal: mora_indices(accent.get("nasal")),
        devoice: mora_indices(accent.get("devoice")),
        tags: parse_tags(accent.get("tags")),
    })
}

/// A `position` in either of its forms.
fn parse_position(value: &Value) -> Option<Position> {
    if let Some(fall) = value.as_u64() {
        return u32::try_from(fall).ok().map(Position::Downstep);
    }
    let levels = value.as_str()?;
    // `^[HL]+$`, and nothing else: a string that is not a run of levels is
    // not a position any mora can be indexed by.
    if levels.is_empty() || !levels.bytes().all(|b| b == b'H' || b == b'L') {
        return None;
    }
    Some(Position::Pattern(levels.to_string()))
}

/// A `nasal` or `devoice` field as the 1-based mora indices it names.
///
/// Yomitan's `_toNumberArray`, which resolves the scalar-or-list ambiguity
/// once and on the way in: a scalar `3`, a list `[3]`, an empty list and an
/// absent field are two facts and not four - the moras marked, and none.
fn mora_indices(value: Option<&Value>) -> Vec<u32> {
    let Some(value) = value else { return Vec::new() };
    match value.as_array() {
        Some(list) => list.iter().filter_map(mora_index).collect(),
        None => mora_index(value).into_iter().collect(),
    }
}

/// One mora index, or nothing.
fn mora_index(value: &Value) -> Option<u32> {
    u32::try_from(value.as_u64()?).ok()
}

/// A `tags` field as the tags it names.
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

    /// One row, parsed into a fresh table.
    fn parsed(row: Value) -> PitchTable {
        let mut table = PitchTable::new();
        merge_pitch_row(&mut table, &row);
        table
    }

    /// The accents one key was given.
    fn accents(table: &PitchTable, term: &str, reading: &str) -> Vec<Accent> {
        table.get(&(term.to_string(), reading.to_string())).cloned().unwrap_or_default()
    }

    /// The downsteps one key was given, which is what a header row draws.
    fn downsteps(table: &PitchTable, term: &str, reading: &str) -> Vec<Position> {
        accents(table, term, reading).into_iter().map(|a| a.position).collect()
    }

    fn heiban() -> Position {
        Position::Downstep(0)
    }

    // ---- the census's own payloads (docs/research/pitch-accent-shapes.md)

    /// 48.0% of the corpus, and the value a renderer has to get right
    /// first. NHK's row, verbatim.
    #[test]
    fn a_single_heiban_accent_parses_to_downstep_zero() {
        let table =
            parsed(json!(["ああ", "pitch", {"pitches": [{"position": 0, "devoice": [], "nasal": []}], "reading": "ああ"}]));

        assert_eq!(vec![heiban()], downsteps(&table, "ああ", "ああ"));
    }

    /// 新明解第八版's row. Key order differs from the row above's, and no
    /// parser may depend on it.
    #[test]
    fn a_single_atamadaka_accent_parses_to_downstep_one() {
        let table =
            parsed(json!(["あ", "pitch", {"pitches": [{"nasal": [], "devoice": [], "position": 1}], "reading": "あ"}]));

        assert_eq!(vec![Position::Downstep(1)], downsteps(&table, "あ", "あ"));
    }

    /// 大辞林第四版's row: two accents for one reading, and the order they
    /// arrive in is the order they are stored in.
    #[test]
    fn two_accents_in_one_row_keep_the_order_the_archive_wrote_them() {
        let table = parsed(json!(["アーカイブ", "pitch", {"reading": "アーカイブ", "pitches": [{"position": 3, "devoice": [], "nasal": []}, {"devoice": [], "nasal": [], "position": 1}]}]));

        assert_eq!(
            vec![Position::Downstep(3), Position::Downstep(1)],
            downsteps(&table, "アーカイブ", "アーカイブ")
        );
    }

    /// 大辞泉's row, three accents.
    #[test]
    fn three_accents_in_one_row_all_parse() {
        let table = parsed(json!(["アーカイブ", "pitch", {"reading": "アーカイブ", "pitches": [{"devoice": [], "position": 1, "nasal": []}, {"nasal": [], "position": 3, "devoice": []}, {"devoice": [], "position": 0, "nasal": []}]}]));

        assert_eq!(
            vec![Position::Downstep(1), Position::Downstep(3), heiban()],
            downsteps(&table, "アーカイブ", "アーカイブ")
        );
    }

    /// The corpus maximum, and its only such row: NHK's `不義理`, four
    /// accents, every one of them carrying the same nasal marker.
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

    /// NHK's `合鍵`: a nasal marker on a heiban accent. 2.03% of the
    /// corpus's accents carry one and every one of them is NHK's, so this
    /// is the shape ticket 06 will draw.
    #[test]
    fn a_nasal_marker_is_kept_as_a_one_based_mora_index() {
        let table = parsed(json!(["合鍵", "pitch", {"reading": "あいかぎ", "pitches": [{"devoice": [], "position": 0, "nasal": [4]}]}]));

        let found = accents(&table, "合鍵", "あいかぎ");
        assert_eq!(vec![4], found[0].nasal);
        assert!(found[0].devoice.is_empty(), "an empty devoice names no mora");
    }

    /// NHK's `アーク灯`, where the marker sits on the third mora of a
    /// five-character reading - which is what makes it a mora index rather
    /// than a character index.
    #[test]
    fn a_devoice_marker_is_kept_as_a_one_based_mora_index() {
        let table = parsed(json!(["アーク灯", "pitch", {"reading": "アークとう", "pitches": [{"nasal": [], "devoice": [3], "position": 0}]}]));

        let found = accents(&table, "アーク灯", "アークとう");
        assert_eq!(vec![3], found[0].devoice);
        assert!(found[0].nasal.is_empty(), "an empty nasal names no mora");
    }

    /// NHK's `扱い`: two accents, both carrying the same devoice marker.
    #[test]
    fn both_markers_survive_on_a_two_accent_row() {
        let table = parsed(json!(["扱い", "pitch", {"pitches": [{"devoice": [2], "nasal": [], "position": 0}, {"position": 3, "nasal": [], "devoice": [2]}], "reading": "あつかい"}]));

        let found = accents(&table, "扱い", "あつかい");
        assert_eq!(vec![heiban(), Position::Downstep(3)],
            found.iter().map(|a| a.position.clone()).collect::<Vec<_>>());
        assert!(found.iter().all(|a| a.devoice == vec![2]), "{found:?}");
    }

    /// 三省堂国語辞典第八番 spells a second accent as a second row, so a
    /// table keyed on expression plus reading has to merge. 3 614 pairs in
    /// the census are named by two or more rows of one dictionary.
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

    /// 三省堂's `あまり`, three rows with one accent each.
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

    /// 大辞泉's `一体`, one of 11 rows in the corpus that list one accent
    /// twice inside one `pitches` list.
    #[test]
    fn one_row_repeating_an_accent_stores_it_once() {
        let table = parsed(json!(["一体", "pitch", {"reading": "いったい", "pitches": [{"position": 0, "devoice": [], "nasal": []}, {"nasal": [], "position": 1, "devoice": []}, {"nasal": [], "position": 0, "devoice": []}]}]));

        assert_eq!(vec![heiban(), Position::Downstep(1)], downsteps(&table, "一体", "いったい"));
    }

    /// NHK's `自動車損害賠償責任保険`: the corpus's longest reading, its
    /// highest downstep, and a nasal marker, in one row.
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

    /// 大辞林第四版's `築後`: a downstep past the last mora, which is
    /// schema-legal and a data error. Both accents survive, and the
    /// out-of-range one draws rather than panicking - a renderer that
    /// indexed an array by `position` would go out of bounds here.
    ///
    /// `isMoraPitchHigh` handles it by accident, and the accident is worth
    /// spelling out because ticket 01's prose rounds it to "odaka": with
    /// three moras and a downstep of 5 the mora *past* the last is high too,
    /// so nothing falls and the row draws as a rise with no tick.
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
        // The in-range accent on the same row *is* odaka: three moras, a
        // downstep of three, so the tick lands after the last one.
        let odaka = marked_morae("ちくご", &Position::Downstep(3));
        assert_eq!(vec![false, false, true], odaka.iter().map(|m| m.fall).collect::<Vec<_>>());
    }

    /// 三省堂's `扱い`, whose reading no term dictionary will ever produce.
    /// 122 rows in the corpus carry one; they are dead rows rather than a
    /// parser problem, so they parse and simply never match a headword.
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

    /// The sixth `[Pitch]`-named archive writes its accent as glossary
    /// text, so its `term_bank_` row reaches no pitch parser at all. Quoted
    /// from the census to pin that a term row is not a pitch row.
    #[test]
    fn a_term_bank_row_is_not_a_pitch_row() {
        let table = parsed(json!(["帯広", "おびひろ", "名詞 地名", "", 0, ["おびひろ【帯広】（北海道）\n ・［0］オビヒロ"], 0, ""]));

        assert!(table.is_empty(), "a term row carries no pitch, whatever its archive is named");
    }

    /// A `"freq"` row shares the bank with the pitch rows and belongs to
    /// the other role.
    #[test]
    fn a_freq_tagged_row_produces_an_empty_pitch_table() {
        assert!(parsed(json!(["猫", "freq", {"reading": "ねこ", "frequency": 42}])).is_empty());
        assert!(parsed(json!(["食べる", "freq", 7])).is_empty());
    }

    /// The one other mode in the closed term-meta enum. Skipped by name,
    /// so a fourth mode Yomitan adds cannot acquire the pitch role either.
    #[test]
    fn an_ipa_row_produces_an_empty_pitch_table() {
        let table = parsed(json!(["猫", "ipa", {"reading": "ねこ", "transcriptions": [{"ipa": "neko"}]}]));

        assert!(table.is_empty());
    }

    // ---- the four shapes the corpus cannot supply, as constructions

    /// Schema-legal and absent from the corpus (0 of 466 990 rows), so this
    /// payload is ticket 01's construction and not evidence. It names a
    /// reading and gives it no accent, which is what an absent key already
    /// says - so it must claim no key, or the card would draw an empty row.
    #[test]
    fn an_empty_pitches_list_claims_no_key() {
        let table = parsed(json!(["れい", "pitch", {"reading": "れい", "pitches": []}]));

        assert!(table.is_empty());
    }

    /// The `^[HL]+$` form of `position`, which the schema permits and no
    /// archive in either corpus writes. A construction, from ticket 01.
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

    /// The scalar rather than list form of the two markers, which
    /// `_toNumberArray` normalises. A construction, from ticket 01.
    #[test]
    fn scalar_nasal_and_devoice_markers_normalise_to_one_element_lists() {
        let table = parsed(json!(["れい", "pitch", {"reading": "れい", "pitches": [{"position": 2, "nasal": 3, "devoice": 1}]}]));

        let found = accents(&table, "れい", "れい");
        assert_eq!(vec![3], found[0].nasal);
        assert_eq!(vec![1], found[0].devoice);
    }

    /// `tags` never appears in 511 488 accents across two corpora, and is
    /// schema-legal. A construction, from ticket 01.
    #[test]
    fn a_tags_list_is_kept_verbatim() {
        let table = parsed(json!(["れい", "pitch", {"reading": "れい", "pitches": [{"position": 2, "tags": ["名"]}]}]));

        assert_eq!(vec!["名".to_string()], accents(&table, "れい", "れい")[0].tags);
    }

    /// `additionalProperties: false` means Yomitan would refuse the archive
    /// this row came from, so the key is ignored rather than fatal and the
    /// accent it hangs off still parses.
    #[test]
    fn an_unknown_accent_key_is_ignored_and_the_accent_still_parses() {
        let table = parsed(json!(["れい", "pitch", {"reading": "れい", "pitches": [{"position": 1, "bogus": 1}]}]));

        assert_eq!(vec![Position::Downstep(1)], downsteps(&table, "れい", "れい"));
    }

    /// A `position` of neither permitted type is not an accent, and the
    /// accents beside it in the same list still are.
    #[test]
    fn an_unreadable_position_drops_its_accent_and_keeps_the_others() {
        let table = parsed(json!(["れい", "pitch", {"reading": "れい", "pitches": [{"position": true}, {"position": "xyz"}, {"position": -1}, {"position": 2}]}]));

        assert_eq!(vec![Position::Downstep(2)], downsteps(&table, "れい", "れい"));
    }

    /// The schema requires both payload keys, and a row missing either
    /// names nothing this build can key an accent by.
    #[test]
    fn a_payload_missing_its_reading_or_its_pitches_claims_no_key() {
        assert!(parsed(json!(["れい", "pitch", {"pitches": [{"position": 1}]}])).is_empty());
        assert!(parsed(json!(["れい", "pitch", {"reading": "れい"}])).is_empty());
        assert!(parsed(json!(["れい", "pitch", 3])).is_empty());
        assert!(parsed(json!(["れい", "pitch"])).is_empty());
        assert!(parsed(json!({"reading": "れい"})).is_empty());
    }

    // ---- moras

    /// Yomitan's `SMALL_KANA_SET` joins a small kana to the mora before it,
    /// and `ッ` and `ー` are deliberately not in it - so `いっぽん` is four
    /// moras and `アーム` is three. A mora index is never a character
    /// index, and this is why.
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

    /// The offsets are UTF-16 units from the start of the reading, because
    /// that is the coordinate the measurement seam addresses a run by.
    #[test]
    fn a_moras_offset_counts_utf16_units_and_not_characters() {
        let found = morae("きょうと");

        assert_eq!(vec![0, 2, 3], found.iter().map(|m| m.at).collect::<Vec<_>>());
        assert_eq!(vec![2, 1, 1], found.iter().map(|m| m.units).collect::<Vec<_>>());
    }

    /// Degenerate, and it must not panic or lose the character: a small
    /// kana with nothing before it opens a mora of its own.
    #[test]
    fn a_leading_small_kana_opens_its_own_mora() {
        let found: Vec<&str> = morae("ゃあ").into_iter().map(|m| m.text).collect();

        assert_eq!(vec!["ゃ", "あ"], found);
    }

    #[test]
    fn an_empty_reading_has_no_morae() {
        assert!(morae("").is_empty());
    }

    /// Heiban's shape, which is 48% of the corpus: the first mora low, every
    /// later one high, and no fall inside the word - the rise carries into
    /// the particle that follows.
    #[test]
    fn heiban_marks_every_mora_after_the_first_high_and_falls_nowhere() {
        let marked = marked_morae("ざつだん", &heiban());

        assert_eq!(vec![false, true, true, true], marked.iter().map(|m| m.high).collect::<Vec<_>>());
        assert!(marked.iter().all(|m| !m.fall), "heiban falls nowhere: {marked:?}");
    }

    /// Atamadaka: the first mora alone is high, and the fall is after it.
    #[test]
    fn atamadaka_marks_the_first_mora_high_and_falls_after_it() {
        let marked = marked_morae("ねこ", &Position::Downstep(1));

        assert_eq!(vec![true, false], marked.iter().map(|m| m.high).collect::<Vec<_>>());
        assert_eq!(vec![true, false], marked.iter().map(|m| m.fall).collect::<Vec<_>>());
    }

    /// Nakadaka: the moras between the first and the downstep are high, and
    /// the fall is at the downstep.
    #[test]
    fn nakadaka_marks_the_moras_before_the_downstep_high() {
        let marked = marked_morae("あつかい", &Position::Downstep(3));

        assert_eq!(vec![false, true, true, false], marked.iter().map(|m| m.high).collect::<Vec<_>>());
        assert_eq!(vec![false, false, true, false], marked.iter().map(|m| m.fall).collect::<Vec<_>>());
    }

    /// Odaka: the downstep is the mora count, so the word ends high and the
    /// fall lands on the particle after it - which is drawn as a tick after
    /// the last mora.
    #[test]
    fn odaka_falls_after_the_last_mora() {
        let marked = marked_morae("おとこ", &Position::Downstep(3));

        assert_eq!(vec![false, true, true], marked.iter().map(|m| m.high).collect::<Vec<_>>());
        assert_eq!(vec![false, false, true], marked.iter().map(|m| m.fall).collect::<Vec<_>>());
    }

    /// A one-mora reading, which 0.3% of the corpus's rows are.
    #[test]
    fn a_one_mora_atamadaka_is_high_and_falls_at_its_end() {
        let marked = marked_morae("あ", &Position::Downstep(1));

        assert_eq!(1, marked.len());
        assert!(marked[0].high && marked[0].fall);
    }

    /// A pattern can state the following particle's level with one extra
    /// character, and then the word's last mora does not fall.
    #[test]
    fn a_pattern_with_a_trailing_level_moves_where_the_pitch_falls() {
        let ends_high = marked_morae("おとこ", &Position::Pattern("LHHH".to_string()));
        assert!(ends_high.iter().all(|m| !m.fall), "the particle is high, so nothing falls");

        let ends_low = marked_morae("おとこ", &Position::Pattern("LHHL".to_string()));
        assert_eq!(vec![false, false, true], ends_low.iter().map(|m| m.fall).collect::<Vec<_>>());
    }
}
