//! Canonical unified-diff hunk header formatting (ADR-LG-04).
//!
//! Git prints a hunk range as `start` when the length is 1, `start,0` for an
//! empty side, and `start,length` otherwise. Before this module the patch
//! writers hard-coded `start,length`, so single-line hunks and empty sides
//! differed from Git (and `format-patch` output could not be applied by Git
//! byte-for-byte). Every patch output surface must call this one helper.

/// Format one side of a hunk header (`start` is the 1-based first line, or the
/// anchor line for an empty side; `count` is the number of lines).
pub fn format_hunk_span(start: usize, count: usize) -> String {
    match count {
        1 => start.to_string(),
        0 => format!("{start},0"),
        _ => format!("{start},{count}"),
    }
}

/// Format a complete `@@ -old +new @@` header (without a section heading).
pub fn format_hunk_header(
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
) -> String {
    format!(
        "@@ -{} +{} @@",
        format_hunk_span(old_start, old_count),
        format_hunk_span(new_start, new_count)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_spans_match_git() {
        // New one-line file: `@@ -0,0 +1 @@`.
        assert_eq!(format_hunk_header(0, 0, 1, 1), "@@ -0,0 +1 @@");
        // New three-line file: `@@ -0,0 +1,3 @@`.
        assert_eq!(format_hunk_header(0, 0, 1, 3), "@@ -0,0 +1,3 @@");
        // Single-line modification: `@@ -1 +1 @@`.
        assert_eq!(format_hunk_header(1, 1, 1, 1), "@@ -1 +1 @@");
        // Two lines to one: `@@ -1,2 +1 @@`.
        assert_eq!(format_hunk_header(1, 2, 1, 1), "@@ -1,2 +1 @@");
        // Pure insertion after old line 5: `@@ -5,0 +6 @@`.
        assert_eq!(format_hunk_header(5, 0, 6, 1), "@@ -5,0 +6 @@");
        // Emptied file: `@@ -1,5 +0,0 @@`.
        assert_eq!(format_hunk_header(1, 5, 0, 0), "@@ -1,5 +0,0 @@");
    }
}
