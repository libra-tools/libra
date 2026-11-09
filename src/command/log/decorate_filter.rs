//! Decoration ref-pattern semantics for `--decorate-refs` /
//! `--decorate-refs-exclude` (ADR-LO-07).
//!
//! Patterns are matched against the full ref name (`refs/heads/…`,
//! `refs/tags/…`, `refs/remotes/…`): each pattern gains an implicit `refs/`
//! prefix (unless it already starts with `refs/` or names `HEAD`), and one
//! trailing `/` is stripped. A pattern containing `*`, `?` or `[` is a
//! wildcard match — a byte-wise port of Git `wildmatch` with flags = 0, so
//! `*` and `?` cross `/` — and every other pattern is an exact match or a
//! prefix match that ends at a `/` component boundary. Exclude patterns win
//! over include patterns; the caller applies the decision order.

/// Whether a pattern contains wildcard specials (`*`, `?`, `[`), like Git's
/// `has_glob_specials`. A backslash is not a special on its own.
pub(crate) fn has_glob_specials(pattern: &str) -> bool {
    pattern
        .bytes()
        .any(|byte| matches!(byte, b'*' | b'?' | b'['))
}

/// Normalize one user pattern: strip one trailing `/`, then qualify with the
/// implicit `refs/` prefix unless the pattern already starts with `refs/` or
/// names `HEAD`.
pub(crate) fn normalize_ref_pattern(pattern: &str) -> String {
    let trimmed = pattern.strip_suffix('/').unwrap_or(pattern);
    if trimmed == "HEAD" || trimmed.starts_with("refs/") {
        trimmed.to_string()
    } else {
        format!("refs/{trimmed}")
    }
}

/// Match one decoration ref pattern against a full ref name.
pub(crate) fn ref_pattern_matches(pattern: &str, full_ref_name: &str) -> bool {
    let normalized = normalize_ref_pattern(pattern);
    if has_glob_specials(&normalized) {
        wildmatch(&normalized, full_ref_name)
    } else if normalized.ends_with('/') {
        // `refs/`-style prefix: everything underneath matches.
        full_ref_name.starts_with(&normalized)
    } else {
        full_ref_name == normalized
            || full_ref_name
                .strip_prefix(&normalized)
                .is_some_and(|rest| rest.starts_with('/'))
    }
}

/// Git `wildmatch` with flags = 0: `*` matches any run (crossing `/`), `?`
/// matches one byte (crossing `/`), `[...]` supports `!`/`^` negation,
/// ranges, `\` escapes, POSIX `[:name:]` classes, `[=c=]`/`[.c.]` elements
/// and the `&&` set-union operator. A backslash outside a class makes the
/// next byte literal. An unterminated class or an unknown POSIX class name
/// never matches. Worst case is O(pattern × text) via memoized backtracking.
pub(crate) fn wildmatch(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let mut memo = vec![vec![None; text.len() + 1]; pattern.len() + 1];
    match_from(pattern, text, 0, 0, &mut memo)
}

fn match_from(
    pattern: &[u8],
    text: &[u8],
    pi: usize,
    ti: usize,
    memo: &mut [Vec<Option<bool>>],
) -> bool {
    if let Some(result) = memo[pi][ti] {
        return result;
    }
    let result = match_loop(pattern, text, pi, ti, memo);
    memo[pi][ti] = Some(result);
    result
}

fn match_loop(
    pattern: &[u8],
    text: &[u8],
    mut pi: usize,
    mut ti: usize,
    memo: &mut [Vec<Option<bool>>],
) -> bool {
    while pi < pattern.len() {
        match pattern[pi] {
            b'*' => {
                while pi < pattern.len() && pattern[pi] == b'*' {
                    pi += 1;
                }
                let mut split = ti;
                loop {
                    if match_from(pattern, text, pi, split, memo) {
                        return true;
                    }
                    if split >= text.len() {
                        return false;
                    }
                    split += 1;
                }
            }
            b'?' => {
                if ti >= text.len() {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
            b'[' => {
                if ti >= text.len() {
                    return false;
                }
                match match_class(pattern, pi, text[ti]) {
                    (true, Some(next)) => {
                        pi = next;
                        ti += 1;
                    }
                    (false, Some(_)) | (_, None) => return false,
                }
            }
            b'\\' => {
                pi += 1;
                let literal = if pi < pattern.len() {
                    pattern[pi]
                } else {
                    b'\\'
                };
                if ti >= text.len() || text[ti] != literal {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
            literal => {
                if ti >= text.len() || text[ti] != literal {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
        }
    }
    ti == text.len()
}

/// Parse the character class starting at `start` (the `[`) against one byte.
/// Returns `(matched, Some(index_after_class))`, or `(false, None)` when the
/// class is unterminated (which aborts the whole match, like `WM_ABORT_ALL`).
fn match_class(pattern: &[u8], start: usize, ch: u8) -> (bool, Option<usize>) {
    let mut i = start + 1;
    let mut negate = false;
    if i < pattern.len() && (pattern[i] == b'!' || pattern[i] == b'^') {
        negate = true;
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < pattern.len() {
        if pattern[i] == b']' && !first {
            return (negate ^ matched, Some(i + 1));
        }
        first = false;
        match pattern[i] {
            b'[' => match pattern.get(i + 1) {
                Some(b':') => {
                    // POSIX character class `[:name:]`.
                    if let Some(rel_end) = pattern[i + 2..].iter().position(|&b| b == b':')
                        && pattern.get(i + 2 + rel_end + 1) == Some(&b']')
                    {
                        let name = &pattern[i + 2..i + 2 + rel_end];
                        if posix_class_matches(name, ch) {
                            matched = true;
                        }
                        i += 2 + rel_end + 2;
                    } else {
                        matched |= ch == b'[';
                        i += 1;
                    }
                }
                Some(b'=') => {
                    // `[=c=]` collating equivalence: literal c.
                    if pattern.get(i + 3) == Some(&b'=')
                        && pattern.get(i + 4) == Some(&b']')
                        && pattern[i + 2] == ch
                    {
                        matched = true;
                        i += 5;
                    } else {
                        matched |= ch == b'[';
                        i += 1;
                    }
                }
                Some(b'.') => {
                    // `[.c.]` collating element: literal c.
                    if pattern.get(i + 3) == Some(&b'.')
                        && pattern.get(i + 4) == Some(&b']')
                        && pattern[i + 2] == ch
                    {
                        matched = true;
                        i += 5;
                    } else {
                        matched |= ch == b'[';
                        i += 1;
                    }
                }
                _ => {
                    if ch == b'[' {
                        matched = true;
                    }
                    i += 1;
                }
            },
            b'\\' => {
                let literal = pattern.get(i + 1).copied().unwrap_or(b'\\');
                if literal == ch {
                    matched = true;
                }
                i += 2;
            }
            b'&' if pattern.get(i + 1) == Some(&b'&') => {
                // `&&` set-union operator: `[a&&b]` matches both a and b.
                i += 2;
            }
            literal => {
                if pattern.get(i + 1) == Some(&b'-')
                    && pattern.get(i + 2).is_some_and(|&high| high != b']')
                {
                    let high = pattern[i + 2];
                    if literal <= ch && ch <= high {
                        matched = true;
                    }
                    i += 3;
                } else {
                    if literal == ch {
                        matched = true;
                    }
                    i += 1;
                }
            }
        }
    }
    (false, None)
}

/// The POSIX classes Git's `wildmatch` recognizes. An unknown name matches
/// nothing.
fn posix_class_matches(name: &[u8], ch: u8) -> bool {
    match name {
        b"alnum" => ch.is_ascii_alphanumeric(),
        b"alpha" => ch.is_ascii_alphabetic(),
        b"blank" => matches!(ch, b' ' | b'\t'),
        b"cntrl" => ch.is_ascii_control(),
        b"digit" => ch.is_ascii_digit(),
        b"graph" => (b'!'..=b'~').contains(&ch),
        b"lower" => ch.is_ascii_lowercase(),
        b"print" => (b' '..=b'~').contains(&ch),
        b"punct" => {
            ch.is_ascii()
                && !ch.is_ascii_alphanumeric()
                && !ch.is_ascii_control()
                && !ch.is_ascii_whitespace()
        }
        b"space" => ch.is_ascii_whitespace(),
        b"upper" => ch.is_ascii_uppercase(),
        b"xdigit" => ch.is_ascii_hexdigit(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::wildmatch;

    #[test]
    fn lo07_wm_negated_class_bang() {
        assert!(wildmatch("refs/heads/[!z]a", "refs/heads/aa"));
    }

    #[test]
    fn lo07_wm_negated_class_caret() {
        assert!(!wildmatch("refs/heads/[^z]a", "refs/heads/za"));
    }

    #[test]
    fn lo07_wm_range() {
        assert!(wildmatch("refs/heads/[a-c]a", "refs/heads/ba"));
    }

    #[test]
    fn lo07_wm_double_star() {
        assert!(wildmatch("refs/**", "refs/heads/aa"));
    }

    #[test]
    fn lo07_wm_question_crosses_slash() {
        assert!(wildmatch("refs/heads?aa", "refs/heads/aa"));
    }

    #[test]
    fn lo07_wm_posix_digit() {
        assert!(wildmatch("refs/heads/[[:digit:]]", "refs/heads/7"));
    }

    #[test]
    fn lo07_wm_bogus_posix_class() {
        assert!(!wildmatch("refs/heads/[[:bogus:]]", "refs/heads/a"));
    }

    #[test]
    fn lo07_wm_escaped_bracket_in_class() {
        assert!(wildmatch("refs/heads/[\\]]", "refs/heads/]"));
    }
}
