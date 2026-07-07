//! Case-fold path utilities (lore.md §1.14) — the substrate for file
//! case-change handling on case-insensitive filesystems.
//!
//! FOLD SEMANTICS: `fold_path_key` lowercases per `char::to_lowercase` — a
//! documented APPROXIMATION of the real NTFS `$UpCase` / APFS
//! `caseFolding.txt` tables (e.g. final sigma `ς`≡`σ` folds on APFS but not
//! here; one-to-many mappings differ). Misses fail OPEN (a real collision
//! can slip past the `error` guard on exotic paths) — never CLOSED (two
//! genuinely distinct paths are never conflated beyond real lowercase
//! equality). Unicode normalization (NFC/NFD — APFS and HFS+ are both
//! normalization-insensitive) is out of scope for v1, documented.
//!
//! POLICY: `core.casehandling` = `error` (default, conservative per the lore
//! row) | `warn` | `allow`; an unrecognized value is a HARD error (a typo
//! must not silently weaken the default). Consulted only when the effective
//! filesystem view is case-insensitive.
//!
//! DETECTION: explicit `core.ignorecase` (git-bool, invalid = hard error) >
//! runtime probe > false. The probe stats a case-swapped spelling of the
//! repo's `.libra` entry and confirms identity via device+inode on Unix
//! (canonicalize-equality is NOT trustworthy for this on macOS: it returns
//! the queried casing, not the on-disk one).

use std::{collections::HashMap, path::Path};

use anyhow::{Result, anyhow};

use crate::{
    internal::{config::ConfigKv, db::establish_connection},
    utils::util,
};

/// The case-handling policy (lore.md 1.14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaseHandling {
    /// Refuse implicit case events with a path-level error (conservative
    /// default per the lore row).
    #[default]
    Error,
    /// Proceed with Git-parity behavior plus a per-path warning.
    Warn,
    /// Proceed silently.
    Allow,
}

impl CaseHandling {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "error" => Ok(CaseHandling::Error),
            "warn" => Ok(CaseHandling::Warn),
            "allow" => Ok(CaseHandling::Allow),
            other => Err(anyhow!(
                "unsupported core.casehandling '{other}' (expected 'error', 'warn', or 'allow')"
            )),
        }
    }
}

/// Resolve `core.casehandling` (default [`CaseHandling::Error`]; unknown
/// value = hard error).
pub async fn case_handling_from_config() -> Result<CaseHandling> {
    let entry = ConfigKv::get_var_case_insensitive("core.", "casehandling")
        .await
        .map_err(|error| anyhow!("failed to read core.casehandling: {error}"))?;
    match entry {
        None => Ok(CaseHandling::Error),
        Some(entry) if entry.value.trim().is_empty() => Ok(CaseHandling::Error),
        Some(entry) => CaseHandling::parse(&entry.value),
    }
}

/// Locale-independent fold key for a repo-relative path (see module docs for
/// the approximation caveats).
pub fn fold_path_key(path: &str) -> String {
    path.chars().flat_map(char::to_lowercase).collect()
}

/// Component-aware case-folded prefix check for paths.
///
/// This intentionally compares path components instead of folded path strings,
/// so `FooBar` is not treated as a descendant of `foo`.
pub fn path_starts_with_casefold(path: &Path, parent: &Path) -> bool {
    let mut path_components = path.components();
    for parent_component in parent.components() {
        let Some(path_component) = path_components.next() else {
            return false;
        };
        let path_key = fold_path_key(path_component.as_os_str().to_string_lossy().as_ref());
        let parent_key = fold_path_key(parent_component.as_os_str().to_string_lossy().as_ref());
        if path_key != parent_key {
            return false;
        }
    }
    true
}

/// Whether two paths differ ONLY by case (fold-equal but byte-different).
pub fn is_case_only_pair(a: &str, b: &str) -> bool {
    a != b && fold_path_key(a) == fold_path_key(b)
}

/// Group paths that collide under the fold (groups, not pairs — Foo/foo/FOO
/// all land in one group). Returns only groups with ≥2 members, each group
/// in first-seen order.
pub fn find_case_collision_groups<'a, I>(paths: I) -> Vec<Vec<&'a str>>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut by_key: HashMap<String, Vec<&'a str>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for path in paths {
        let key = fold_path_key(path);
        let entry = by_key.entry(key.clone()).or_default();
        if entry.is_empty() {
            order.push(key);
        }
        entry.push(path);
    }
    order
        .into_iter()
        .filter_map(|key| {
            let group = by_key.remove(&key)?;
            (group.len() >= 2).then_some(group)
        })
        .collect()
}

/// Whether two existing paths are the SAME filesystem entry (device+inode on
/// Unix; best-effort canonicalize equality elsewhere — documented weaker).
pub fn same_file_entry(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) {
            (Ok(ma), Ok(mb)) => ma.dev() == mb.dev() && ma.ino() == mb.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(ca), Ok(cb)) => ca == cb,
            _ => false,
        }
    }
}

/// Whether `candidate` and `tracked` differ only by case and resolve to the
/// same worktree entry. This is the case-only alias path on case-insensitive
/// filesystems; it is not an index twin.
pub fn is_same_file_case_alias(workdir: &Path, candidate: &Path, tracked: &Path) -> bool {
    let candidate_text = candidate.to_string_lossy();
    let tracked_text = tracked.to_string_lossy();
    candidate_text != tracked_text
        && fold_path_key(candidate_text.as_ref()) == fold_path_key(tracked_text.as_ref())
        && same_file_entry(&workdir.join(candidate), &workdir.join(tracked))
}

fn parse_ignore_case_value(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" | "" => Ok(false),
        other => Err(anyhow!(
            "unsupported core.ignorecase '{other}' (expected a boolean)"
        )),
    }
}

/// The repo's EFFECTIVE case-insensitivity: explicit `core.ignorecase`
/// (git-bool, invalid = hard error) wins; otherwise a per-process runtime
/// probe of the workdir; missing workdir → false (guards no-op).
pub async fn effective_ignore_case() -> Result<bool> {
    let entry = ConfigKv::get_var_case_insensitive("core.", "ignorecase")
        .await
        .map_err(|error| anyhow!("failed to read core.ignorecase: {error}"))?;
    if let Some(entry) = entry {
        return parse_ignore_case_value(&entry.value);
    }
    Ok(probe_workdir_ignore_case())
}

/// Synchronous counterpart for status helpers that are intentionally sync.
///
/// It honors the explicit `core.ignorecase` override first and falls back to an
/// uncached probe of the supplied workdir, so callers do not accidentally reuse
/// the process-wide probe for a different repository.
pub fn effective_ignore_case_for_dir_sync(dir: &Path) -> Result<bool> {
    let value = core_ignorecase_value_sync(dir)?;
    match value {
        Some(value) => parse_ignore_case_value(&value),
        None => Ok(probe_dir_ignore_case(dir)),
    }
}

fn core_ignorecase_value_sync(dir: &Path) -> Result<Option<String>> {
    let storage = util::try_get_storage_path(Some(dir.to_path_buf())).map_err(|error| {
        anyhow!(
            "failed to resolve repository storage for {}: {error}",
            dir.display()
        )
    })?;
    let db_path = storage.join(util::DATABASE);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<Option<String>> {
            let runtime = tokio::runtime::Runtime::new()
                .map_err(|err| anyhow!("failed to create tokio runtime: {err}"))?;
            runtime.block_on(async {
                let db_path_text = db_path.to_str().ok_or_else(|| {
                    anyhow!(
                        "repository database path is not valid UTF-8: {}",
                        db_path.display()
                    )
                })?;
                let db = establish_connection(db_path_text).await.map_err(|error| {
                    anyhow!(
                        "failed to open repository database {}: {error}",
                        db_path.display()
                    )
                })?;
                let value =
                    ConfigKv::get_var_case_insensitive_with_conn(&db, "core.", "ignorecase")
                        .await
                        .map(|entry| entry.map(|entry| entry.value));
                db.close().await.map_err(|error| {
                    anyhow!(
                        "failed to close repository database {}: {error}",
                        db_path.display()
                    )
                })?;
                value
            })
        })();
        let _ = tx.send(result);
    });
    rx.recv()
        .map_err(|_| anyhow!("core.ignorecase sync worker exited unexpectedly"))?
        .map_err(|error| anyhow!("failed to read core.ignorecase: {error}"))
}

/// Runtime probe (cached per process): does the working directory's
/// filesystem resolve a case-swapped `.libra` spelling to the same entry?
pub fn probe_workdir_ignore_case() -> bool {
    use std::sync::OnceLock;
    static PROBE: OnceLock<bool> = OnceLock::new();
    *PROBE.get_or_init(|| {
        let Ok(workdir) = crate::utils::util::try_working_dir() else {
            return false;
        };
        probe_dir_ignore_case(&workdir)
    })
}

/// Uncached probe for a specific directory (init-time use): stat the
/// case-swapped `.libra` spelling and confirm identity.
pub fn probe_dir_ignore_case(dir: &Path) -> bool {
    let lower = dir.join(".libra");
    let swapped = dir.join(".LIBRA");
    if std::fs::symlink_metadata(&lower).is_err() {
        return false;
    }
    if std::fs::symlink_metadata(&swapped).is_err() {
        return false;
    }
    // A hit could be a GENUINE `.LIBRA` sibling on a case-sensitive FS —
    // confirm both spellings resolve to the same entry.
    same_file_entry(&lower, &swapped)
}

/// Tree-materialization collision guard (checkout/switch, lore.md 1.14): on
/// a case-insensitive view, refuse (policy `error`) or warn before writing a
/// tree whose paths collide under the fold — a partial write IS the data
/// loss, so the refusal is atomic and lists every colliding GROUP.
pub async fn guard_tree_case_collisions(
    tree_paths: &[String],
) -> Result<(), crate::utils::error::CliError> {
    use crate::utils::error::{CliError, StableErrorCode};
    let ignore_case = effective_ignore_case().await.map_err(|error| {
        CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    if !ignore_case {
        return Ok(());
    }
    let groups = find_case_collision_groups(tree_paths.iter().map(String::as_str));
    if groups.is_empty() {
        return Ok(());
    }
    let policy = case_handling_from_config().await.map_err(|error| {
        CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    let listing = groups
        .iter()
        .map(|group| group.join(" / "))
        .collect::<Vec<_>>()
        .join("; ");
    match policy {
        CaseHandling::Error => Err(CliError::failure(format!(
            "target tree contains paths that collide on this case-insensitive filesystem: \
             {listing}"
        ))
        .with_stable_code(StableErrorCode::ConflictCaseCollision)
        .with_hint("set core.casehandling=warn to proceed (later-written paths win, like git)")),
        CaseHandling::Warn => {
            for group in &groups {
                crate::utils::error::emit_warning(format!(
                    "case-fold collision in the target tree: {} (later-written paths win)",
                    group.join(" / ")
                ));
            }
            Ok(())
        }
        CaseHandling::Allow => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;

    #[test]
    fn fold_and_pair_semantics() {
        assert!(is_case_only_pair("Foo.txt", "foo.txt"));
        assert!(is_case_only_pair("src/Über.rs", "src/über.rs"));
        assert!(
            !is_case_only_pair("foo.txt", "foo.txt"),
            "byte-equal is not a pair"
        );
        assert!(!is_case_only_pair("foo.txt", "bar.txt"));
    }

    #[test]
    fn casefold_prefix_check_is_component_aware() {
        assert!(path_starts_with_casefold(
            Path::new("Slides/Deck.md"),
            Path::new("slides")
        ));
        assert!(path_starts_with_casefold(
            Path::new("Slides"),
            Path::new("slides")
        ));
        assert!(!path_starts_with_casefold(
            Path::new("SlidesExtra/Deck.md"),
            Path::new("slides")
        ));
    }

    #[test]
    fn collision_groups_not_pairs() {
        let paths = ["Foo", "foo", "FOO", "bar", "Baz", "baz"];
        let groups = find_case_collision_groups(paths);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0], vec!["Foo", "foo", "FOO"]);
        assert_eq!(groups[1], vec!["Baz", "baz"]);
    }

    #[test]
    fn case_handling_parse_matrix() {
        assert_eq!(CaseHandling::parse("error").unwrap(), CaseHandling::Error);
        assert_eq!(CaseHandling::parse("WARN").unwrap(), CaseHandling::Warn);
        assert_eq!(CaseHandling::parse(" allow ").unwrap(), CaseHandling::Allow);
        assert!(
            CaseHandling::parse("bogus").is_err(),
            "typo is a hard error"
        );
    }

    #[test]
    fn probe_is_honest_on_this_fs() {
        // On the (case-sensitive) CI filesystem a temp dir with `.libra`
        // must probe false; creating a REAL `.LIBRA` sibling must still
        // probe false (different entries).
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".libra")).unwrap();
        // (On a genuinely case-insensitive FS this assert flips — the probe
        // is answering the question correctly either way.)
        let insensitive = probe_dir_ignore_case(dir.path());
        if !insensitive {
            std::fs::create_dir(dir.path().join(".LIBRA")).unwrap();
            assert!(
                !probe_dir_ignore_case(dir.path()),
                "a genuine .LIBRA sibling is not case-insensitivity"
            );
        }
    }

    #[test]
    fn same_file_case_alias_requires_same_entry() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".libra")).unwrap();
        let lower = dir.path().join("slides.txt");
        std::fs::write(&lower, "content").unwrap();

        if probe_dir_ignore_case(dir.path()) {
            assert!(is_same_file_case_alias(
                dir.path(),
                Path::new("Slides.txt"),
                Path::new("slides.txt")
            ));
            return;
        }

        std::fs::write(dir.path().join("Slides.txt"), "other").unwrap();
        assert!(!is_same_file_case_alias(
            dir.path(),
            Path::new("Slides.txt"),
            Path::new("slides.txt")
        ));

        #[cfg(unix)]
        {
            std::fs::remove_file(dir.path().join("Slides.txt")).unwrap();
            std::fs::hard_link(&lower, dir.path().join("Slides.txt")).unwrap();
            assert!(is_same_file_case_alias(
                dir.path(),
                Path::new("Slides.txt"),
                Path::new("slides.txt")
            ));
        }
    }

    #[tokio::test]
    #[serial]
    async fn sync_ignore_case_uses_supplied_repo_dir_not_process_cwd() {
        let repo = tempfile::tempdir().unwrap();
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;

        {
            let _guard = crate::utils::test::ChangeDirGuard::new(repo.path());
            ConfigKv::set("core.ignorecase", "true", false)
                .await
                .unwrap();
        }

        let outside = tempfile::tempdir().unwrap();
        let _outside_guard = crate::utils::test::ChangeDirGuard::new(outside.path());

        assert!(
            effective_ignore_case_for_dir_sync(repo.path()).unwrap(),
            "sync lookup must use the explicit repo dir, not the process cwd"
        );
    }
}
