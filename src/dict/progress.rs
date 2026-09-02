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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_archive_line_is_rendered_without_its_index() {
        assert_eq!(
            Some("Reading 01 [JA-EN] jitendex.zip…".to_string()),
            friendly("term dict  [0] 01 [JA-EN] jitendex.zip")
        );
        assert_eq!(
            Some("Reading [JA Freq] jiten_freq.zip…".to_string()),
            friendly("freq dict      [JA Freq] jiten_freq.zip")
        );
        assert_eq!(
            Some("Reading b.zip…".to_string()),
            friendly("term dict  [10] b.zip")
        );
    }

    #[test]
    fn a_progress_line_is_formatted_with_commas() {
        assert_eq!(
            Some("12,500 of 768,636 entries…".to_string()),
            friendly("progress  12500 / 768636")
        );
    }

    #[test]
    fn a_small_progress_has_no_commas() {
        assert_eq!(
            Some("500 of 3,000 entries…".to_string()),
            friendly("progress  500 / 3000")
        );
    }

    #[test]
    fn a_progress_line_with_unknown_total_works() {
        assert_eq!(
            Some("5,000 entries…".to_string()),
            friendly("progress  5000 / ?")
        );
    }

    #[test]
    fn a_building_line_is_passed_through() {
        assert_eq!(
            Some("Creating search index…".to_string()),
            friendly("building  creating index")
        );
    }

    /// The status areas must hide the builder's final line.
    #[test]
    fn the_builders_final_line_is_swallowed() {
        assert_eq!(
            None,
            friendly(r"wrote C:\Users\x\data\chibipop.sqlite.tmp: 3 entries, 5 term rows")
        );
        assert_eq!(None, friendly("wrote /home/x/.local/share/chibipop/chibipop.sqlite.building: 3 entries, 5 term rows"));
        assert_eq!(None, friendly("term dict  [0] "));
        assert_eq!(None, friendly(""));
    }
}
