//! Deconjugation rule types and loading.
//!
//! `data/deconjugator.json` is a list mixing comment strings with rule
//! objects. Any of `dec_end` / `con_end` / `dec_tag` / `con_tag` may be a
//! single string or a list of strings.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StrOrList {
    One(String),
    Many(Vec<String>),
}

impl StrOrList {
    pub fn as_slice(&self) -> &[String] {
        match self {
            StrOrList::One(s) => std::slice::from_ref(s),
            StrOrList::Many(v) => v.as_slice(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    #[serde(rename = "type")]
    pub rule_type: String,
    pub dec_end: StrOrList,
    pub con_end: StrOrList,
    #[serde(default)]
    pub dec_tag: Option<StrOrList>,
    #[serde(default)]
    pub con_tag: Option<StrOrList>,
    #[serde(default)]
    pub detail: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Element {
    Rule(Box<Rule>),
    // The `String` payload is never read, but it is load-bearing: it is what
    // makes serde's untagged matching accept comment strings (and only
    // strings) instead of failing to parse, or silently accepting malformed
    // rule objects. Do not change it to `()` (a JSON string can't
    // deserialise into `()`) or to `IgnoredAny` (matches any JSON value).
    #[allow(dead_code)]
    Comment(String),
}

/// Load rules, discarding comment strings and `substitution` rules.
pub fn load_rules(path: &Path) -> Result<Vec<Rule>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading rules from {}", path.display()))?;
    let elements: Vec<Element> =
        serde_json::from_str(&text).context("parsing deconjugator.json")?;

    Ok(elements
        .into_iter()
        .filter_map(|e| match e {
            Element::Rule(r) => Some(*r),
            Element::Comment(_) => None,
        })
        .filter(|r| r.rule_type != "substitution")
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn rules_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/deconjugator.json")
    }

    #[test]
    fn loads_real_rule_file_skipping_comments_and_substitutions() {
        let rules = load_rules(&rules_path()).unwrap();
        // 106 objects in the file, minus 2 `substitution` rules.
        assert_eq!(104, rules.len());
        assert!(rules.iter().all(|r| r.rule_type != "substitution"));
    }

    #[test]
    fn parses_scalar_and_list_fields() {
        let one: StrOrList = serde_json::from_str(r#""る""#).unwrap();
        assert_eq!(&["る".to_string()], one.as_slice());
        let many: StrOrList = serde_json::from_str(r#"["く","す"]"#).unwrap();
        assert_eq!(2, many.as_slice().len());
    }

    #[test]
    fn optional_tags_absent_parse_as_none() {
        let r: Rule = serde_json::from_str(
            r#"{"type":"stdrule","dec_end":"る","con_end":"た","detail":"x"}"#,
        )
        .unwrap();
        assert!(r.dec_tag.is_none());
        assert!(r.con_tag.is_none());
    }

    #[test]
    fn every_rule_type_present_in_real_file() {
        let rules = load_rules(&rules_path()).unwrap();
        for t in ["stdrule", "onlyfinalrule", "neverfinalrule", "rewriterule",
                  "contextrule"] {
            assert!(rules.iter().any(|r| r.rule_type == t), "missing {t}");
        }
    }
}
