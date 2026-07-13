//! Shared text helpers for safe abbreviated display and fuzzy matching.

/// Default short hash width used in human-readable confirmations.
pub const SHORT_HASH_LEN: usize = 7;

/// Return a shortened display form of a hash-like string without assuming ASCII.
pub fn short_display_hash(hash: &str) -> &str {
    if hash.chars().count() <= SHORT_HASH_LEN {
        return hash;
    }

    let byte_idx = hash
        .char_indices()
        .nth(SHORT_HASH_LEN)
        .map(|(idx, _)| idx)
        .unwrap_or(hash.len());

    hash.get(..byte_idx).unwrap_or(hash)
}

/// Compute the Levenshtein edit distance between two strings.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (a, b) = if a.len() > b.len() {
        (&b, &a)
    } else {
        (&a, &b)
    };
    let mut prev: Vec<usize> = (0..=a.len()).collect();
    let mut curr = vec![0; a.len() + 1];
    for (i, cb) in b.iter().enumerate() {
        curr[0] = i + 1;
        for (j, ca) in a.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[a.len()]
}

/// Git-compatible relative date (`2 days ago`) for a Unix timestamp,
/// calculated against the current machine clock.
pub fn relative_date(ts: i64) -> String {
    relative_date_at(chrono::Local::now().timestamp(), ts)
}

/// Pure relative-date core (testable with an injected `now`), mirroring
/// git's `show_date_relative` thresholds and singular/plural wording.
pub fn relative_date_at(now: i64, ts: i64) -> String {
    if ts > now {
        return "in the future".to_string();
    }
    let unit = |n: u64, word: &str| {
        if n == 1 {
            format!("1 {word} ago")
        } else {
            format!("{n} {word}s ago")
        }
    };

    let mut diff = (now - ts) as u64;
    if diff < 90 {
        return unit(diff, "second");
    }
    diff = (diff + 30) / 60;
    if diff < 90 {
        return unit(diff, "minute");
    }
    diff = (diff + 30) / 60;
    if diff < 36 {
        return unit(diff, "hour");
    }
    let days = (diff + 12) / 24;
    if days < 14 {
        return unit(days, "day");
    }
    if days < 70 {
        return unit((days + 3) / 7, "week");
    }
    if days < 365 {
        return unit((days + 15) / 30, "month");
    }
    if days < 365 * 5 {
        let total_months = (days * 12 * 2 + 365) / (365 * 2);
        let years = total_months / 12;
        let months = total_months % 12;
        if months > 0 {
            let y = if years == 1 { "year" } else { "years" };
            let m = if months == 1 { "month" } else { "months" };
            return format!("{years} {y}, {months} {m} ago");
        }
        return unit(years, "year");
    }
    unit((days + 183) / 365, "year")
}

#[cfg(test)]
mod tests {
    use super::{levenshtein, relative_date_at, short_display_hash};

    const HOUR: i64 = 3600;
    const DAY: i64 = 86_400;

    #[test]
    fn short_display_hash_keeps_ascii_prefix() {
        assert_eq!(short_display_hash("1234567890"), "1234567");
    }

    #[test]
    fn short_display_hash_respects_utf8_boundaries() {
        assert_eq!(short_display_hash("éééééééé"), "ééééééé");
    }

    /// Inputs at or below `SHORT_HASH_LEN` (7 chars) are returned whole
    /// — the `<=` early-return branch. Pins the boundary: exactly 7
    /// chars passes through unchanged, 8 chars truncates to 7. A
    /// regression to `<` would drop the last char of a 7-char hash.
    #[test]
    fn short_display_hash_passes_through_short_and_boundary_inputs() {
        // Shorter than the limit → unchanged.
        assert_eq!(short_display_hash(""), "");
        assert_eq!(short_display_hash("abc"), "abc");
        // Exactly at the limit (7) → unchanged (inclusive boundary).
        assert_eq!(short_display_hash("1234567"), "1234567");
        // One over the limit (8) → truncated to the first 7.
        assert_eq!(short_display_hash("12345678"), "1234567");
        // UTF-8: exactly 7 multibyte chars → unchanged.
        assert_eq!(short_display_hash("ßßßßßßß"), "ßßßßßßß");
    }

    #[test]
    fn levenshtein_handles_basic_edge_cases() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("main", "maim"), 1);
        assert_eq!(levenshtein("feature", "featur"), 1);
    }

    /// Mirrors git's `show_date_relative` thresholds + rounding. Note git's
    /// `+30`/`+12` rounding offsets mean the minute/hour/day/week/month bands
    /// effectively start at 2 (e.g. 90s rounds straight to "2 minutes ago"), so
    /// "1 minute/hour/day ago" never appear — only "1 second ago" and the
    /// year forms are singular.
    #[test]
    fn relative_date_matches_git_thresholds() {
        let now = 1_000_000_000;
        let ago = |secs: i64| relative_date_at(now, now - secs);

        assert_eq!(ago(0), "0 seconds ago");
        assert_eq!(ago(1), "1 second ago");
        assert_eq!(ago(89), "89 seconds ago");
        assert_eq!(ago(90), "2 minutes ago");
        assert_eq!(ago(3600), "60 minutes ago");
        assert_eq!(ago(2 * HOUR), "2 hours ago");
        assert_eq!(ago(2 * DAY), "2 days ago");
        assert_eq!(ago(20 * DAY), "3 weeks ago");
        assert_eq!(ago(100 * DAY), "3 months ago");
        assert_eq!(ago(400 * DAY), "1 year, 1 month ago");
        assert_eq!(ago(365 * 6 * DAY), "6 years ago");
    }

    #[test]
    fn relative_date_future_is_guarded() {
        assert_eq!(relative_date_at(1000, 2000), "in the future");
    }
}
