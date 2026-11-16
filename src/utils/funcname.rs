//! Shared grep / unified-diff function-header rules (ADR-LG-08 item 2).
//!
//! Git derives the `@@ … @@ <func>` suffix and `grep -p`/`-W` headers from the
//! same rules: a `.gitattributes` `diff=<driver>` selects
//! `diff.<driver>.xfuncname`/`funcname`; otherwise a line starting with a
//! letter, `_`, or `$` is a function header.

use regex::Regex;

/// Whether `line` is a default function header (Git's fallback rule).
pub fn is_default_function_header(line: &str) -> bool {
    line.chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_' || ch == '$')
}

/// Index of the nearest default function header at or before `line_index`
/// (inclusive). Returns `None` when there is none.
pub fn default_header_before(lines: &[&str], line_index: usize) -> Option<usize> {
    (0..=line_index.min(lines.len().saturating_sub(1)))
        .rev()
        .find(|&idx| is_default_function_header(lines[idx]))
}

/// Build the configured `xfuncname`/`funcname` matcher for `path`, when a
/// `.gitattributes` `diff=<driver>` and matching config value exist.
pub async fn configured_funcname_regex(path: &str) -> Option<Regex> {
    let driver = crate::utils::attributes::diff_driver_for_path(std::path::Path::new(path))?;
    let key = format!("diff.{driver}.xfuncname");
    let value = match crate::internal::config::ConfigKv::get(&key).await {
        Ok(Some(entry)) => Some(entry.value),
        _ => match crate::internal::config::ConfigKv::get(&format!("diff.{driver}.funcname")).await
        {
            Ok(Some(entry)) => Some(entry.value),
            _ => None,
        },
    }?;
    Regex::new(&value).ok()
}

/// The function name for the hunk/line at `line_index`: the last configured
/// regex match at or before it, else the default header text.
pub fn function_name_with(
    regex: Option<&Regex>,
    lines: &[&str],
    line_index: usize,
) -> Option<String> {
    if lines.is_empty() {
        return None;
    }
    let upto = line_index.min(lines.len() - 1);
    if let Some(regex) = regex {
        for idx in (0..=upto).rev() {
            if let Some(captures) = regex.captures(lines[idx])
                && let Some(name) = captures.get(0)
            {
                return Some(name.as_str().to_string());
            }
        }
        return None;
    }
    default_header_before(lines, upto).map(|idx| lines[idx].to_string())
}

/// Index of the function header governing `line_index`, using the configured
/// regex when present and the default rule otherwise.
pub fn header_index_with(
    regex: Option<&Regex>,
    lines: &[&str],
    line_index: usize,
) -> Option<usize> {
    if lines.is_empty() {
        return None;
    }
    let upto = line_index.min(lines.len() - 1);
    if let Some(regex) = regex {
        return (0..=upto).rev().find(|&idx| regex.is_match(lines[idx]));
    }
    default_header_before(lines, upto)
}

/// Default-rule convenience wrapper.
pub fn function_name_for_path(_path: &str, lines: &[&str], line_index: usize) -> Option<String> {
    function_name_with(None, lines, line_index)
}

/// `(header_index, end_index)` of the function containing `line_index`.
/// `end_index` is exclusive. The whole file is the fallback range when no
/// header exists.
pub fn function_range(lines: &[&str], line_index: usize) -> (usize, usize) {
    if lines.is_empty() {
        return (0, 0);
    }
    let start = default_header_before(lines, line_index).unwrap_or(0);
    let end = ((start + 1)..lines.len())
        .find(|&idx| is_default_function_header(lines[idx]))
        .unwrap_or(lines.len());
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rule_matches_git() {
        assert!(is_default_function_header("int main(void)"));
        assert!(is_default_function_header("_private()"));
        assert!(is_default_function_header("$$helper"));
        assert!(!is_default_function_header("  return foo;"));
        assert!(!is_default_function_header("{"));
        assert!(!is_default_function_header(""));
    }

    #[test]
    fn header_and_range_lookup() {
        let lines = ["int main(void)", "{", "  return foo;", "}", "word foo"];
        assert_eq!(default_header_before(&lines, 2), Some(0));
        assert_eq!(
            function_name_for_path("code.c", &lines, 2),
            Some("int main(void)".into())
        );
        assert_eq!(function_range(&lines, 2), (0, 4));
        assert_eq!(
            function_name_for_path("code.c", &lines, 4),
            Some("word foo".into())
        );
    }
}
