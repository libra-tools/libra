//! Shared `--grep` pattern handling for `log`, `rev-list`, and `grep` (ADR-LG-02, ADR-LG-09).
//!
//! Git's default commit-message grep is POSIX basic regex, not a plain
//! substring and not Rust regex syntax. This module owns dialect resolution,
//! BRE/ERE/Perl compilation, runtime matching, and the "only message body"
//! scope so commands share one implementation.

use crate::utils::error::{CliError, CliResult, StableErrorCode};

/// Default limit for backtracking steps in the Perl (`fancy-regex`) engine.
pub const DEFAULT_BACKTRACK_LIMIT: usize = 250_000;

/// Maximum allowed pattern length in bytes.
pub const MAX_PATTERN_LEN: usize = 32 * 1024;

/// The `--grep` pattern dialect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrepPatternType {
    /// POSIX basic regex (Git's default; GNU `\+`, `\?`, `\|` extensions).
    Basic,
    /// POSIX extended regex.
    Extended,
    /// Literal byte string.
    Fixed,
    /// Perl-compatible regex (`fancy-regex`).
    Perl,
}

impl GrepPatternType {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "basic" => Some(Self::Basic),
            "extended" => Some(Self::Extended),
            "fixed" => Some(Self::Fixed),
            "perl" => Some(Self::Perl),
            _ => None,
        }
    }

    pub fn as_config_value(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Extended => "extended",
            Self::Fixed => "fixed",
            Self::Perl => "perl",
        }
    }
}

/// A compiled matcher wrapping either a linear DFA (`regex::Regex`) or a
/// backtracking Perl engine (`fancy_regex::Regex`).
#[derive(Clone, Debug)]
pub struct GrepMatcher {
    pattern: String,
    inner: MatcherInner,
}

#[derive(Clone, Debug)]
enum MatcherInner {
    Linear(regex::Regex),
    Perl(fancy_regex::Regex),
}

impl GrepMatcher {
    /// The original pattern string before dialect translation.
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Check if `text` matches this pattern.
    pub fn is_match(&self, text: &str) -> CliResult<bool> {
        match &self.inner {
            MatcherInner::Linear(re) => Ok(re.is_match(text)),
            MatcherInner::Perl(re) => re.is_match(text).map_err(|err| {
                CliError::fatal(format!(
                    "regex execution failed for pattern '{}': {err}",
                    self.pattern
                ))
                .with_stable_code(StableErrorCode::InternalInvariant)
            }),
        }
    }

    /// Find the first match span `(start, end)` in `text`.
    pub fn find(&self, text: &str) -> CliResult<Option<(usize, usize)>> {
        match &self.inner {
            MatcherInner::Linear(re) => Ok(re.find(text).map(|m| (m.start(), m.end()))),
            MatcherInner::Perl(re) => match re.find(text) {
                Ok(Some(m)) => Ok(Some((m.start(), m.end()))),
                Ok(None) => Ok(None),
                Err(err) => Err(CliError::fatal(format!(
                    "regex execution failed for pattern '{}': {err}",
                    self.pattern
                ))
                .with_stable_code(StableErrorCode::InternalInvariant)),
            },
        }
    }

    /// Find all non-overlapping match spans `(start, end)` in `text`.
    pub fn find_all(&self, text: &str) -> CliResult<Vec<(usize, usize)>> {
        match &self.inner {
            MatcherInner::Linear(re) => {
                Ok(re.find_iter(text).map(|m| (m.start(), m.end())).collect())
            }
            MatcherInner::Perl(re) => {
                let mut spans = Vec::new();
                for item in re.find_iter(text) {
                    match item {
                        Ok(m) => spans.push((m.start(), m.end())),
                        Err(err) => {
                            return Err(CliError::fatal(format!(
                                "regex execution failed for pattern '{}': {err}",
                                self.pattern
                            ))
                            .with_stable_code(StableErrorCode::InternalInvariant));
                        }
                    }
                }
                Ok(spans)
            }
        }
    }

    #[cfg(test)]
    pub fn from_regex(re: regex::Regex) -> Self {
        Self {
            pattern: re.as_str().to_string(),
            inner: MatcherInner::Linear(re),
        }
    }
}

/// Translate POSIX basic regex to the extended dialect the `regex` crate
/// speaks: unescaped ERE operators become escaped literals, while the BRE
/// escaped forms (`\(`, `\+`, …) become their ERE operators.
pub fn bre_to_ere(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                Some(next @ ('(' | ')' | '{' | '}' | '+' | '?' | '|')) => out.push(next),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            },
            '(' | ')' | '{' | '}' | '+' | '?' | '|' => {
                out.push('\\');
                out.push(ch);
            }
            other => out.push(other),
        }
    }
    out
}

/// Resolve the effective dialect: an explicit CLI selector wins; otherwise
/// `grep.patternType`, then `grep.extendedRegexp`, then Git's default (basic).
pub fn resolve_pattern_type(
    cli: Option<GrepPatternType>,
    config_pattern_type: Option<&str>,
    config_extended_regexp: Option<bool>,
) -> GrepPatternType {
    if let Some(cli) = cli {
        return cli;
    }
    if let Some(raw) = config_pattern_type
        && let Some(parsed) = GrepPatternType::parse(raw)
    {
        return parsed;
    }
    if config_extended_regexp == Some(true) {
        GrepPatternType::Extended
    } else {
        GrepPatternType::Basic
    }
}

/// Check if a Perl regex contains constructs not supported by fancy-regex
/// (ADR-LG-09).
fn check_unsupported_pcre_syntax(pattern: &str) -> Option<&'static str> {
    // 1. Unescaped \K (reset match start / keepout).
    let mut in_escape = false;
    for ch in pattern.chars() {
        if in_escape {
            if ch == 'K' {
                return Some(r"\K");
            }
            in_escape = false;
        } else if ch == '\\' {
            in_escape = true;
        }
    }

    // 2. (*VERB) constructs, e.g. (*FAIL), (*ACCEPT), (*SKIP).
    if pattern.contains("(*") {
        return Some("(*VERB)");
    }

    // 3. PCRE recursion: (?R), (?0), (?1), (?P>name).
    if let Some(idx) = pattern.find("(?") {
        let rest = &pattern[idx + 2..];
        if rest.starts_with('R')
            || rest.starts_with('P')
            || rest.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            return Some("recursion");
        }
    }

    None
}

/// Compile pattern operands with shared dialect rules and word boundary option.
pub fn compile_grep_patterns_with_options(
    patterns: &[String],
    kind: GrepPatternType,
    ignore_case: bool,
    word_regexp: bool,
) -> CliResult<Vec<GrepMatcher>> {
    patterns
        .iter()
        .map(|pattern| {
            if pattern.len() > MAX_PATTERN_LEN {
                return Err(CliError::fatal(format!(
                    "invalid pattern '{pattern}': pattern exceeds maximum supported length ({MAX_PATTERN_LEN} bytes)"
                ))
                .with_exit_code(129)
                .with_stable_code(StableErrorCode::CliInvalidArguments));
            }

            match kind {
                GrepPatternType::Perl => {
                    if let Some(feature) = check_unsupported_pcre_syntax(pattern) {
                        return Err(CliError::fatal(format!(
                            "PCRE feature '{feature}' is not supported in this build: '{pattern}'"
                        ))
                        .with_exit_code(129)
                        .with_stable_code(StableErrorCode::CliInvalidArguments));
                    }

                    let pattern_str = if word_regexp {
                        format!(r"\b(?:{pattern})\b")
                    } else {
                        pattern.clone()
                    };

                    let mut builder = fancy_regex::RegexBuilder::new(&pattern_str);
                    builder
                        .case_insensitive(ignore_case)
                        .backtrack_limit(DEFAULT_BACKTRACK_LIMIT);
                    let re = builder.build().map_err(|error| {
                        CliError::fatal(format!("invalid --grep pattern '{pattern}': {error}"))
                            .with_exit_code(129)
                            .with_stable_code(StableErrorCode::CliInvalidArguments)
                    })?;
                    Ok(GrepMatcher {
                        pattern: pattern.clone(),
                        inner: MatcherInner::Perl(re),
                    })
                }
                _ => {
                    let mut translated = match kind {
                        GrepPatternType::Fixed => regex::escape(pattern),
                        GrepPatternType::Extended => pattern.clone(),
                        GrepPatternType::Basic => bre_to_ere(pattern),
                        GrepPatternType::Perl => unreachable!(),
                    };
                    if word_regexp {
                        translated = format!(r"\b(?:{translated})\b");
                    }
                    let re = regex::RegexBuilder::new(&translated)
                        .case_insensitive(ignore_case)
                        .build()
                        .map_err(|error| {
                            CliError::fatal(format!("invalid --grep pattern '{pattern}': {error}"))
                                .with_exit_code(129)
                                .with_stable_code(StableErrorCode::CliInvalidArguments)
                        })?;
                    Ok(GrepMatcher {
                        pattern: pattern.clone(),
                        inner: MatcherInner::Linear(re),
                    })
                }
            }
        })
        .collect()
}

/// Compile pattern operands with shared dialect rules.
pub fn compile_grep_patterns(
    patterns: &[String],
    kind: GrepPatternType,
    ignore_case: bool,
) -> CliResult<Vec<GrepMatcher>> {
    compile_grep_patterns_with_options(patterns, kind, ignore_case, false)
}

/// The commit-message slice `--grep` matches: the message body without an
/// embedded `gpgsig` signature block. This is what makes `--grep PGP` miss a
/// signed commit (ADR-LG-02 item 4).
pub fn commit_message_for_grep(message: &str) -> &str {
    if let Some(signature) = crate::common_utils::parse_commit_msg(message).1 {
        // INVARIANT: the shared parser returns a signature subslice of
        // `message`, ending at a UTF-8 boundary within that string.
        let signature_end =
            signature.as_ptr() as usize - message.as_ptr() as usize + signature.len();
        let after_signature = &message[signature_end..];
        after_signature
            .strip_prefix("\n\n")
            .unwrap_or(after_signature)
    } else {
        message.strip_prefix('\n').unwrap_or(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bre_translation_matches_git_examples() {
        assert_eq!(bre_to_ere("c++ (fix)"), "c\\+\\+ \\(fix\\)");
        assert_eq!(bre_to_ere("foo.bar"), "foo.bar");
        assert_eq!(bre_to_ere(r"a\+"), "a+");
        assert_eq!(bre_to_ere(r"a\{2,\}"), "a{2,}");
        assert_eq!(bre_to_ere(r"\(x\)"), "(x)");
        assert_eq!(bre_to_ere(r"a\|b"), "a|b");
        assert_eq!(bre_to_ere(r"\."), r"\.");
        assert_eq!(bre_to_ere(r"\<foo\>"), r"\<foo\>");
    }

    #[test]
    fn pattern_type_precedence() {
        assert_eq!(
            resolve_pattern_type(Some(GrepPatternType::Fixed), Some("extended"), Some(true)),
            GrepPatternType::Fixed
        );
        assert_eq!(
            resolve_pattern_type(None, Some("extended"), Some(false)),
            GrepPatternType::Extended
        );
        assert_eq!(
            resolve_pattern_type(None, None, Some(true)),
            GrepPatternType::Extended
        );
        assert_eq!(
            resolve_pattern_type(None, None, None),
            GrepPatternType::Basic
        );
    }

    #[test]
    fn basic_dialect_is_literal_for_ere_operators() {
        let re = compile_grep_patterns(&["c++ (fix)".into()], GrepPatternType::Basic, false)
            .expect("literal BRE");
        assert!(re[0].is_match("c++ (fix)").unwrap());
        assert!(!re[0].is_match("cc fix").unwrap());
    }

    #[test]
    fn perl_dialect_supports_lookaround_and_backreferences() {
        // Lookahead (?=...)
        let matchers = compile_grep_patterns(&[r"foo(?=bar)".into()], GrepPatternType::Perl, false)
            .expect("lookahead compiles");
        assert!(matchers[0].is_match("foobar").unwrap());
        assert!(!matchers[0].is_match("foobaz").unwrap());

        // Backreference \1
        let matchers = compile_grep_patterns(&[r"(a)\1".into()], GrepPatternType::Perl, false)
            .expect("backreference compiles");
        assert!(matchers[0].is_match("aa").unwrap());
        assert!(!matchers[0].is_match("ab").unwrap());
    }

    #[test]
    fn perl_dialect_rejects_unsupported_pcre_features() {
        let err =
            compile_grep_patterns(&[r"\Kfoo".into()], GrepPatternType::Perl, false).unwrap_err();
        assert!(err.to_string().contains(r"\K"));

        let err =
            compile_grep_patterns(&[r"(*FAIL)".into()], GrepPatternType::Perl, false).unwrap_err();
        assert!(err.to_string().contains("(*VERB)"));

        let err =
            compile_grep_patterns(&[r"(?R)".into()], GrepPatternType::Perl, false).unwrap_err();
        assert!(err.to_string().contains("recursion"));
    }

    #[test]
    fn pattern_length_limit_is_enforced() {
        let long_pattern = "a".repeat(MAX_PATTERN_LEN + 1);
        let err = compile_grep_patterns(&[long_pattern], GrepPatternType::Perl, false).unwrap_err();
        assert!(err.to_string().contains("exceeds maximum supported length"));
    }

    #[test]
    fn perl_catastrophic_backtracking_hits_limit() {
        let mut builder = fancy_regex::RegexBuilder::new(r"(?i)(a|b|ab)*(?>c)");
        builder.backtrack_limit(10_000);
        let re = builder.build().unwrap();
        let matcher = GrepMatcher {
            pattern: "(?i)(a|b|ab)*(?>c)".to_string(),
            inner: MatcherInner::Perl(re),
        };
        let input = "abababababababababababababababababababababababababababab";
        let err = matcher.is_match(input).unwrap_err();
        assert!(err.to_string().contains("backtracking"));
    }
}
