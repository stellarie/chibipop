//! Deconjugation rule types and rule-file input.
//!
//! `data/deconjugator.json` mixes comment strings with rule objects. Each
//! `dec_end`, `con_end`, `dec_tag`, and `con_tag` field can contain one string or
//! a list of strings.

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
    // Serde never reads the `String` payload, but the untagged match needs it to
    // accept comment strings and reject other values. Do not replace it with
    // `()`. A JSON string cannot deserialize into `()`. Do not replace it with
    // `IgnoredAny`, because that type accepts every JSON value.
    #[allow(dead_code)]
    Comment(String),
}

/// Load rules. Discard comment strings and rules with type `substitution`.
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

