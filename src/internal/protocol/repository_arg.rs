//! Shared repository-argument parsing for `fetch`/`pull`/`push`/`ls-remote`.
//!
//! A command's repository argument may be either a configured remote name or an
//! anonymous local repository spec: a `file://` URL, an absolute path, or a
//! relative path (including `.` and `..`). Git treats the latter as an unnamed
//! remote that negotiates directly against the target; Libra writes only
//! `FETCH_HEAD` for an anonymous fetch (no tracking ref, no `remote.*` config).
//! The configured-name lookups are intentionally left to the caller so the
//! precedence (configured name wins over a same-named directory) stays correct.

use std::path::Path;

/// Whether `spec` is a direct/remote spec that should not be resolved through
/// the configured-remote table: SSH specs, any parseable URL, and absolute or
/// relative paths. Mirrors the `ls-remote` heuristic and Git's local-path
/// transport (see `ADR-HP-05`).
pub fn is_anonymous_repository_spec(spec: &str) -> bool {
    if super::ssh_client::is_ssh_spec(spec) || url::Url::parse(spec).is_ok() {
        return true;
    }
    // `.` and `..` name the current / parent repository directory directly.
    if matches!(spec, "." | "..") {
        return true;
    }
    let path = Path::new(spec);
    path.is_absolute()
        || spec.starts_with("./")
        || spec.starts_with("../")
        || spec.starts_with(".\\")
        || spec.starts_with("..\\")
}

/// Normalize an anonymous repository spec into the canonical URL/transport form
/// used by the client (relative paths stay relative for Git, which is able to
/// resolve them in-process). For an HTTP(S)/SSH URL this is the URL itself; for
/// a local path it is the path string, matching `ls-remote`.
pub fn canonical_repository_url(spec: &str) -> String {
    spec.to_string()
}

/// Derive a single-component, refname-safe name for an anonymous repository
/// spec (the last path component). Anonymous fetch uses this only to build a
/// valid default tracking destination; because `--refmap=` suppresses tracking,
/// it is not persisted anywhere. Falls back to the whole spec when there is no
/// path separator, and to the spec itself when the last component is empty.
pub fn anonymous_remote_name(spec: &str) -> String {
    let trimmed = spec.trim_end_matches(['/', '\\']);
    // Prefer the last path separator; for SSH `host:path` specs, fall back to ':'.
    let sep_index = trimmed.rfind(['/', '\\']).or_else(|| trimmed.rfind(':'));
    let candidate = match sep_index {
        Some(i) => trimmed[i + 1..].to_string(),
        None => trimmed.to_string(),
    };
    // `.`/`..` and an empty trailing component are not valid refname components,
    // so use a refname-safe placeholder (tracking is suppressed for anonymous
    // fetches, so the exact name is incidental).
    match candidate.as_str() {
        "." => "local".to_string(),
        ".." => "upstream".to_string(),
        _ if candidate.is_empty() => trimmed.to_string(),
        _ => candidate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anonymous_spec_recognition() {
        assert!(is_anonymous_repository_spec("/abs/path"));
        assert!(is_anonymous_repository_spec("file:///tmp/repo.git"));
        assert!(is_anonymous_repository_spec("https://example.com/repo.git"));
        assert!(is_anonymous_repository_spec("git@github.com:org/repo.git"));
        assert!(is_anonymous_repository_spec("./relative"));
        assert!(is_anonymous_repository_spec("../up"));
        assert!(!is_anonymous_repository_spec("origin"));
        assert!(!is_anonymous_repository_spec("origin/main"));
    }

    #[test]
    fn canonical_url_passthrough() {
        assert_eq!(canonical_repository_url("/tmp/repo"), "/tmp/repo");
        assert_eq!(
            canonical_repository_url("https://example.com/r.git"),
            "https://example.com/r.git"
        );
    }

    #[test]
    fn anonymous_name_selection() {
        assert_eq!(anonymous_remote_name("/tmp/x/remote.git"), "remote.git");
        assert_eq!(anonymous_remote_name("./src"), "src");
        assert_eq!(anonymous_remote_name("../up"), "up");
        assert_eq!(anonymous_remote_name("file:///tmp/y/repo"), "repo");
        assert_eq!(anonymous_remote_name("git@host:path"), "path");
        assert_eq!(anonymous_remote_name("origin"), "origin");
        assert_eq!(anonymous_remote_name("."), "local");
        assert_eq!(anonymous_remote_name(".."), "upstream");
    }
}
