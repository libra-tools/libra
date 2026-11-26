//! Git-faithful commit-message trailer parsing (lore.md §1.9) — the shared
//! substrate for `log --trailer`/`--only-trailers`, `shortlog
//! --group=trailer:<key>`, and revision metadata (lore.md 1.10, landed:
//! `metadata --revision` reads trailers through
//! `parse_trailers_with_recognized`'s extra-keys hook).
//!
//! The rules mirror git's `trailer.c` (`git-interpret-trailers`), empirically
//! cross-checked against Git 2.43:
//!
//! - The candidate block is the LAST paragraph of the message (comment lines
//!   are transparent while locating it), and the first paragraph — the title —
//!   can never be a trailer block, so a single-paragraph message has none.
//! - A trailer line is `Key: value` where the key is one or more ASCII
//!   alphanumerics or `-` (git's `find_separator` accepts nothing else — a
//!   `Change_Id:` or non-ASCII key line is NOT a trailer), with optional
//!   spaces/tabs between the key and the `:`. An empty value is legal.
//! - A line starting with a space/tab continues the previous trailer
//!   (RFC-822 folding). For the qualification arithmetic, an attached
//!   continuation counts as NEITHER a trailer nor a non-trailer line (git
//!   resets `possible_continuation_lines` when a trailer is found); an orphan
//!   continuation counts as a non-trailer line.
//! - `#`-comment lines are ignored entirely (output and arithmetic).
//! - The block qualifies iff every counted line is a trailer, OR it contains a
//!   RECOGNIZED trailer (`Signed-off-by: `, `(cherry picked from commit `,
//!   plus any caller-supplied keys) and `trailer_lines * 3 >= non_trailer_lines`
//!   (git's "at least 25% trailers" rule).
//! - The `(cherry picked from commit …)` line is recognized for qualification
//!   and rendered by the RAW block accessor, but is NOT surfaced as a
//!   [`Trailer`] (it has no key/separator; exposing it as a pseudo-trailer
//!   would put a whitespace-and-parens "key" in structured output).
//!
//! Intentional simplifications vs `git-interpret-trailers` (documented in
//! docs/development/commands/log.md): no `trailer.separators` /
//! `trailer.<token>.*` config, no custom `core.commentChar`, no `---` divider
//! rule (patch-input only, never stored messages).

use crate::utils::commit_msg::parse_commit_msg;

/// One parsed trailer: `key` in its original spelling, `value` unfolded
/// (continuation lines joined with single spaces) and trimmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trailer {
    pub key: String,
    pub value: String,
}

impl Trailer {
    /// ASCII case-insensitive key comparison (`--trailer reviewed-by` matches
    /// `Reviewed-by`), matching the existing shortlog behavior.
    pub fn key_matches(&self, key: &str) -> bool {
        self.key.eq_ignore_ascii_case(key)
    }
}

/// Recognized Git-generated trailer prefixes (case-sensitive, exactly as
/// git's `trailer.c` matches them).
const RECOGNIZED_PREFIXES: [&str; 2] = ["Signed-off-by: ", "(cherry picked from commit "];

/// Whether `line` is a comment line (git's default `#` comment char).
fn is_comment(line: &str) -> bool {
    line.starts_with('#')
}

/// Whether `line` is blank (empty or whitespace-only).
fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// Split a candidate line at git's trailer separator: the key must be one or
/// more ASCII alphanumerics or `-`, optionally followed by spaces/tabs, then
/// `:`. Returns `(key, value)` with the value trimmed.
fn split_trailer_line(line: &str) -> Option<(&str, &str)> {
    let mut key_end = 0;
    for (idx, c) in line.char_indices() {
        if c.is_ascii_alphanumeric() || c == '-' {
            key_end = idx + c.len_utf8();
        } else {
            break;
        }
    }
    if key_end == 0 {
        return None;
    }
    let rest = &line[key_end..];
    let after_ws = rest.trim_start_matches([' ', '\t']);
    let value = after_ws.strip_prefix(':')?;
    Some((&line[..key_end], value.trim()))
}

/// The message's lines with any gpgsig block stripped and CRLF tolerated.
fn message_lines(message: &str) -> Vec<&str> {
    let (message, _sig) = parse_commit_msg(message);
    message
        .lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

/// Locate the candidate trailer block: the last run of non-blank lines
/// (comment lines transparent — a trailing comment-only paragraph does not
/// hide the block), which must be preceded by a blank line and must not be
/// part of the first paragraph. Returns the line-index range.
fn locate_block(lines: &[&str]) -> Option<(usize, usize)> {
    // Walk back over trailing blanks AND comment lines to find the block end.
    let mut end = lines.len();
    while end > 0 && (is_blank(lines[end - 1]) || is_comment(lines[end - 1])) {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    // Walk back to the blank line preceding the block.
    let mut start = end;
    while start > 0 && !is_blank(lines[start - 1]) {
        start -= 1;
    }
    // Must be preceded by a blank line — i.e. not the very first paragraph
    // (the title can never be a trailer block).
    if start == 0 {
        return None;
    }
    Some((start, end))
}

/// Shared analysis: classify the candidate block and return, when it
/// QUALIFIES, `(raw_non_comment_lines, structured_trailers)`. One pass feeds
/// [`parse_trailers`], [`trailer_block`], and [`ends_with_trailer_block`] so
/// they can never disagree about qualification (e.g. a mixed block whose only
/// recognized line is a separator-less cherry-pick line qualifies for ALL of
/// them, even though it yields no structured [`Trailer`]).
fn analyze_block(
    message: &str,
    extra_recognized_keys: &[&str],
) -> Option<(Vec<String>, Vec<Trailer>)> {
    let lines = message_lines(message);
    let (start, end) = locate_block(&lines)?;

    let mut raw: Vec<String> = Vec::new();
    let mut trailers: Vec<Trailer> = Vec::new();
    let mut trailer_lines = 0usize;
    let mut non_trailer_lines = 0usize;
    let mut recognized = false;

    for line in &lines[start..end] {
        if is_comment(line) {
            continue; // transparent: neither output nor arithmetic
        }
        raw.push(line.to_string());
        if line.starts_with(' ') || line.starts_with('\t') {
            // Continuation: attaches to the previous trailer and counts as
            // NEITHER kind (git resets possible_continuation_lines); an orphan
            // continuation is a non-trailer line.
            if let Some(last) = trailers.last_mut() {
                let fragment = line.trim();
                if !fragment.is_empty() {
                    if !last.value.is_empty() {
                        last.value.push(' ');
                    }
                    last.value.push_str(fragment);
                }
            } else {
                non_trailer_lines += 1;
            }
            continue;
        }
        if RECOGNIZED_PREFIXES
            .iter()
            .any(|prefix| line.starts_with(prefix))
        {
            recognized = true;
            trailer_lines += 1;
            // The cherry-pick line has no key/separator: it qualifies the
            // block and shows in the raw block, but is not a Trailer.
            if let Some((key, value)) = split_trailer_line(line) {
                trailers.push(Trailer {
                    key: key.to_string(),
                    value: value.to_string(),
                });
            }
            continue;
        }
        match split_trailer_line(line) {
            Some((key, value)) => {
                trailer_lines += 1;
                if extra_recognized_keys
                    .iter()
                    .any(|extra| key.eq_ignore_ascii_case(extra))
                {
                    recognized = true;
                }
                trailers.push(Trailer {
                    key: key.to_string(),
                    value: value.to_string(),
                });
            }
            None => non_trailer_lines += 1,
        }
    }

    // Qualification: all counted lines are trailers, or a recognized trailer
    // is present and trailers are at least 25% of the counted lines.
    let qualifies = trailer_lines > 0
        && (non_trailer_lines == 0 || (recognized && trailer_lines * 3 >= non_trailer_lines));
    if qualifies {
        Some((raw, trailers))
    } else {
        None
    }
}

/// Parse the message's qualifying trailer block into [`Trailer`]s (empty when
/// no block qualifies). See the module docs for the exact rules.
pub fn parse_trailers(message: &str) -> Vec<Trailer> {
    parse_trailers_with_recognized(message, &[])
}

/// [`parse_trailers`] with additional RECOGNIZED keys (ASCII case-insensitive)
/// that strengthen the qualification rule — the hook lore.md 1.10 uses for
/// config-recognized trailer keys. The extra keys do not change what parses as
/// a trailer line, only whether a mixed block qualifies.
pub fn parse_trailers_with_recognized(
    message: &str,
    extra_recognized_keys: &[&str],
) -> Vec<Trailer> {
    analyze_block(message, extra_recognized_keys)
        .map(|(_, trailers)| trailers)
        .unwrap_or_default()
}

/// The qualifying trailer block's RAW lines (comment lines excluded), for
/// verbatim `%(trailers)`-style rendering — includes separator-less recognized
/// lines like `(cherry picked from commit …)` that [`parse_trailers`] omits.
/// `None` when no block qualifies. Shares one analysis pass with
/// [`parse_trailers`], so the two can never disagree about qualification.
pub fn trailer_block(message: &str) -> Option<Vec<String>> {
    analyze_block(message, &[]).map(|(raw, _)| raw)
}

/// Whether the message already ends in a qualifying trailer block — used by
/// the commit writer to decide between appending INTO the block (single
/// newline) and opening a new paragraph (blank line), so `commit -s
/// --trailer` produces ONE Git-parseable block.
pub fn ends_with_trailer_block(message: &str) -> bool {
    analyze_block(message, &[]).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_paragraph_message_has_no_trailers() {
        assert!(parse_trailers("Key: value").is_empty());
        assert!(parse_trailers("Signed-off-by: A <a@b>").is_empty());
        assert!(parse_trailers("subject line only\n").is_empty());
    }

    #[test]
    fn all_trailer_final_paragraph_qualifies() {
        let msg = "subject\n\nReviewed-by: A <a@b>\nChange-Id: I123\n";
        let trailers = parse_trailers(msg);
        assert_eq!(trailers.len(), 2);
        assert_eq!(trailers[0].key, "Reviewed-by");
        assert_eq!(trailers[1].value, "I123");
    }

    #[test]
    fn unrecognized_mixed_block_does_not_qualify() {
        // One unrecognized trailer + prose: no recognized trailer → reject.
        let msg = "subject\n\nprose line\nReviewed-by: A\n";
        assert!(parse_trailers(msg).is_empty());
    }

    #[test]
    fn recognized_mixed_block_uses_25_percent_rule() {
        // SOB + 3 prose: 1*3 >= 3 → qualifies.
        let msg = "subject\n\np1\np2\np3\nSigned-off-by: A <a@b>\n";
        assert_eq!(parse_trailers(msg).len(), 1);
        // SOB + 4 prose: 1*3 >= 4 fails → rejected.
        let msg = "subject\n\np1\np2\np3\np4\nSigned-off-by: A <a@b>\n";
        assert!(parse_trailers(msg).is_empty());
    }

    #[test]
    fn continuation_counts_as_neither_side() {
        // SOB + attached continuation + 4 prose: still 1*3 >= 4 → rejected
        // (the continuation must NOT count as a trailer line).
        let msg = "subject\n\nSigned-off-by: A\n more\np1\np2\np3\np4\n";
        assert!(parse_trailers(msg).is_empty());
        // Attached continuation unfolds into the value.
        let msg = "subject\n\nKey: long\n value continues\n";
        let trailers = parse_trailers(msg);
        assert_eq!(trailers[0].value, "long value continues");
        // Orphan continuation is a non-trailer line: block of just it → none.
        let msg = "subject\n\n indented orphan\n";
        assert!(parse_trailers(msg).is_empty());
    }

    #[test]
    fn key_charset_is_git_exact() {
        // Alnum + dash only; underscore and non-ASCII keys are NOT trailers.
        assert!(parse_trailers("s\n\nKey-1: v\n").len() == 1);
        assert!(parse_trailers("s\n\nChange_Id: v\n").is_empty());
        assert!(parse_trailers("s\n\nCafé: v\n").is_empty());
        assert!(parse_trailers("s\n\nTwo words: v\n").is_empty());
        // Whitespace between key and separator is fine; empty value legal.
        let t = parse_trailers("s\n\nKey  : v\n");
        assert_eq!(t[0].key, "Key");
        let t = parse_trailers("s\n\nKey:\n");
        assert_eq!(t[0].value, "");
    }

    #[test]
    fn comment_lines_are_transparent_everywhere() {
        // In classification…
        let msg = "s\n\nKey: v\n# comment\n";
        assert_eq!(parse_trailers(msg).len(), 1);
        // …and in block LOCATION: a trailing comment-only paragraph does not
        // hide the trailer block.
        let msg = "s\n\nKey: v\n\n# trailing comment\n";
        assert_eq!(parse_trailers(msg).len(), 1);
    }

    #[test]
    fn cherry_pick_line_qualifies_but_is_not_a_trailer() {
        let msg = "s\n\n(cherry picked from commit abc123)\n";
        assert!(parse_trailers(msg).is_empty());
        let block = trailer_block(msg).expect("block qualifies");
        assert_eq!(block.len(), 1);
        assert!(block[0].starts_with("(cherry picked"));
        // Mixed with a real trailer, both appear in the raw block; only the
        // real one is a Trailer.
        let msg = "s\n\nKey: v\n(cherry picked from commit abc)\n";
        assert_eq!(parse_trailers(msg).len(), 1);
        assert_eq!(trailer_block(msg).unwrap().len(), 2);
    }

    #[test]
    fn extra_recognized_keys_strengthen_qualification() {
        let msg = "s\n\np1\nCo-authored-by: A\n";
        assert!(parse_trailers(msg).is_empty());
        let t = parse_trailers_with_recognized(msg, &["co-authored-by"]);
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn mixed_qualifying_cherry_pick_block_agrees_across_accessors() {
        // Recognized cherry-pick line + 3 prose lines: 1*3 >= 3 qualifies.
        // parse_trailers yields no structured Trailer, but trailer_block and
        // ends_with_trailer_block must still see the qualifying block (shared
        // analysis — the accessors can never disagree).
        let msg = "subject\n\np1\np2\np3\n(cherry picked from commit abc)\n";
        assert!(parse_trailers(msg).is_empty());
        let block = trailer_block(msg).expect("mixed recognized block qualifies");
        assert_eq!(block.len(), 4);
        assert!(ends_with_trailer_block(msg));
        // One more prose line: 1*3 >= 4 fails → all accessors reject.
        let msg = "subject\n\np1\np2\np3\np4\n(cherry picked from commit abc)\n";
        assert!(trailer_block(msg).is_none());
        assert!(!ends_with_trailer_block(msg));
    }

    #[test]
    fn ends_with_trailer_block_detects_blocks() {
        assert!(ends_with_trailer_block("s\n\nKey: v\n"));
        assert!(ends_with_trailer_block(
            "s\n\n(cherry picked from commit abc)\n"
        ));
        assert!(!ends_with_trailer_block("s\n\nplain paragraph\n"));
        assert!(!ends_with_trailer_block("single paragraph"));
    }
}
