//! What following a glossary link does.
//!
//! **One reason to change:** the allow-list, or the URL grammar a
//! dictionary's own cross-references are written in. Kept out of
//! [`style`](super::style) because an `href` is not a declaration and out
//! of [`gloss`](super::gloss) because the walk only asks the question - and
//! kept in core rather than in a bin because the same allow-list has to
//! hold for the Anki HTML renderer, which is not a bin at all.

use crate::controller::HitAction;
use super::style::hex_pair;

/// What following a glossary link
/// does.
///
/// A dictionary's own cross-references
/// carry no scheme and name their
/// target in a `query` parameter
/// (`?query=見出し語&wildcards=off`),
/// so they drill down in the panel
/// exactly as a headword's kanji does.
/// Its citations are `http` or
/// `https` and belong in a browser.
/// Anything else - `javascript:`,
/// `data:` - arrives from a file
/// chibipop did not write and earns no
/// target at all, which is the same
/// allow-list the Anki HTML renderer
/// applies. Whitespace and control
/// characters go first, because a URL
/// parser ignores them inside a URL
/// and a naive scheme check would not.
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
        // No scheme, so relative to a
        // dictionary archive nothing
        // here can serve.
        _ => false,
    };
    followable.then_some(HitAction::OpenUrl(clean))
}

/// A cross-reference's `query`
/// parameter, percent-decoded.
pub(super) fn query_param(url: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    let raw = query
        .split(['&', '#'])
        .find_map(|pair| pair.strip_prefix("query="))?;
    Some(percent_decode(raw))
}

/// `%XX` back to bytes, leaving
/// anything malformed as written.
///
/// Yomitan writes these with
/// `encodeURIComponent`, which spells
/// a space `%20` and never `+`, so `+`
/// is left alone: in a headword it is
/// a character rather than a space.
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
