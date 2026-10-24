//! The deconjugator applies the rule set with a breadth-first search (BFS) to a
//! fixpoint.
//!
//! This code ports weikipop's `src/dictionary/deconjugator.py`. It preserves
//! two inherited behaviors:
//!
//! 1. `max_len` uses only `dec_end`. The sixth `dec_tag` in the mizenkei rule
//!    (`vs-i`) stays unreachable, so that path tags `する` as `vk`.
//!    The correct `vs-i` form also appears, so this quirk is harmless in practical lookup.
//! 2. The code ignores `contextrule` predicates. Those two rules match too much.
//!
//! Tests pin both behaviors. A change to either behavior changes the semantics.
//! It is not a refactor.

use crate::lookup::rules::Rule;
use std::collections::HashSet;

pub const MAX_DECONJ_ITERATIONS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Form {
    pub text: String,
    pub process: Vec<String>,
    pub tags: Vec<String>,
}

impl Form {
    pub fn seed(text: &str) -> Self {
        Form { text: text.to_string(), process: Vec::new(), tags: Vec::new() }
    }
}

pub struct Deconjugator {
    rules: Vec<Rule>,
}

impl Deconjugator {
    pub fn new(rules: Vec<Rule>) -> Self {
        Deconjugator { rules }
    }

    pub fn deconjugate(&self, text: &str) -> HashSet<Form> {
        let clean = text.trim();
        if clean.is_empty() {
            return HashSet::new();
        }

        let mut processed: HashSet<Form> = HashSet::new();
        let mut novel: HashSet<Form> = HashSet::new();
        novel.insert(Form::seed(clean));

        let mut iteration = 0usize;
        while !novel.is_empty() {
            iteration += 1;
            if iteration > MAX_DECONJ_ITERATIONS {
                break;
            }

            let mut new_novel: HashSet<Form> = HashSet::new();
            for form in &novel {
                for rule in &self.rules {
                    if rule.rule_type == "onlyfinalrule" && !form.tags.is_empty() {
                        continue;
                    }
                    if rule.rule_type == "neverfinalrule" && form.tags.is_empty() {
                        continue;
                    }
                    for f in self.apply_rule(form, rule) {
                        if !processed.contains(&f)
                            && !novel.contains(&f)
                            && !new_novel.contains(&f)
                        {
                            new_novel.insert(f);
                        }
                    }
                }
            }

            processed.extend(novel);
            novel = new_novel;
        }

        // The reference discards the frontier when the iteration cap stops the loop.
        // It always adds the seed again. Keep this behavior.
        processed.insert(Form::seed(clean));
        processed
    }

    fn apply_rule(&self, form: &Form, rule: &Rule) -> Vec<Form> {
        let dec_ends = rule.dec_end.as_slice();
        let con_ends = rule.con_end.as_slice();
        let empty: &[String] = &[];
        let dec_tags = rule.dec_tag.as_ref().map(|t| t.as_slice()).unwrap_or(empty);
        let con_tags = rule.con_tag.as_ref().map(|t| t.as_slice()).unwrap_or(empty);

        // Inherited quirk #1: `dec_end` alone sets the iteration count.
        let max_len = dec_ends.len().max(1);

        let is_starter = matches!(
            rule.rule_type.as_str(),
            "stdrule" | "rewriterule" | "onlyfinalrule" | "contextrule"
        );

        let mut results = Vec::new();
        for i in 0..max_len {
            let con_end = if con_ends.is_empty() {
                ""
            } else {
                con_ends[i % con_ends.len()].as_str()
            };
            let dec_end = if dec_ends.is_empty() {
                ""
            } else {
                dec_ends[i % dec_ends.len()].as_str()
            };
            let con_tag = if con_tags.is_empty() {
                None
            } else {
                Some(con_tags[i % con_tags.len()].as_str())
            };
            let dec_tag = if dec_tags.is_empty() {
                None
            } else {
                Some(dec_tags[i % dec_tags.len()].as_str())
            };

            if !form.text.ends_with(con_end) {
                continue;
            }

            let tag_match = if form.tags.is_empty() {
                is_starter
            } else {
                form.tags.last().map(String::as_str) == con_tag
            };
            if !tag_match {
                continue;
            }

            if rule.rule_type == "rewriterule" && form.text != con_end {
                continue;
            }

            // The `ends_with` check guarantees a character boundary. Byte slices are safe.
            let new_text = if con_end.is_empty() {
                format!("{}{}", form.text, dec_end)
            } else {
                format!(
                    "{}{}",
                    &form.text[..form.text.len() - con_end.len()],
                    dec_end
                )
            };

            let mut process = form.process.clone();
            process.push(rule.detail.clone());

            let tags = if form.tags.is_empty() {
                match dec_tag {
                    Some(t) => vec![t.to_string()],
                    None => Vec::new(),
                }
            } else {
                let mut t = form.tags[..form.tags.len() - 1].to_vec();
                if let Some(d) = dec_tag {
                    t.push(d.to_string());
                }
                t
            };

            results.push(Form { text: new_text, process, tags });
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookup::rules::load_rules;

    fn deconjugator() -> Deconjugator {
        Deconjugator::new(load_rules(None).unwrap())
    }

    fn reaches(d: &Deconjugator, input: &str, want: &str) -> bool {
        d.deconjugate(input).iter().any(|f| f.text == want)
    }

    #[test]
    fn past_plain_ichidan() {
        assert!(reaches(&deconjugator(), "食べた", "食べる"));
    }

    #[test]
    fn causative_passive_past() {
        assert!(reaches(&deconjugator(), "食べさせられた", "食べる"));
    }

    #[test]
    fn negative_past_godan() {
        assert!(reaches(&deconjugator(), "行かなかった", "行く"));
    }

    #[test]
    fn te_iru_progressive() {
        assert!(reaches(&deconjugator(), "見ている", "見る"));
    }

}
