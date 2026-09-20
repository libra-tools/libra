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

/// Decode one Git C-style quoted string (the inverse of the `quote_path` family
/// used by `status`/`ls-files` and by `--pathspec-from-file`'s non-NUL mode).
/// `Ok(None)` when `raw` is not quoted — callers then use it verbatim. On
/// success `raw` must be exactly one quoted string (`"..."`); `Err` describes a
/// malformed one (missing closing quote, trailing bytes after it, a trailing
/// backslash, an unsupported escape, or a non-UTF-8 result).
///
/// Escape handling follows Git's `unquote_c_style` for the named escapes
/// (`\a \b \f \n \r \t \v`, `\\`, `\"`) and for octal byte values, with two
/// documented divergences: it accepts one to three octal digits (Git requires
/// exactly three) and it rejects trailing bytes after the closing quote instead
/// of ignoring them (PSF-02 review P2-1).
pub fn decode_c_quoted(raw: &str) -> Result<Option<String>, String> {
    let bytes = raw.as_bytes();
    if bytes.first() != Some(&b'"') {
        return Ok(None);
    }
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 1usize;
    loop {
        let Some(&byte) = bytes.get(index) else {
            return Err("unterminated quoted path".to_string());
        };
        match byte {
            b'"' => {
                index += 1;
                if index != bytes.len() {
                    return Err("trailing bytes after the closing quote".to_string());
                }
                let text = String::from_utf8(decoded)
                    .map_err(|_| "quoted path is not valid UTF-8".to_string())?;
                return Ok(Some(text));
            }
            b'\\' => {
                index += 1;
                let Some(&escape) = bytes.get(index) else {
                    return Err("quoted path ends with a backslash".to_string());
                };
                match escape {
                    b'a' => decoded.push(0x07),
                    b'b' => decoded.push(0x08),
                    b'f' => decoded.push(0x0c),
                    b'n' => decoded.push(b'\n'),
                    b'r' => decoded.push(b'\r'),
                    b't' => decoded.push(b'\t'),
                    b'v' => decoded.push(0x0b),
                    b'\\' => decoded.push(b'\\'),
                    b'"' => decoded.push(b'"'),
                    digit @ b'0'..=b'7' => {
                        let mut value = u16::from(digit - b'0');
                        let mut digits = 1usize;
                        while digits < 3 {
                            match bytes.get(index + 1) {
                                Some(next @ b'0'..=b'7') => {
                                    index += 1;
                                    value = (value << 3) + u16::from(next - b'0');
                                    digits += 1;
                                }
                                _ => break,
                            }
                        }
                        // Git masks the accumulated octal value to one byte.
                        decoded.push((value & 0xff) as u8);
                    }
                    other => {
                        return Err(format!(
                            "unsupported quoted-path escape '\\{}'",
                            char::from(other)
                        ));
                    }
                }
            }
            other => decoded.push(other),
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_c_quoted, levenshtein, relative_date_at, short_display_hash};

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

    /// PSF-02 (plan-20260918): the shared decoder mirrors Git's
    /// `unquote_c_style` — unquoted input passes through, escapes and octal
    /// decode, and malformed quoting fails closed.
    #[test]
    fn decode_c_quoted_matches_git_unquote_semantics() {
        assert_eq!(decode_c_quoted("plain.txt"), Ok(None));
        assert_eq!(
            decode_c_quoted("\"qu\\\"ote.txt\""),
            Ok(Some("qu\"ote.txt".to_string()))
        );
        assert_eq!(
            decode_c_quoted("\"we ird.txt\""),
            Ok(Some("we ird.txt".to_string()))
        );
        assert_eq!(
            decode_c_quoted("\"tab\\there\\n\""),
            Ok(Some("tab\there\n".to_string()))
        );
        // Three octal digits decode and mask to one byte; one-to-three digits
        // are accepted (a documented divergence: Git requires exactly three).
        assert_eq!(
            decode_c_quoted("\"oct\\101l\""),
            Ok(Some("octAl".to_string()))
        );
        // Malformed forms fail closed.
        assert!(decode_c_quoted("\"unterminated").is_err());
        assert!(decode_c_quoted("\"trailing\" junk").is_err());
        assert!(decode_c_quoted("\"bad\\q\"").is_err());
        assert!(decode_c_quoted("\"ends\\\"").is_err());

        // A multi-byte UTF-8 name written as octal escapes — the shape
        // `status`/`ls-files` emit under `core.quotePath` — decodes back.
        assert_eq!(
            decode_c_quoted("\"\\303\\251.txt\""),
            Ok(Some("é.txt".to_string()))
        );
        // Round-trip against the forward `quote_pathname` helper
        // (ADR-PSF-02 §2 makes them inverses).
        let quoted = crate::command::status::quote_pathname(std::path::Path::new("é.txt"), true);
        assert_eq!(decode_c_quoted(&quoted), Ok(Some("é.txt".to_string())));
    }
}
