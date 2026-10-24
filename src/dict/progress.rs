//! Converts builder progress lines into text for people.
//!
//! [`build`](super::build::build) and the `build-dict` command write the same
//! short machine lines. Examples are `progress  12500 / 768636`, `term dict
//! [0] jitendex.zip`, `building  creating index`, and a final `wrote …` line.
//! Each settings surface that shows a build sends these lines to `friendly`.
//! The Win32 status area and the iced status area show the same text for each
//! build step. Both areas hide the final `wrote …` line because it names a
//! temporary file.

/// Converts one builder line into text for a person.
///
/// Returns `None` when a status area must hide the line.
pub fn friendly(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("progress") {
        let rest = rest.trim();
        let (n, t) = rest.split_once('/')?;
        let n: u64 = n.trim().parse().ok()?;
        let total: Option<u64> = t.trim().parse().ok();
        return Some(match total {
            Some(t) => format!("{} of {} entries…", format_thousands(n), format_thousands(t)),
            None => format!("{} entries…", format_thousands(n)),
        });
    }
    if line.starts_with("building") {
        return Some("Creating search index…".to_string());
    }
    let rest = line.strip_prefix("term dict").or_else(|| line.strip_prefix("freq dict"))?;
    let rest = rest.trim();
    // Remove the `[i]` prefix that the `build-dict` command adds.
    let name = match rest.strip_prefix('[').and_then(|a| a.split_once(']')) {
        Some((n, tail)) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => tail.trim(),
        _ => rest,
    };
    if name.is_empty() {
        return None;
    }
    Some(format!("Reading {name}…"))
}

/// Adds commas between groups of three digits.
fn format_thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

