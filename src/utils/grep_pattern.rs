//! Shared `--grep` pattern handling for `log` and `rev-list` (ADR-LG-02).
//!
//! Git's default commit-message grep is POSIX basic regex, not a plain
//! substring and not Rust regex syntax. This module owns the dialect
//! translation, the CLI/config precedence, and the "only the message body"
//! scope so both commands share one implementation.

use crate::utils::error::{CliError, CliResult, StableErrorCode};

/// The `--grep` pattern dialect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrepPatternType {
    /// POSIX basic regex (Git's default; GNU `\+`, `\?`, `\|` extensions).
    Basic,
    /// POSIX extended regex.
    Extended,
    /// Literal byte string.
    Fixed,
    /// Perl-compatible regex (engine lands with LG-14).
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

/// Compile every `--grep` operand with the shared dialect rules.
pub fn compile_grep_patterns(
    patterns: &[String],
    kind: GrepPatternType,
    ignore_case: bool,
) -> CliResult<Vec<regex::Regex>> {
    if kind == GrepPatternType::Perl {
        return Err(CliError::command_usage(
            "--perl-regexp is not enabled in this build (planned for a later release)",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }

    patterns
        .iter()
        .map(|pattern| {
            let translated = match kind {
                GrepPatternType::Fixed => regex::escape(pattern),
                GrepPatternType::Extended => pattern.clone(),
                GrepPatternType::Basic => bre_to_ere(pattern),
                GrepPatternType::Perl => unreachable!("handled above"),
            };
            regex::RegexBuilder::new(&translated)
                .case_insensitive(ignore_case)
                .build()
                .map_err(|error| {
                    CliError::fatal(format!("invalid --grep pattern '{pattern}': {error}"))
                        .with_exit_code(129)
                        .with_stable_code(StableErrorCode::CliInvalidArguments)
                })
        })
        .collect()
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
        // Unescaped ERE operators are literals in BRE.
        assert_eq!(bre_to_ere("c++ (fix)"), "c\\+\\+ \\(fix\\)");
        assert_eq!(bre_to_ere("foo.bar"), "foo.bar");
        // Escaped BRE operators become ERE operators.
        assert_eq!(bre_to_ere(r"a\+"), "a+");
        assert_eq!(bre_to_ere(r"a\{2,\}"), "a{2,}");
        assert_eq!(bre_to_ere(r"\(x\)"), "(x)");
        assert_eq!(bre_to_ere(r"a\|b"), "a|b");
        // A literal backslash and word boundaries survive.
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
    fn compile_rejects_perl_and_invalid_basic() {
        let err = compile_grep_patterns(&["x".into()], GrepPatternType::Perl, false).unwrap_err();
        assert!(err.to_string().contains("--perl-regexp"));
        let err =
            compile_grep_patterns(&[r"a\(".into()], GrepPatternType::Basic, false).unwrap_err();
        assert!(err.to_string().contains("invalid --grep pattern"));
    }

    #[test]
    fn basic_dialect_is_literal_for_ere_operators() {
        let re = compile_grep_patterns(&["c++ (fix)".into()], GrepPatternType::Basic, false)
            .expect("literal BRE");
        assert!(re[0].is_match("c++ (fix)"));
        assert!(!re[0].is_match("cc fix"));
    }
}
