//! Deconjugation rule types and rule input.
//!
//! The default rules are embedded from `data/deconjugator.json` in both
//! platform binaries. An explicit path loads that file and preserves errors.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::borrow::Cow;
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

/// Load the embedded default rules when `path` is `None`.
/// Load the file at `path` when it is `Some`. Return read and parse errors
/// without a fallback.
pub fn load_rules(path: Option<&Path>) -> Result<Vec<Rule>> {
    let text = match path {
        Some(path) => Cow::Owned(std::fs::read_to_string(path)
            .with_context(|| format!("reading rules from {}", path.display()))?),
        None => Cow::Borrowed(include_str!("../../data/deconjugator.json")),
    };
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

    #[test]
    fn bundled_rules_exclude_substitutions() {
        let rules = load_rules(None).unwrap();
        assert!(rules.iter().all(|r| r.rule_type != "substitution"));
    }

    #[test]
    fn explicit_rules_replace_bundled_rules() {
        let path = std::env::temp_dir().join(format!("chibipop-custom-rules-{}.json", std::process::id()));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); } }
        let _cleanup = Cleanup(path.clone());
        std::fs::write(&path,
            r#"[{"type":"stdrule","con_end":"った","dec_end":"る","detail":"custom past"}]"#).unwrap();
        let deconjugator = crate::lookup::deconj::Deconjugator::new(load_rules(Some(&path)).unwrap());
        assert!(deconjugator.deconjugate("治った").iter()
            .any(|form| form.text == "治る" && form.process == ["custom past"]));
        assert!(!deconjugator.deconjugate("治って").iter().any(|form| form.text == "治る"));
    }

    #[test]
    fn explicit_rule_errors_do_not_use_bundled_rules() {
        let path = std::env::temp_dir().join(format!("chibipop-invalid-rules-{}.json", std::process::id()));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); } }
        let _cleanup = Cleanup(path.clone());
        std::fs::write(&path, "invalid JSON").unwrap();
        assert!(load_rules(Some(&path)).is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(load_rules(Some(&path)).is_err());
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
        let rules = load_rules(None).unwrap();
        for t in ["stdrule", "onlyfinalrule", "neverfinalrule", "rewriterule",
                  "contextrule"] {
            assert!(rules.iter().any(|r| r.rule_type == t), "missing {t}");
        }
    }
}
