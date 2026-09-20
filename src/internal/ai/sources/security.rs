//! W4-11: shared RequestScope + resolver helpers for security-sensitive loaders.
//!
//! Domain merge (tighten-only sandbox/hooks/approval, rules/contexts
//! overlay-add) stays in each loader. This module only pins scope, calls the
//! W4-06 resolver, and formats fail-closed diagnostics that name the source
//! layer without echoing file contents.

use std::path::{Path, PathBuf};

use super::resolver::{
    ConfigResolveError, ResolvedConfig, ResolvedConfigDir, resolve_config_dir, resolve_config_file,
};
use crate::internal::worktree_scope::RequestScope;

/// Operator diagnostic for a parse failure: kind + layer + path + optional
/// line/column. Never includes file contents or parser snippets (those can
/// leak tokens from malformed security config).
pub fn format_security_parse_error(
    kind: &str,
    layer: &str,
    path: &Path,
    location: Option<(usize, usize)>,
) -> String {
    match location {
        Some((line, column)) => format!(
            "failed to parse {kind} ({layer} at `{}`: line {line} column {column})",
            path.display()
        ),
        None => format!("failed to parse {kind} ({layer} at `{}`)", path.display()),
    }
}

/// Line/column from a TOML byte span without echoing the source line.
pub fn toml_error_location(source: &str, error: &toml::de::Error) -> Option<(usize, usize)> {
    let span = error.span()?;
    Some(byte_offset_line_column(source, span.start))
}

/// Line/column from `serde_json` without echoing the source line.
pub fn json_error_location(error: &serde_json::Error) -> Option<(usize, usize)> {
    Some((error.line(), error.column()))
}

fn byte_offset_line_column(source: &str, byte_offset: usize) -> (usize, usize) {
    let prefix = source.get(..byte_offset.min(source.len())).unwrap_or("");
    let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
    let column = prefix
        .rsplit('\n')
        .next()
        .map(|row| row.chars().count() + 1)
        .unwrap_or(1);
    (line, column)
}

/// Format a resolver failure for operators: layer + location + path, never
/// the unreadable file's contents.
pub fn format_resolve_error(err: &ConfigResolveError) -> String {
    match err {
        ConfigResolveError::SecurityRepositoryUnreadable {
            location,
            repository_path,
            message,
        } => format!(
            "config '{location}' repository layer is unreadable at '{}': {message}",
            repository_path.display()
        ),
        ConfigResolveError::Paths { workdir, message } => format!(
            "cannot resolve config paths for workdir '{}': {message}",
            workdir.display()
        ),
        other => other.to_string(),
    }
}

/// Resolve a registered security file through the W4-06 resolver.
pub fn resolve_security_file(
    request: &RequestScope,
    location: &'static str,
) -> Result<ResolvedConfig, String> {
    resolve_config_file(request, location).map_err(|err| format_resolve_error(&err))
}

/// Resolve a registered security directory through the W4-06 resolver.
pub fn resolve_security_dir(
    request: &RequestScope,
    location: &'static str,
) -> Result<ResolvedConfigDir, String> {
    resolve_config_dir(request, location).map_err(|err| format_resolve_error(&err))
}

/// Pin RequestScope for `workdir` when it sits inside a Libra repository.
///
/// `Ok(None)` means the path is outside any repo (unit tests, non-repo
/// sandbox use). Callers fall back to the conventional
/// `<workdir>/.libra/<file>` path rather than minting a phantom store.
/// A damaged worktree (unreadable `commondir`, dangling gitdir) is `Err`
/// so security loaders fail closed instead of reading a local overlay.
pub fn request_scope_for_workdir(workdir: &Path) -> Result<Option<RequestScope>, String> {
    RequestScope::try_resolve(workdir.to_path_buf()).map_err(|error| {
        format!(
            "cannot resolve config paths for workdir '{}': {error}",
            workdir.display()
        )
    })
}

/// Resolve a registered directory via RequestScope, or fall back to
/// `<workdir>/.libra/<location>` outside a repository.
pub fn resolved_dir_paths(
    working_dir: &Path,
    location: &'static str,
) -> Result<(PathBuf, Option<PathBuf>), String> {
    if let Some(request) = request_scope_for_workdir(working_dir)? {
        let resolved = resolve_security_dir(&request, location)?;
        return Ok((resolved.repository_path, resolved.overlay_path));
    }
    Ok((working_dir.join(".libra").join(location), None))
}
