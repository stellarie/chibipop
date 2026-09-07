//! Direct search shares lookup ranking and presentation so typed inflections
//! and dictionary ordering behave like popup lookups without an OCR source.

use crate::config::Config;
use crate::dict::pitch::PitchClaim;
use crate::lookup::deconj::Deconjugator;
use crate::lookup::engine::LookupEngine;
use crate::lookup::model::{Dictionary, Entry, TermRow};
use crate::lookup::rules::load_rules;
use crate::lookup::sqlite::SqliteDictionary;
use crate::present::{self, DictInfo, PresentConfig, Presentation};
use anyhow::Result;
use std::path::Path;
use std::ops::Range;
use std::path::PathBuf;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    #[default]
    Dictionary,
    Sentence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub index: usize,
    pub headword: String,
    pub reading: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentenceToken {
    pub range: Range<usize>,
    pub text: String,
    pub selectable: bool,
}

pub fn sentence_token_at(tokens: &[SentenceToken], byte: usize) -> Option<&SentenceToken> {
    tokens.iter().find(|token| token.selectable && token.range.contains(&byte))
}

pub struct SentenceAnalyzer {
    model: PathBuf,
    analyzer: Option<crate::analysis::Analyzer>,
    attempted: bool,
}

impl SentenceAnalyzer {
    pub fn new(model: PathBuf) -> Self {
        Self { model, analyzer: None, attempted: false }
    }

    pub fn tokenize(&mut self, text: &str) -> Vec<SentenceToken> {
        if !self.attempted && !text.trim().is_empty() {
            self.attempted = true;
            match crate::analysis::Analyzer::load(&self.model) {
                Ok(analyzer) => self.analyzer = Some(analyzer),
                Err(error) => eprintln!("chibipop: sentence analysis unavailable: {error:#}"),
            }
        }
        let ranges = match &mut self.analyzer {
            Some(analyzer) => analyzer.analyze(text).words,
            None => crate::analysis::fallback_words(text),
        };
        sentence_tokens(text, &ranges)
    }

    pub fn fallback_active(&self) -> bool { self.attempted && self.analyzer.is_none() }
}

fn sentence_tokens(text: &str, ranges: &[Range<usize>]) -> Vec<SentenceToken> {
    let token = |range: Range<usize>, selectable: bool| SentenceToken {
        text: text[range.clone()].to_string(), range, selectable,
    };
    let mut tokens = Vec::new();
    let mut end = 0;
    for range in ranges {
        if range.start < end || range.start >= range.end || range.end > text.len()
            || !text.is_char_boundary(range.start) || !text.is_char_boundary(range.end) {
            continue;
        }
        if range.start > end { tokens.push(token(end..range.start, false)); }
        let selectable = text[range.clone()].chars().any(char::is_alphanumeric);
        tokens.push(token(range.clone(), selectable));
        end = range.end;
    }
    if end < text.len() { tokens.push(token(end..text.len(), false)); }
    tokens
}

pub struct SearchService {
    dictionary: SqliteDictionary,
    engine: LookupEngine,
    dicts: Vec<DictInfo>,
    config: PresentConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SearchResult {
    Empty,
    Miss,
    Found(Box<Presentation>),
}

pub fn candidates(result: &SearchResult) -> Vec<Candidate> {
    let SearchResult::Found(presentation) = result else { return Vec::new() };
    presentation.all_cards.iter().enumerate().map(|(index, card)| {
        let summary: String = card.blocks.iter().flat_map(|block| &block.entries)
            .flat_map(|entry| &entry.glosses).next().map(String::as_str).unwrap_or("")
            .chars().take(80).collect();
        Candidate {
            index, headword: card.written.as_ref().or(card.reading.as_ref()).cloned().unwrap_or_default(),
            reading: card.reading.clone().unwrap_or_default(), summary,
        }
    }).collect()
}

pub fn selected_presentation(result: &SearchResult, index: usize) -> Option<Presentation> {
    let SearchResult::Found(presentation) = result else { return None };
    let card = presentation.all_cards.get(index)?.clone();
    Some(Presentation {
        top: Some(card.clone()), all_cards: vec![card], collapsed: Vec::new(),
        sentence: presentation.sentence.clone(), surface: presentation.surface.clone(),
    })
}

impl SearchService {
    pub fn open(database: &Path, rules: &Path, config: &Config) -> Result<Self> {
        let dictionary = SqliteDictionary::open(database)?;
        let dicts = dictionary.dicts()?;
        Ok(Self {
            dictionary,
            engine: LookupEngine::new(Deconjugator::new(load_rules(rules)?)),
            config: config.present_config(&dicts),
            dicts,
        })
    }

    pub fn search(&self, query: &str) -> Result<SearchResult> {
        search(&self.dictionary, &self.engine, &self.dicts, &self.config, query)
    }
}

struct EnabledDictionary<'a> {
    dictionary: &'a dyn Dictionary,
    ids: Vec<i64>,
}

impl Dictionary for EnabledDictionary<'_> {
    fn terms_for(&self, surface: &str) -> Result<Vec<TermRow>> {
        Ok(self.dictionary.terms_for(surface)?.into_iter()
            .filter(|row| self.ids.contains(&row.dict_id)).collect())
    }

    fn entries(&self, ids: &[i64]) -> Result<Vec<Entry>> { self.dictionary.entries(ids) }
    fn dicts(&self) -> Result<Vec<DictInfo>> { self.dictionary.dicts() }
    fn pitch_for(&self, term: &str, reading: &str) -> Vec<PitchClaim> {
        self.dictionary.pitch_for(term, reading)
    }
}

fn search(dictionary: &dyn Dictionary, engine: &LookupEngine, dicts: &[DictInfo],
    config: &PresentConfig, query: &str) -> Result<SearchResult> {
    if query.trim().is_empty() { return Ok(SearchResult::Empty); }
    let enabled = EnabledDictionary {
        dictionary,
        ids: dicts.iter().filter(|dict| config.terms.contains(&dict.name))
            .map(|dict| dict.dict_id).collect(),
    };
    let hits = engine.run(&enabled, query)?;
    let presentation = present::build(&hits, dicts, config, dictionary);
    Ok(if presentation.top.is_some() { SearchResult::Found(Box::new(presentation)) }
        else { SearchResult::Miss })
}

pub fn result_text(result: &SearchResult) -> String {
    let SearchResult::Found(presentation) = result else {
        return match result {
            SearchResult::Empty => "Type a Japanese word or expression, then press Enter.".into(),
            _ => "No matching entries in the enabled dictionaries.".into(),
        };
    };
    let mut output = Vec::new();
    for card in &presentation.all_cards {
        let mut heading = card.written.clone().or_else(|| card.reading.clone()).unwrap_or_default();
        if let Some(reading) = &card.reading {
            if Some(reading) != card.written.as_ref() { heading.push_str(&format!("  {reading}")); }
        }
        output.push(heading);
        if let Some(freq) = card.freq { output.push(format!("Frequency: {freq}")); }
        if !card.pos.is_empty() { output.push(card.pos.join(" · ")); }
        if !card.inflections.is_empty() { output.push(card.inflections.join(" « ")); }
        for pitch in &card.pitch {
            let position = match &pitch.accent.position {
                crate::dict::pitch::Position::Downstep(0) => "0 (heiban)".into(),
                crate::dict::pitch::Position::Downstep(position) => position.to_string(),
                crate::dict::pitch::Position::Pattern(pattern) => pattern.clone(),
            };
            output.push(format!("Pitch: {position} ({})", pitch.dicts.join(", ")));
        }
        for block in &card.blocks {
            output.push(format!("[{}]", block.dict_name));
            for entry in &block.entries {
                if !entry.tags.is_empty() { output.push(entry.tags.join(" · ")); }
                output.extend(entry.glosses.iter().cloned());
            }
        }
        output.push(String::new());
    }
    output.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookup::model::FakeDictionary;

    #[test]
    fn sentence_tokens_preserve_text_and_select_full_words_at_byte_offsets() {
        let text = "猫は食べました。\n 鳥";
        let ranges = [0..3, 3..6, 6..21, 21..24, 26..29];
        let tokens = sentence_tokens(text, &ranges);
        assert_eq!(tokens.iter().map(|token| token.text.as_str()).collect::<String>(), text);
        let selected = sentence_token_at(&tokens, 12).unwrap();
        assert_eq!(selected.text, "食べました");
        assert_eq!(selected.range, 6..21);
        assert!(sentence_token_at(&tokens, 21).is_none());
        assert!(sentence_token_at(&tokens, 24).is_none());
        assert!(sentence_token_at(&tokens, text.len()).is_none());
    }

    #[test]
    fn sentence_analysis_groups_real_japanese_inflections() {
        let model = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/ipadic/system.dic");
        let mut analyzer = SentenceAnalyzer::new(model);
        let text = "猫は食べました。";
        let tokens = analyzer.tokenize(text);
        assert!(!analyzer.fallback_active());
        assert_eq!(sentence_token_at(&tokens, text.find("べ").unwrap()).unwrap().text, "食べました");
        assert_eq!(tokens.iter().map(|token| token.text.as_str()).collect::<String>(), text);
    }

    #[test]
    fn missing_sentence_model_falls_back_without_losing_unicode_or_spacing() {
        let mut analyzer = SentenceAnalyzer::new(PathBuf::from("missing-sentence-model.dic"));
        let text = "𠮷野家で 食べる。\n";
        let tokens = analyzer.tokenize(text);
        assert!(analyzer.fallback_active());
        assert_eq!(tokens.iter().map(|token| token.text.as_str()).collect::<String>(), text);
        assert!(sentence_token_at(&tokens, 0).is_some());
        assert!(sentence_token_at(&tokens, text.len() - 1).is_none());
    }

    #[test]
    fn japanese_repeated_empty_miss_and_dictionary_filter() {
        let mut dictionary = FakeDictionary::new();
        for id in 1..=14 {
            dictionary.add_term("猫", Some("猫"), Some("ねこ"), "", Some(id), id, id);
            dictionary.add_entry(id, id, "[\"a complete definition\",\"another sense\"]");
        }
        let dicts = vec![DictInfo { dict_id: 14, name: "Enabled".into() }];
        let config = PresentConfig { terms: vec!["Enabled".into()], pitch: vec![], summary_chars: 1 };
        let engine = LookupEngine::new(Deconjugator::new(vec![]));
        let run = |query| search(&dictionary, &engine, &dicts, &config, query).unwrap();
        assert_eq!(run("　 "), SearchResult::Empty);
        assert_eq!(run("犬"), SearchResult::Miss);
        let first = run(" 猫 ");
        assert!(matches!(first, SearchResult::Found(_)));
        let text = result_text(&first);
        assert!(text.contains("ねこ"));
        assert!(text.contains("a complete definition\nanother sense"));
        assert_eq!(run("猫"), first);
        let disabled = PresentConfig { terms: vec![], ..config };
        assert_eq!(search(&dictionary, &engine, &dicts, &disabled, "猫").unwrap(), SearchResult::Miss);
    }

    #[test]
    fn missing_database_reports_an_error() {
        assert!(SearchService::open(Path::new("missing-search.sqlite"), Path::new("missing-rules.json"),
            &Config::default()).is_err());
    }

    #[test]
    fn sqlite_fixture_supports_inflections_reload_and_disabled_dictionaries() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let database = std::env::temp_dir().join(format!("chibipop-search-{}.sqlite", std::process::id()));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); }
        }
        let _cleanup = Cleanup(database.clone());
        crate::dict::build::build(&[root.join("tests/fixtures/yomitan/terms.zip")],
            &[root.join("tests/fixtures/yomitan/freq.zip")], &database, &|_| {}).unwrap();
        let rules = root.join("data/deconjugator.json");
        let mut config = Config::default();
        {
            let service = SearchService::open(&database, &rules, &config).unwrap();
            assert_eq!(service.search("　").unwrap(), SearchResult::Empty);
            assert_eq!(service.search("絶対にない検索").unwrap(), SearchResult::Miss);
            let first = service.search("食べました").unwrap();
            let choices = candidates(&first);
            assert!(!choices.is_empty());
            let index = choices.iter().find(|candidate| candidate.headword == "食べる").unwrap().index;
            let selected = selected_presentation(&first, index).unwrap();
            assert_eq!(selected.top.as_ref().unwrap().written.as_deref(), Some("食べる"));
            assert_eq!(selected.all_cards.len(), 1);
            assert!(selected.collapsed.is_empty());
            assert!(selected_presentation(&first, usize::MAX).is_none());
            let text = result_text(&first);
            assert!(text.contains("食べる"), "{text}");
            assert!(text.contains("to eat"), "{text}");
            assert!(result_text(&service.search("猫").unwrap()).contains("cat (kanji)"));
            assert_eq!(service.search("食べました").unwrap(), first);
        }
        config.dictionaries.terms_disabled = vec!["FixtureTerms".into()];
        let service = SearchService::open(&database, &rules, &config).unwrap();
        assert_eq!(service.search("猫").unwrap(), SearchResult::Miss);
        drop(service);
        config.dictionaries.terms_disabled.clear();
        let service = SearchService::open(&database, &rules, &config).unwrap();
        assert!(matches!(service.search("猫").unwrap(), SearchResult::Found(_)));
        drop(service);
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection.execute("DROP TABLE term", []).unwrap();
        let service = SearchService::open(&database, &rules, &config).unwrap();
        assert!(service.search("猫").is_err());
    }
}
