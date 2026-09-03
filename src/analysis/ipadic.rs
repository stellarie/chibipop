use super::PartOfSpeech;

/// The IPADIC fields needed by the analysis result.
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
    /// Parse an IPADIC feature string without rejecting shortened unknown rows.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tabe_with_surface_and_nine_fields() {
        let feature = Feature::parse("食べ,動詞,自立,*,*,一段,連用形,食べる,タベ,タベ");
        assert_eq!("動詞", feature.pos);
        assert_eq!("自立", feature.pos1);
        assert_eq!("一段", feature.ctype);
        assert_eq!("連用形", feature.cform);
        assert_eq!("食べる", feature.base);
        assert_eq!("タベ", feature.reading);
        assert_eq!(PartOfSpeech::Verb, feature.part_of_speech());
    }

    #[test]
    fn parses_ta_with_surface_and_nine_fields() {
        let feature = Feature::parse("た,助動詞,*,*,*,特殊・タ,基本形,た,タ,タ");
        assert_eq!("助動詞", feature.pos);
        assert_eq!("特殊・タ", feature.ctype);
        assert_eq!("基本形", feature.cform);
        assert_eq!("た", feature.base);
        assert_eq!("タ", feature.reading);
        assert_eq!(PartOfSpeech::AuxiliaryVerb, feature.part_of_speech());
    }

    #[test]
    fn parses_abc_with_a_short_unknown_record() {
        let feature = Feature::parse("abc,名詞,一般,*,*,*,*,abc");
        assert_eq!("名詞", feature.pos);
        assert_eq!("一般", feature.pos1);
        assert_eq!("abc", feature.base);
        assert_eq!("", feature.reading);
        assert_eq!(PartOfSpeech::Noun, feature.part_of_speech());
    }

    #[test]
    fn parses_123_with_a_feature_only_nine_field_record() {
        let feature = Feature::parse("名詞,数,*,*,*,*,123,イチ,イチ");
        assert_eq!("名詞", feature.pos);
        assert_eq!("数", feature.pos1);
        assert_eq!("123", feature.base);
        assert_eq!("イチ", feature.reading);
        assert_eq!(PartOfSpeech::Noun, feature.part_of_speech());
    }
}
