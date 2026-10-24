use super::PartOfSpeech;

/// This type stores the IPADIC fields that the analysis result needs.
///
/// Vibrato returns the surface as the first field for dictionary entries. Its
/// unknown-word entries can return only the feature fields. The parser accepts
/// both forms and both seven-field and nine-field feature records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Feature<'a> {
    pub(super) pos: &'a str,
    pub(super) pos1: &'a str,
    pub(super) pos2: &'a str,
    pub(super) pos3: &'a str,
    pub(super) ctype: &'a str,
    pub(super) cform: &'a str,
    pub(super) base: &'a str,
    pub(super) reading: &'a str,
    pub(super) pron: &'a str,
}

impl<'a> Feature<'a> {
    /// Parse an IPADIC feature string. Accept shortened unknown rows.
    pub(super) fn parse(value: &'a str) -> Self {
        let fields: Vec<&str> = value.split(',').collect();
        let offset = match (fields.first(), fields.get(1)) {
            (Some(first), _) if is_pos(first) => 0,
            (_, Some(second)) if is_pos(second) => 1,
            _ => 0,
        };
        Self {
            pos: field(&fields, offset),
            pos1: field_at(&fields, offset + 1),
            pos2: field_at(&fields, offset + 2),
            pos3: field_at(&fields, offset + 3),
            ctype: field_at(&fields, offset + 4),
            cform: field_at(&fields, offset + 5),
            base: field_at(&fields, offset + 6),
            reading: field_at(&fields, offset + 7),
            pron: field_at(&fields, offset + 8),
        }
    }

    /// Map the IPADIC base and first detail labels to the public category.
    pub(super) fn part_of_speech(self) -> PartOfSpeech {
        match self.pos {
            "名詞" => match self.pos1 {
                "固有名詞" => PartOfSpeech::ProperNoun,
                "代名詞" => PartOfSpeech::Pronoun,
                "形容動詞語幹" => PartOfSpeech::AdjectivalNoun,
                "接尾" | "接尾辞" => PartOfSpeech::Suffix,
                _ => PartOfSpeech::Noun,
            },
            "動詞" => PartOfSpeech::Verb,
            "助動詞" => PartOfSpeech::AuxiliaryVerb,
            "形容詞" => PartOfSpeech::Adjective,
            "副詞" => PartOfSpeech::Adverb,
            "連体詞" => PartOfSpeech::Adnominal,
            "接続詞" => PartOfSpeech::Conjunction,
            "助詞" => PartOfSpeech::Particle,
            "接頭詞" => PartOfSpeech::Prefix,
            "接尾辞" => PartOfSpeech::Suffix,
            "感動詞" => PartOfSpeech::Interjection,
            "記号" => PartOfSpeech::Symbol,
            "フィラー" => PartOfSpeech::Filler,
            _ => PartOfSpeech::Other,
        }
    }
}
fn field<'a>(fields: &[&'a str], index: usize) -> &'a str {
    fields.get(index).copied().unwrap_or("*")
}

fn field_at<'a>(fields: &[&'a str], index: usize) -> &'a str {
    fields.get(index).copied().unwrap_or("")
}

fn is_pos(value: &str) -> bool {
    matches!(
        value,
        "名詞"
            | "動詞"
            | "助動詞"
            | "形容詞"
            | "副詞"
            | "連体詞"
            | "接続詞"
            | "助詞"
            | "接頭詞"
            | "接尾辞"
            | "感動詞"
            | "記号"
            | "フィラー"
    )
}

