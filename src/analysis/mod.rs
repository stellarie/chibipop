//! Japanese analysis stays in one concrete core module because issue #32 needs
//! one shared implementation on both platforms. A trait would add a fourth
//! seam without a second implementation, and a crate would hide the small
//! synchronous API behind a new workspace boundary.
//!
//! The benchmark that selected Vibrato used 27 strings and 5,400 warm calls per
//! process on Linux x86_64:
//!
//! | Metric | Vibrato | Sudachi.rs core |
//! |---|---:|---:|
//! | Decoded model | 47,788,814 bytes | 217,466,039 bytes |
//! | New-process load | 64.486 ms | 134.223 ms |
//! | RSS after load | 61,360 KiB | 243,608 KiB |
//! | Warm mean | 1.896 us | 5.213 us |
//! | Warm p95 | 3.630 us | 7.830 us |
//! | Characters per second | 9,180,885 | 3,339,336 |
//!
//! Both analyzers were fast enough after load. Vibrato also won on package
//! size, resident memory, and cold-load cost. The benchmark did not measure
//! Windows runtime behavior or segmentation accuracy against a gold corpus.
//!
//! The model is committed with its license notices and a pinned SHA-256 digest.
//! The loader verifies those bytes before it gives them to Vibrato. No code
//! downloads a model, so analysis remains offline-first.

mod ipadic;
mod model;
mod service;

pub use service::Service;

use anyhow::Result;
use ipadic::Feature;
use std::ops::Range;
use std::path::Path;
use unicode_segmentation::UnicodeSegmentation;

use crate::dict::gloss::NodePath;

/// A Japanese-focused part of speech from the IPADIC feature record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PartOfSpeech {
    Noun,
    ProperNoun,
    Pronoun,
    Verb,
    AuxiliaryVerb,
    Adjective,
    AdjectivalNoun,
    Adverb,
    Adnominal,
    Conjunction,
    Particle,
    Prefix,
    Suffix,
    Interjection,
    Symbol,
    Filler,
    Other,
}

/// One conjugation kind and form from IPADIC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inflection {
    pub kind: String,
    pub form: String,
}

/// One fine-grained token returned by the Japanese analyzer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Morpheme {
    pub range: Range<usize>,
    pub surface: String,
    pub lemma: String,
    pub reading: String,
    pub inflection: Option<Inflection>,
    pub pos: PartOfSpeech,
    pub labels: Vec<String>,
    pub normalized: Option<String>,
}

/// Fine morphemes and the larger words used by glossary selection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Analysis {
    pub morphemes: Vec<Morpheme>,
    pub words: Vec<Range<usize>>,
}

/// A loaded tokenizer for the committed IPADIC model.
///
/// The tokenizer owns the dictionary. `analyze` creates a short-lived Vibrato
/// worker because the worker borrows the tokenizer and cannot live in this
/// self-referential struct.
pub struct Analyzer {
    tokenizer: Box<vibrato::Tokenizer>,
}

impl Analyzer {
    /// Read and verify the model at `path`, then build its tokenizer.
    pub fn load(path: &Path) -> Result<Analyzer> {
        let bytes = model::read(path)?;
        let dict = vibrato::Dictionary::read(&bytes[..])?;
        let tokenizer = vibrato::Tokenizer::new(dict).ignore_space(true)?.max_grouping_len(24);
        Ok(Analyzer { tokenizer: Box::new(tokenizer) })
    }

    /// Analyze `text` and keep both fine morphemes and grouped word ranges.
    pub fn analyze(&mut self, text: &str) -> Analysis {
        let mut worker = self.tokenizer.new_worker();
        worker.reset_sentence(text);
        worker.tokenize();

        let mut morphemes = Vec::with_capacity(worker.num_tokens());
        for index in 0..worker.num_tokens() {
            let token = worker.token(index);
            let feature = Feature::parse(token.feature());
            let surface = token.surface().to_owned();
            let lemma = if feature.base == "*" || feature.base.is_empty() {
                surface.clone()
            } else {
                feature.base.to_owned()
            };
            let reading = if feature.reading == "*" {
                String::new()
            } else {
                feature.reading.to_owned()
            };
            let inflection = if (feature.ctype == "*" || feature.ctype.is_empty())
                && (feature.cform == "*" || feature.cform.is_empty())
            {
                None
            } else {
                Some(Inflection {
                    kind: empty_feature(feature.ctype),
                    form: empty_feature(feature.cform),
                })
            };
            let labels = [feature.pos1, feature.pos2, feature.pos3]
                .into_iter()
                .filter(|label| !label.is_empty() && *label != "*")
                .map(str::to_owned)
                .collect();
            let normalized = (lemma != surface).then(|| lemma.clone());

            morphemes.push(Morpheme {
                range: token.range_byte(),
                surface,
                lemma,
                reading,
                inflection,
                pos: feature.part_of_speech(),
                labels,
                normalized,
            });
        }

        let words = grouped_words(text, &morphemes);
        Analysis { morphemes, words }
    }
}

fn empty_feature(value: &str) -> String {
    if value == "*" { String::new() } else { value.to_owned() }
}

/// Return UAX #29 word-boundary segments that contain non-whitespace text.
///
/// The segmentation rule keeps each CJK ideograph separate while it keeps a
/// kana run together. Punctuation remains a separate segment so `word_at`
/// can reduce a punctuation hit to one grapheme.
pub fn fallback_words(text: &str) -> Vec<Range<usize>> {
    text.split_word_bound_indices()
        .filter_map(|(start, segment)| {
            (!segment.chars().all(|character| character.is_whitespace()))
                .then_some(start..start + segment.len())
        })
        .collect()
}

/// Return the word that contains `byte`, or one grapheme for punctuation and whitespace.
pub fn word_at(text: &str, words: &[Range<usize>], byte: usize) -> Range<usize> {
    let (grapheme, point) = grapheme_at(text, byte);
    if grapheme.start == grapheme.end {
        return grapheme;
    }
    let cluster = &text[grapheme.clone()];
    if !wordish(cluster) {
        return grapheme;
    }
    words
        .iter()
        .find(|word| word.start <= point && point < word.end)
        .cloned()
        .unwrap_or(grapheme)
}

fn grapheme_at(text: &str, byte: usize) -> (Range<usize>, usize) {
    if text.is_empty() {
        return (0..0, 0);
    }

    let mut point = byte.min(text.len());
    while point > 0 && !text.is_char_boundary(point) {
        point -= 1;
    }

    let mut last = None;
    for (start, cluster) in text.grapheme_indices(true) {
        let range = start..start + cluster.len();
        if point < range.end {
            return (range, point);
        }
        last = Some(range);
    }
    let range = last.expect("non-empty text has a grapheme");
    (range.clone(), range.start)
}

fn grouped_words(text: &str, morphemes: &[Morpheme]) -> Vec<Range<usize>> {
    let mut words: Vec<Range<usize>> = Vec::new();
    let mut previous_morpheme: Option<usize> = None;
    let mut previous_word: Option<usize> = None;

    for (index, morpheme) in morphemes.iter().enumerate() {
        if !morpheme.surface.chars().any(is_japanese) {
            let ranges = fallback_words(&morpheme.surface);
            for range in ranges {
                words.push((morpheme.range.start + range.start)..(morpheme.range.start + range.end));
            }
            previous_morpheme = None;
            previous_word = None;
            continue;
        }

        let merge = previous_morpheme
            .zip(previous_word)
            .and_then(|(previous_index, word_index)| {
                can_merge(text, &morphemes[previous_index], morpheme).then_some(word_index)
            });
        if let Some(word_index) = merge {
            words[word_index].end = morpheme.range.end;
        } else {
            words.push(morpheme.range.clone());
            previous_word = Some(words.len() - 1);
        }
        previous_morpheme = Some(index);
    }

    words
}

fn can_merge(text: &str, previous: &Morpheme, current: &Morpheme) -> bool {
    if previous.pos == PartOfSpeech::Symbol || current.pos == PartOfSpeech::Symbol {
        return false;
    }
    if previous.range.end > current.range.start
        || text[previous.range.end..current.range.start]
            .chars()
            .any(|character| character.is_whitespace())
    {
        return false;
    }

    if previous.pos == PartOfSpeech::Prefix {
        return true;
    }
    if matches!(current.pos, PartOfSpeech::AuxiliaryVerb | PartOfSpeech::Suffix) {
        return true;
    }
    if matches!(current.pos, PartOfSpeech::Verb | PartOfSpeech::Adjective)
        && current.labels.iter().any(|label| label.starts_with("非自立"))
    {
        return true;
    }
    current.pos == PartOfSpeech::Particle
        && current.labels.iter().any(|label| label == "接続助詞")
        && previous.pos == PartOfSpeech::Verb
}

fn wordish(cluster: &str) -> bool {
    cluster
        .chars()
        .any(|character| character == '_' || character.is_alphanumeric() || is_japanese(character))
}

fn is_japanese(character: char) -> bool {
    matches!(
        character,
        '\u{3040}'..='\u{30ff}'
            | '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{f900}'..='\u{faff}'
            | '\u{ff66}'..='\u{ff9f}'
            | '\u{20000}'..='\u{2ffff}'
    )
}
/// The file name used by the platform path resolver for the bundled model.
pub const MODEL_FILE: &str = "data/ipadic/system.dic";

/// The SHA-256 digest of the bundled decoded IPADIC model.
pub const MODEL_SHA256: &str =
    "f3ddd9cacbd6eff696e49bb9266824eed28dceaa89c55e75e4a13384cc7f9d02";

/// A text leaf key used to associate analysis with one Card entry.
pub type TextKey = (u32, NodePath);

/// Grouped word ranges keyed by their Card text leaf.
pub type WordMap = std::collections::HashMap<TextKey, Vec<std::ops::Range<usize>>>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::{LazyLock, Mutex};

    fn bundled_analyzer() -> Option<&'static Mutex<Analyzer>> {
        static ANALYZER: LazyLock<Option<Mutex<Analyzer>>> = LazyLock::new(|| {
            let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(MODEL_FILE);
            if !path.is_file() {
                eprintln!("skipping Japanese analysis test because {} is absent", path.display());
                return None;
            }
            Some(Mutex::new(Analyzer::load(&path).expect("the bundled Japanese model must load")))
        });
        ANALYZER.as_ref()
    }

    #[test]
    fn fallback_words_and_punctuation_use_uax_boundaries() {
        assert_eq!(vec![0..5, 6..11], fallback_words("Hello world"));
        let text = "Hello, world";
        let words = fallback_words(text);
        assert_eq!(5..6, word_at(text, &words, 5));
    }

    #[test]
    fn the_bundled_model_analyzes_a_past_sentence_and_groups_the_verb_chain() {
        let Some(analyzer) = bundled_analyzer() else {
            return;
        };
        let mut analyzer = analyzer.lock().unwrap();
        let text = "ご飯を食べた。";
        let analysis = analyzer.analyze(text);

        let surfaces: Vec<&str> = analysis.morphemes.iter().map(|morpheme| morpheme.surface.as_str()).collect();
        assert_eq!(vec!["ご飯", "を", "食べ", "た", "。"], surfaces);

        let tabe = analysis
            .morphemes
            .iter()
            .find(|morpheme| morpheme.surface == "食べ")
            .expect("食べ must be a morpheme");
        assert_eq!("食べる", tabe.lemma);
        assert_eq!("タベ", tabe.reading);
        assert_eq!(Some("一段"), tabe.inflection.as_ref().map(|inflection| inflection.kind.as_str()));

        let tabe_start = text.find("食べ").unwrap();
        let tabe_end = text.find("。").unwrap();
        assert!(analysis.words.iter().any(|range| range == &(tabe_start..tabe_end)));
        assert_eq!(tabe_end..text.len(), word_at(text, &analysis.words, tabe_end));
    }


}
