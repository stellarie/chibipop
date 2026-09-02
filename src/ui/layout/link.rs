//! Resolves actions for a glossary link.
//!
//! **One reason to change:** change the allow-list or URL grammar for a dictionary cross-reference.
//! This module stays separate from [`style`](super::style).
//! `href` is not a style declaration.
//! It stays separate from [`gloss`](super::gloss) because the walk only asks for the result.
//! Core owns this module.
//! All renderers use one allow-list, and this includes the Anki HTML renderer.

use crate::controller::HitAction;
use super::style::hex_pair;

/// Resolves a glossary link to an allowed action.
///
/// A dictionary cross-reference has no scheme.
/// It names its target in a `query` parameter (`?query=見出し語&wildcards=off`).
/// The action uses the panel, like a headword's kanji.
/// A citation uses `http` or `https`, so the action opens a browser.
/// The function rejects every other scheme, such as `javascript:` and `data:`.
/// A dictionary can contain a file that chibipop did not write.
/// Therefore, the function uses the allow-list of the Anki HTML renderer.
///
/// The function removes whitespace and control characters first.
/// A URL parser ignores these characters inside a URL, but a simple scheme check does not.
pub(super) fn link_action(href: &str) -> Option<HitAction> {
    let clean: String =
        href.chars().filter(|c| !c.is_whitespace() && !c.is_control()).collect();
    if let Some(query) = query_param(&clean) {
        return (!query.is_empty()).then_some(HitAction::DrillDown(query));
    }
    let followable = match clean.find([':', '/', '?', '#']) {
        Some(at) if clean.as_bytes()[at] == b':' => {
            let scheme = &clean[..at];
            scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
        }
        // A relative path cannot identify a useful target in a dictionary archive.
        _ => false,
    };
    followable.then_some(HitAction::OpenUrl(clean))
}

/// Returns the percent-decoded `query` parameter of a cross-reference.
pub(super) fn query_param(url: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    let raw = query
        .split(['&', '#'])
        .find_map(|pair| pair.strip_prefix("query="))?;
    Some(percent_decode(raw))
}

/// Decodes `%XX` sequences to bytes and preserves malformed sequences.
///
/// Yomitan uses `encodeURIComponent`, which writes a space as `%20` and never writes `+`.
/// The function preserves `+` as a headword character, not a space.
pub(super) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], hex_pair(bytes, i + 1)) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
