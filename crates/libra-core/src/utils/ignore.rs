//! Ignore handling utilities defining policies for .libraignore, index-aware filtering, and helpers to test whether paths are ignored.

use std::{
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
};

use git_internal::internal::index::Index;
use walkdir::WalkDir;

use super::util;

mod bounded;
pub use bounded::BoundedIgnoreWalk;

const LIBRAIGNORE_FILE: &str = ".libraignore";
const GITIGNORE_FILE: &str = ".gitignore";
const DEFAULT_LIBRAIGNORE_CONTENT: &[u8] = b"# Libra ignore file
# Uses gitignore-compatible patterns.
# Add generated files and local-only paths below.
";

/// File-system errors raised while creating or converting ignore files.
#[derive(thiserror::Error, Debug)]
pub enum IgnoreFileError {
    #[error("failed to read ignore file '{path}': {source}")]
    Read { path: PathBuf, source: io::Error },

    #[error("failed to create parent directory '{path}' for ignore file '{target}': {source}")]
    CreateDirectory {
        path: PathBuf,
        target: PathBuf,
        source: io::Error,
    },

    #[error("failed to write ignore file '{path}': {source}")]
    Write { path: PathBuf, source: io::Error },

    #[error("failed to scan ignore files under '{path}': {source}")]
    Walk { path: PathBuf, source: io::Error },

    #[error("failed to resolve ignore file path '{path}' relative to '{root}': {message}")]
    RelativePath {
        root: PathBuf,
        path: PathBuf,
        message: String,
    },
}

impl IgnoreFileError {
    pub fn is_write(&self) -> bool {
        matches!(self, Self::CreateDirectory { .. } | Self::Write { .. })
    }

    pub fn recovery_hint(&self) -> &'static str {
        match self {
            Self::Read { .. } | Self::Write { .. } => {
                "check .gitignore/.libraignore permissions and retry."
            }
            Self::CreateDirectory { .. } => {
                "check parent directory permissions for .libraignore and retry."
            }
            Self::Walk { .. } => "check source repository permissions and retry.",
            Self::RelativePath { .. } => {
                "ensure the source ignore file is inside the repository being converted."
            }
        }
    }
}

/// Describes how commands should treat entries matched by `.libraignore`.
/// - `Respect`: honor ignore rules for untracked files but always keep tracked ones.
/// - `IncludeIgnored`: disable ignore filtering entirely, used by `add --force` and similar flows.
/// - `OnlyIgnored`: surface only the ignored set, used by `status --ignored` flows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IgnorePolicy {
    Respect,
    IncludeIgnored,
    OnlyIgnored,
}

/// Summary returned by [`convert_gitignore_files_to_libraignore`].
pub struct IgnoreConversionSummary {
    /// Worktree-relative paths of `.libraignore` files that were newly written
    /// (either created from scratch or updated from the default stub).
    pub converted: Vec<PathBuf>,
    /// Non-fatal messages for locations where a user-owned `.libraignore` was
    /// already present and was left unchanged.
    pub warnings: Vec<String>,
}

/// Copies every `.gitignore` under `source_root` to a sibling `.libraignore` under `target_root`.
///
/// Existing generated default `.libraignore` files are replaced; existing user-owned
/// `.libraignore` files are preserved and reported as non-fatal warnings.
/// Returns an [`IgnoreConversionSummary`] with the paths of newly written files and any warnings.
pub fn convert_gitignore_files_to_libraignore(
    source_root: &Path,
    target_root: &Path,
) -> Result<IgnoreConversionSummary, IgnoreFileError> {
    let mut converted = Vec::new();
    let mut warnings = Vec::new();
    for entry in WalkDir::new(source_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| should_visit_ignore_entry(entry.path(), source_root))
    {
        let entry = entry.map_err(|error| walkdir_error(source_root, error))?;
        if !entry.file_type().is_file() || entry.file_name() != OsStr::new(GITIGNORE_FILE) {
            continue;
        }

        let relative = entry.path().strip_prefix(source_root).map_err(|error| {
            IgnoreFileError::RelativePath {
                root: source_root.to_path_buf(),
                path: entry.path().to_path_buf(),
                message: error.to_string(),
            }
        })?;
        let target = target_root.join(relative).with_file_name(LIBRAIGNORE_FILE);
        if copy_gitignore_to_libraignore(entry.path(), &target, &mut warnings)? {
            // Record the path relative to the target root for display purposes.
            let display_path = target
                .strip_prefix(target_root)
                .unwrap_or(&target)
                .to_path_buf();
            converted.push(display_path);
        }
    }
    Ok(IgnoreConversionSummary {
        converted,
        warnings,
    })
}

/// Copy one `.gitignore` to a sibling `.libraignore`.
///
/// Returns `true` when the `.libraignore` was written (created or updated from the
/// default stub) and `false` when an existing user-owned file was preserved.
fn copy_gitignore_to_libraignore(
    source: &Path,
    target: &Path,
    warnings: &mut Vec<String>,
) -> Result<bool, IgnoreFileError> {
    let source_content = fs::read(source).map_err(|read_error| IgnoreFileError::Read {
        path: source.to_path_buf(),
        source: read_error,
    })?;

    if !target.exists() {
        write_libraignore(target, &source_content)?;
        return Ok(true);
    }

    let existing = fs::read(target).map_err(|read_error| IgnoreFileError::Read {
        path: target.to_path_buf(),
        source: read_error,
    })?;
    if existing == DEFAULT_LIBRAIGNORE_CONTENT || existing == source_content {
        if existing != source_content {
            write_libraignore(target, &source_content)?;
            return Ok(true);
        }
        return Ok(false);
    }

    warnings.push(format!(
        "kept existing .libraignore at '{}'; skipped converting '{}'",
        target.display(),
        source.display()
    ));
    Ok(false)
}

fn write_libraignore(target: &Path, content: &[u8]) -> Result<(), IgnoreFileError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|source| IgnoreFileError::CreateDirectory {
            path: parent.to_path_buf(),
            target: target.to_path_buf(),
            source,
        })?;
    }
    fs::write(target, content).map_err(|source| IgnoreFileError::Write {
        path: target.to_path_buf(),
        source,
    })
}

fn should_visit_ignore_entry(path: &Path, root: &Path) -> bool {
    if path == root {
        return true;
    }
    let Some(name) = path.file_name() else {
        return true;
    };
    name != OsStr::new(".git") && name != OsStr::new(util::ROOT_DIR)
}

fn walkdir_error(root: &Path, error: walkdir::Error) -> IgnoreFileError {
    let message = error.to_string();
    let source = error
        .into_io_error()
        .unwrap_or_else(|| io::Error::other(message));
    IgnoreFileError::Walk {
        path: root.to_path_buf(),
        source,
    }
}

/// Returns `true` if the given workdir-relative `path` should be filtered out under `policy`.
/// The check is index-aware; tracked entries remain visible for `Respect`, are always included for
/// `IncludeIgnored`, and get filtered when `OnlyIgnored` is requested.
pub fn should_ignore(path: &Path, policy: IgnorePolicy, index: &Index) -> bool {
    let workdir = util::working_dir();
    let layers = crate::internal::layer::ExclusionSnapshot::for_request();
    should_ignore_with_workdir(path, policy, index, &workdir, &layers)
}

/// Applies [`should_ignore`] over an iterator of workdir paths and returns the retained list.
pub fn filter_workdir_paths<I>(paths: I, policy: IgnorePolicy, index: &Index) -> Vec<PathBuf>
where
    I: IntoIterator<Item = PathBuf>,
{
    let workdir = util::working_dir();
    // ONE snapshot for the whole walk (§C.4.1.1): captured here, immutable for
    // the duration, so a concurrent task re-pinning the request scope cannot
    // switch which worktree's exclusions this walk is applying halfway
    // through — and the per-path consult below costs no lock and no allocation.
    let layers = crate::internal::layer::ExclusionSnapshot::for_request();
    paths
        .into_iter()
        .filter(|path| !should_ignore_with_workdir(path, policy, index, &workdir, &layers))
        .collect()
}

/// Worker that shares the ignore logic between direct calls and batched iterators.
fn should_ignore_with_workdir(
    path: &Path,
    policy: IgnorePolicy,
    index: &Index,
    workdir: &Path,
    layers: &crate::internal::layer::ExclusionSnapshot,
) -> bool {
    match should_ignore_with_matcher(path, policy, index, layers, || {
        Ok::<_, std::convert::Infallible>(is_path_ignored_with_layers(path, workdir, layers))
    }) {
        Ok(ignored) => ignored,
        Err(infallible) => match infallible {},
    }
}

/// Apply index/layer policy once; callers choose the raw lookup's I/O boundary.
fn should_ignore_with_matcher<E>(
    path: &Path,
    policy: IgnorePolicy,
    index: &Index,
    layers: &crate::internal::layer::ExclusionSnapshot,
    matcher: impl FnOnce() -> Result<bool, E>,
) -> Result<bool, E> {
    let is_tracked = path_is_tracked_or_unknown_encoding(path, index);

    // lore.md 2.4: a materialized layer-overlay path is UN-NEGATABLY excluded
    // (highest precedence, above tracked/.libraignore/`!` negations) so a
    // purely-local overlay can never be swept into `status`/`add .`. This is
    // consulted for EVERY policy except IncludeIgnored (force-add), where the
    // `add` staging guard is the airtight backstop instead. Empty snapshot
    // (no layers) → no-op.
    if !matches!(policy, IgnorePolicy::IncludeIgnored)
        && !layers.is_empty()
        && let Some(key) = crate::internal::layer::normalize_key(path)
        && layers.is_owned(&key)
    {
        // Respect: the layer path is excluded from `status`/`add .` (un-negatable).
        // OnlyIgnored (the `clean -x` candidate scan): NOT a candidate — protect
        // the active local overlay from being deleted by `clean -x` (only a
        // re-apply could restore it).
        return Ok(matches!(policy, IgnorePolicy::Respect));
    }

    match policy {
        IgnorePolicy::Respect => {
            if is_tracked {
                return Ok(false);
            }
            matcher()
        }
        IgnorePolicy::IncludeIgnored => Ok(false),
        IgnorePolicy::OnlyIgnored => {
            if is_tracked {
                return Ok(true);
            }
            matcher().map(|ignored| !ignored)
        }
    }
}

fn path_is_tracked_or_unknown_encoding(path: &Path, index: &Index) -> bool {
    match path.to_str() {
        Some(path_str) => index.tracked(path_str, 0),
        // The current index API is UTF-8 keyed. Preserve visibility for paths we cannot
        // look up instead of silently treating a possibly tracked path as untracked.
        None => true,
    }
}

/// Whether `path` matches an active ignore pattern, regardless of whether it is
/// tracked. Unlike `IgnorePolicy::OnlyIgnored` (which treats every tracked entry as
/// "ignored"), this reports the raw pattern match — used by `ls-files -i` so a
/// tracked file that matches an exclude pattern (`-i -c`) is surfaced correctly.
pub fn path_matches_ignore_pattern(path: &Path, workdir: &Path) -> bool {
    is_path_ignored(path, workdir)
}

fn is_path_ignored(path: &Path, workdir: &Path) -> bool {
    is_path_ignored_with_layers(
        path,
        workdir,
        &crate::internal::layer::ExclusionSnapshot::for_request(),
    )
}

/// [`is_path_ignored`] against an ALREADY-CAPTURED layer snapshot.
///
/// The walk's snapshot has to reach the bottom of the call chain: recapturing
/// here would re-read process-global state per path, which is exactly the
/// re-pin hazard capturing once was meant to close.
fn is_path_ignored_with_layers(
    path: &Path,
    workdir: &Path,
    layers: &crate::internal::layer::ExclusionSnapshot,
) -> bool {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workdir.join(path)
    };
    util::check_gitignore_with_layers(workdir, &absolute, layers)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use git_internal::internal::index::Index;
    use tempfile::tempdir;

    use super::*;

    /// Scenario: a fresh repo with two `.gitignore` files (root + subdirectory).
    /// `convert_gitignore_files_to_libraignore` must create both `.libraignore`
    /// siblings and report them in `IgnoreConversionSummary::converted`.
    #[test]
    fn conversion_reports_newly_created_libraignore_files() {
        let repo = tempdir().unwrap();
        let root = repo.path();
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(root.join(".gitignore"), "*.tmp\n").unwrap();
        fs::write(sub.join(".gitignore"), "build/\n").unwrap();

        let summary = convert_gitignore_files_to_libraignore(root, root).unwrap();

        // Both files should be reported as converted (no pre-existing .libraignore).
        assert_eq!(summary.converted.len(), 2, "expected 2 converted paths");
        assert!(summary.warnings.is_empty(), "expected no warnings");

        // The created files should contain the source content.
        assert_eq!(fs::read(root.join(".libraignore")).unwrap(), b"*.tmp\n");
        assert_eq!(fs::read(sub.join(".libraignore")).unwrap(), b"build/\n");
    }

    /// Scenario: when a user-owned `.libraignore` already exists (content differs from
    /// the default stub AND from `.gitignore`), the conversion must skip it, emit a
    /// warning, and NOT include it in `converted`.
    #[test]
    fn conversion_preserves_user_owned_libraignore_and_warns() {
        let repo = tempdir().unwrap();
        let root = repo.path();
        fs::write(root.join(".gitignore"), "*.tmp\n").unwrap();
        // Write a user-owned .libraignore with different content.
        fs::write(root.join(".libraignore"), "custom_rule/\n").unwrap();

        let summary = convert_gitignore_files_to_libraignore(root, root).unwrap();

        assert!(
            summary.converted.is_empty(),
            "user-owned .libraignore must not be reported as converted"
        );
        assert_eq!(
            summary.warnings.len(),
            1,
            "expected one preservation warning"
        );
        // The original user content must be intact.
        assert_eq!(
            fs::read(root.join(".libraignore")).unwrap(),
            b"custom_rule/\n"
        );
    }

    #[test]
    fn write_libraignore_reports_parent_directory_creation_errors() {
        let repo = tempdir().unwrap();
        let parent = repo.path().join("not-a-directory");
        fs::write(&parent, "file").unwrap();
        let target = parent.join(".libraignore");

        let error = write_libraignore(&target, b"ignored\n").unwrap_err();

        match error {
            IgnoreFileError::CreateDirectory {
                path,
                target: error_target,
                ..
            } => {
                assert_eq!(path, parent);
                assert_eq!(error_target, target);
            }
            other => panic!("expected CreateDirectory error, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial(cwd, env)]
    fn non_utf8_paths_use_conservative_tracked_fallback() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let repo = tempdir().unwrap();
        let workdir = repo.path().to_path_buf();
        fs::write(workdir.join(".libraignore"), "*\n").unwrap();
        let non_utf8_path = PathBuf::from(OsString::from_vec(vec![b'i', 0xff, b'n']));
        let index = Index::new();

        assert!(
            !should_ignore_with_workdir(
                &non_utf8_path,
                IgnorePolicy::Respect,
                &index,
                &workdir,
                &Default::default(),
            ),
            "unknown-encoding paths should stay visible under Respect"
        );
        assert!(
            should_ignore_with_workdir(
                &non_utf8_path,
                IgnorePolicy::OnlyIgnored,
                &index,
                &workdir,
                &Default::default(),
            ),
            "unknown-encoding paths should be excluded from OnlyIgnored like tracked entries"
        );
    }
}
