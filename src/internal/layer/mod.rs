//! Lore's `layer` local-overlay primitive (lore.md 2.4).
//!
//! A **layer** is a named, purely-LOCAL overlay: a stack of files materialized
//! onto the working tree on explicit command, which NEVER enters a commit. It
//! is the Phase-2 landable half of the §3.5 composition pair (its versioned
//! sibling `link` is deferred to the §3.4 RFC); the §3.5 red line forbids a
//! *default* auto-compose model, not this opt-in, explicit-command overlay.
//!
//! This module is the SOLE owner of the `layer` and `layer_path` tables
//! (§3.6): no command performs lazy DDL or touches the rows directly. Two
//! guarantees underpin the primitive:
//!
//! 1. **Never-enters-commit** — enforced at TWO chokepoints, because a single
//!    one is not airtight: (a) materialized paths are injected into the ignore
//!    resolver as a highest-precedence, UN-NEGATABLE exclusion (keeps default
//!    `status`/`add` blind to them); and (b) a hard guard in the `add` staging
//!    path refuses to stage any layer-owned path REGARDLESS of ignore policy —
//!    closing the `add --force` hole that bypasses ignore filtering.
//! 2. **Never-clobbers** — a layer destination that collides with a tracked
//!    (index or HEAD) path is rejected at `apply` time (`LBR-LAYER-001`,
//!    fail-closed, nothing written); a user-edited overlay file is skipped by
//!    `unapply`/`remove` (content-hash mismatch), never deleted.

use std::{
    collections::{BTreeMap, HashSet},
    path::{Component, Path, PathBuf},
};

use git_internal::{hash::ObjectHash, internal::object::types::ObjectType};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};

use crate::{
    internal::{db::get_db_conn_instance, worktree_scope::WorktreeScope},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        util,
    },
};

/// A registered overlay.
#[derive(Debug, Clone)]
pub struct Layer {
    pub name: String,
    pub source: String,
    pub priority: i64,
    pub enabled: bool,
}

/// A materialized overlay path record.
#[derive(Debug, Clone)]
pub struct MaterializedPath {
    pub layer_name: String,
    pub path: String,
    pub content_hash: String,
}

/// Single-owner store over `layer` + `layer_path`.
pub struct LayerStore;

impl LayerStore {
    /// All layers registered in `scope`, ordered (priority ASC, name ASC) —
    /// the deterministic apply stack order (higher priority materializes
    /// last, so it wins a same-destination collision). W1 §C.4.1.1: every
    /// method takes the request's ONE resolved [`WorktreeScope`] — the same
    /// layer name may exist independently in different worktrees.
    pub async fn list(scope: &WorktreeScope) -> Result<Vec<Layer>, String> {
        let db = get_db_conn_instance().await;
        let stmt = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT name, source, priority, enabled FROM layer \
             WHERE worktree_id = ? ORDER BY priority ASC, name ASC",
            [scope.storage_key().into()],
        );
        let rows = db
            .query_all(stmt)
            .await
            .map_err(|e| format!("failed to list layers: {e}"))?;
        let mut layers = Vec::with_capacity(rows.len());
        for row in rows {
            layers.push(Layer {
                name: row.try_get_by_index(0).map_err(|e| e.to_string())?,
                source: row.try_get_by_index(1).map_err(|e| e.to_string())?,
                priority: row.try_get_by_index(2).map_err(|e| e.to_string())?,
                enabled: row.try_get_by_index::<i32>(3).map_err(|e| e.to_string())? != 0,
            });
        }
        Ok(layers)
    }

    /// Register a new layer in `scope`. Duplicate names (within the scope)
    /// are rejected by the UNIQUE constraint, surfaced as a clean error.
    pub async fn add(
        scope: &WorktreeScope,
        name: &str,
        source: &str,
        priority: i64,
        enabled: bool,
    ) -> Result<(), String> {
        let db = get_db_conn_instance().await;
        db.execute(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO layer (worktree_id, name, source, priority, enabled) \
             VALUES (?, ?, ?, ?, ?)",
            [
                scope.storage_key().into(),
                name.into(),
                source.into(),
                priority.into(),
                (if enabled { 1 } else { 0 }).into(),
            ],
        ))
        .await
        .map_err(|e| {
            if e.to_string().contains("UNIQUE") {
                format!("a layer named '{name}' already exists")
            } else {
                format!("failed to add layer: {e}")
            }
        })?;
        Ok(())
    }

    /// Look up one layer by name within `scope`.
    pub async fn get(scope: &WorktreeScope, name: &str) -> Result<Option<Layer>, String> {
        Ok(Self::list(scope)
            .await?
            .into_iter()
            .find(|l| l.name == name))
    }

    /// Enable/disable a layer in `scope`. Returns whether a row was affected.
    pub async fn set_enabled(
        scope: &WorktreeScope,
        name: &str,
        enabled: bool,
    ) -> Result<bool, String> {
        let db = get_db_conn_instance().await;
        let result = db
            .execute(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "UPDATE layer SET enabled = ?, updated_at = CURRENT_TIMESTAMP \
                 WHERE worktree_id = ? AND name = ?",
                [
                    (if enabled { 1 } else { 0 }).into(),
                    scope.storage_key().into(),
                    name.into(),
                ],
            ))
            .await
            .map_err(|e| format!("failed to update layer: {e}"))?;
        Ok(result.rows_affected() > 0)
    }

    /// Remove a layer registration and its path records within `scope` (the
    /// caller unapplies the materialized files first).
    pub async fn remove(scope: &WorktreeScope, name: &str) -> Result<bool, String> {
        let db = get_db_conn_instance().await;
        let txn = db.begin().await.map_err(|e| e.to_string())?;
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "DELETE FROM layer_path WHERE worktree_id = ? AND layer_name = ?",
            [scope.storage_key().into(), name.into()],
        ))
        .await
        .map_err(|e| format!("failed to clear layer paths: {e}"))?;
        let result = txn
            .execute(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "DELETE FROM layer WHERE worktree_id = ? AND name = ?",
                [scope.storage_key().into(), name.into()],
            ))
            .await
            .map_err(|e| format!("failed to remove layer: {e}"))?;
        txn.commit().await.map_err(|e| e.to_string())?;
        Ok(result.rows_affected() > 0)
    }

    /// Every overlay path currently materialized in `scope` (repo-relative,
    /// '/'-sep). This is the set the ignore resolver and the `add` guard
    /// consult; an empty set (no layers applied) makes both a zero-overhead
    /// no-op.
    pub async fn materialized_paths(
        scope: &WorktreeScope,
    ) -> Result<Vec<MaterializedPath>, String> {
        let db = get_db_conn_instance().await;
        let stmt = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT layer_name, path, content_hash FROM layer_path WHERE worktree_id = ?",
            [scope.storage_key().into()],
        );
        let rows = match db.query_all(stmt).await {
            Ok(rows) => rows,
            // Absence-tolerant: before the migration created the table (or on
            // an old binary), there are simply no materialized paths.
            Err(e) if e.to_string().contains("no such table") => return Ok(Vec::new()),
            Err(e) => return Err(format!("failed to list layer paths: {e}")),
        };
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(MaterializedPath {
                layer_name: row.try_get_by_index(0).map_err(|e| e.to_string())?,
                path: row.try_get_by_index(1).map_err(|e| e.to_string())?,
                content_hash: row.try_get_by_index(2).map_err(|e| e.to_string())?,
            });
        }
        Ok(out)
    }

    /// The set of layer-owned paths in `scope` as a fast lookup (for the
    /// ignore resolver's snapshot refresh). Errors resolve to an EMPTY set so
    /// a probe failure never blocks normal `status`/`add` — the `add` staging
    /// guard and the apply path (which mutate) surface real errors instead of
    /// consuming this advisory set.
    pub async fn owned_path_set(scope: &WorktreeScope) -> HashSet<String> {
        Self::materialized_paths(scope)
            .await
            .map(|paths| paths.into_iter().map(|p| p.path).collect())
            .unwrap_or_default()
    }
}

/// Process-global snapshot of layer-owned paths, consulted SYNCHRONOUSLY by
/// the ignore resolver (which cannot await a DB read). Async command entry
/// points that enumerate the working tree (`status`, `add`) call
/// [`refresh_exclusion_snapshot`] with their ONE resolved scope first, so the
/// sync consult is consistent within one command; the default (no layers) is
/// an empty set → zero overhead and byte-identical pre-feature behavior.
///
/// W1 §C.4.1.1: the snapshot is keyed by the scope that loaded it (storage
/// key), so it can never silently carry another worktree's set across a
/// refresh. The sync consult itself trusts the same-command guarantee — a
/// concurrent multi-scope runtime is fail-closed at W0 preflight until W4
/// threads a per-request snapshot through the ignore engine (per-consult
/// scope resolution would put a filesystem probe in the per-path hot loop).
static EXCLUSION_SNAPSHOT: std::sync::RwLock<Option<(String, HashSet<String>)>> =
    std::sync::RwLock::new(None);

/// Load `scope`'s layer-owned path set into the process snapshot. Cheap and
/// idempotent; call at the start of any command that enumerates untracked
/// files so layer overlays are excluded like ignored paths.
pub async fn refresh_exclusion_snapshot(scope: &WorktreeScope) {
    let set = LayerStore::owned_path_set(scope).await;
    let mut guard = EXCLUSION_SNAPSHOT
        .write()
        .unwrap_or_else(|poison| poison.into_inner());
    *guard = Some((scope.storage_key().to_string(), set));
}

/// SYNC un-negatable consult: is `path_norm` (repo-relative, '/'-sep) a
/// layer-owned overlay path in the scope that last refreshed the snapshot
/// (the current command's scope)? A `!path` negation in `.libraignore` must
/// NOT be able to un-exclude it. Returns `false` when the snapshot is
/// empty/unloaded.
pub fn is_layer_owned(path_norm: &str) -> bool {
    EXCLUSION_SNAPSHOT
        .read()
        .unwrap_or_else(|poison| poison.into_inner())
        .as_ref()
        .is_some_and(|(_scope, set)| set.contains(path_norm))
}

/// Normalize an arbitrary worktree-relative path to the snapshot's key form.
pub fn normalize_key(path: &Path) -> Option<String> {
    normalize_rel(path)
}

impl LayerStore {
    /// Transactionally replace `scope`'s `layer_path` records (scoped
    /// DELETE+INSERT, torn-write-safe like `internal::sequencer::save`).
    /// Another worktree's ownership rows are never touched.
    async fn rewrite_paths(
        scope: &WorktreeScope,
        records: &[MaterializedPath],
    ) -> Result<(), String> {
        let db = get_db_conn_instance().await;
        let txn = db.begin().await.map_err(|e| e.to_string())?;
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "DELETE FROM layer_path WHERE worktree_id = ?",
            [scope.storage_key().into()],
        ))
        .await
        .map_err(|e| format!("failed to clear layer paths: {e}"))?;
        for record in records {
            txn.execute(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "INSERT INTO layer_path (worktree_id, layer_name, path, content_hash) \
                 VALUES (?, ?, ?, ?)",
                [
                    scope.storage_key().into(),
                    record.layer_name.as_str().into(),
                    record.path.as_str().into(),
                    record.content_hash.as_str().into(),
                ],
            ))
            .await
            .map_err(|e| format!("failed to record layer path: {e}"))?;
        }
        txn.commit().await.map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// W1 §C.4.1.1 scope↔workdir binding: a MUTATION entry point verifies that
/// the scope it was handed matches the working tree it is about to touch. A
/// concurrent process-cwd switch between the caller's scope resolution and
/// the mutation (async await points in between) must fail closed — never
/// materialize/prune one worktree's files against another worktree's
/// ownership rows.
fn verify_scope_matches_workdir(scope: &WorktreeScope, workdir: &Path) -> CliResult<()> {
    let derived = WorktreeScope::for_workdir(workdir);
    if derived != *scope {
        let show = |s: &WorktreeScope| match s.storage_key() {
            "" => "the main worktree".to_string(),
            id => format!("worktree '{id}'"),
        };
        return Err(CliError::fatal(format!(
            "the worktree scope changed mid-command: this request resolved {}, but the working \
             tree at '{}' belongs to {}",
            show(scope),
            workdir.display(),
            show(&derived)
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
        .with_hint("re-run the command from inside the target worktree"));
    }
    Ok(())
}

/// Last-moment staging-context re-verification for the `add` hard guard
/// (W1 §C.4.1.1): after the awaits between entry and staging, the process
/// cwd must still resolve to the captured workdir AND that workdir's scope
/// must still be the one this request resolved — otherwise the guard would
/// protect a different tree than the index being written. Fail closed.
pub(crate) fn verify_staging_context(workdir: &Path, scope: &WorktreeScope) -> CliResult<()> {
    verify_workdir_unchanged(workdir)?;
    verify_scope_matches_workdir(scope, workdir)
}

/// Companion re-check for the await gaps AFTER the entry binding: the
/// index/HEAD reads and the ambient path helpers resolve from the process
/// cwd, so a cwd switch to another worktree mid-command would split them
/// from the captured workdir. Cheap (one cwd read), called before each
/// cwd-dependent phase — fail closed on any drift.
fn verify_workdir_unchanged(workdir: &Path) -> CliResult<()> {
    let now = util::working_dir();
    if now != *workdir {
        return Err(CliError::fatal(format!(
            "the working directory changed while the layer operation was running \
             (started in '{}', now '{}'); nothing further was written",
            workdir.display(),
            now.display()
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
        .with_hint("re-run the command from inside the target worktree"));
    }
    Ok(())
}

/// Content hash of a file's bytes, in the repo's active blob-object framing
/// (so it is stable and hash-kind-agnostic across the store).
fn hash_bytes(data: &[u8]) -> String {
    ObjectHash::from_type_and_data(ObjectType::Blob, data).to_string()
}

/// Normalize a repo-relative path to the '/'-separated, `Normal`-components
/// form the tables and ignore engine use.
fn normalize_rel(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            // Any `..`, absolute, or prefix component is a worktree escape.
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// The set of tracked paths (index + HEAD tree), '/'-normalized, used to
/// reject a layer destination that would shadow committed content.
async fn tracked_path_set() -> Result<HashSet<String>, String> {
    use git_internal::internal::{
        index::Index,
        object::{commit::Commit, tree::Tree},
    };

    use crate::{
        internal::head::Head,
        utils::object_ext::{CommitExt, TreeExt},
    };

    let mut set = HashSet::new();
    let index_path = crate::utils::path::index();
    // Fail CLOSED on a real index-load error (Codex P1): a corrupt/unreadable
    // index must NOT let apply proceed blind to index-tracked paths. Only a
    // genuinely absent index (unborn repo) is tolerated.
    if index_path.exists() {
        let index = Index::load(&index_path)
            .map_err(|e| format!("cannot read the index for collision checking: {e}"))?;
        for path in index.tracked_files() {
            if let Some(norm) = normalize_rel(&path) {
                set.insert(norm);
            }
        }
    }
    // HEAD tree (covers a committed-but-not-in-index edge).
    if let Head::Branch(_) | Head::Detached(_) = Head::current().await
        && let Some(head_oid) = Head::current_commit().await
        && let Some(commit) = Commit::try_load(&head_oid)
        && let Some(tree) = Tree::try_load(&commit.tree_id)
    {
        for (path, _hash) in tree.get_plain_items() {
            if let Some(norm) = normalize_rel(&path) {
                set.insert(norm);
            }
        }
    }
    Ok(set)
}

/// Recursively enumerate a source directory into (repo-relative dest, absolute
/// source file) pairs. Rejects symlinks and any path that escapes the
/// worktree or lands in `.libra/`.
fn enumerate_source(source_root: &Path, workdir: &Path) -> CliResult<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    let mut stack = vec![source_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| {
            CliError::fatal(format!("cannot read layer source '{}': {e}", dir.display()))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        for entry in entries {
            let entry = entry.map_err(|e| {
                CliError::fatal(format!("cannot read layer source entry: {e}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })?;
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).map_err(|e| {
                CliError::fatal(format!("cannot stat '{}': {e}", path.display()))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })?;
            if meta.file_type().is_symlink() {
                return Err(CliError::fatal(format!(
                    "layer source contains a symlink '{}', which is not supported",
                    path.display()
                ))
                .with_stable_code(StableErrorCode::CliInvalidArguments));
            }
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path.strip_prefix(source_root).map_err(|_| {
                CliError::internal("layer enumeration produced a non-relative path")
            })?;
            let Some(dest) = normalize_rel(rel) else {
                return Err(CliError::fatal(format!(
                    "layer source path '{}' does not map to a safe worktree path",
                    rel.display()
                ))
                .with_stable_code(StableErrorCode::CliInvalidArguments));
            };
            // Never allow materializing into the metadata dir or an ignore file
            // (which would perturb the very engine the invariant relies on).
            if dest.starts_with(".libra/")
                || dest == ".libraignore"
                || dest == ".gitignore"
                || dest.ends_with("/.libraignore")
                || dest.ends_with("/.gitignore")
            {
                return Err(CliError::fatal(format!(
                    "layer cannot materialize into '{dest}' (reserved / ignore-affecting path)"
                ))
                .with_stable_code(StableErrorCode::CliInvalidArguments));
            }
            // The destination must resolve inside the worktree.
            if !util::is_sub_path(workdir.join(&dest), workdir) {
                return Err(CliError::fatal(format!(
                    "layer destination '{dest}' escapes the worktree"
                ))
                .with_stable_code(StableErrorCode::CliInvalidArguments));
            }
            out.push((dest, path));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Outcome of an `apply`.
#[derive(Debug, Default)]
pub struct ApplyReport {
    pub written: usize,
    pub pruned: usize,
    pub layers: usize,
}

/// Materialize all enabled layers onto the working tree (lore.md 2.4).
///
/// Two-phase, fail-closed (Codex-hardened): a VALIDATION phase reads every
/// source and checks every destination WITHOUT touching the working tree, so
/// a bad source, a tracked-path collision, or a destination occupied by an
/// untracked user file / edited overlay aborts with `LBR-LAYER-001` and
/// NOTHING written or pruned. Only once all destinations are proven safe does
/// the MUTATION phase prune stale unmodified materializations, write the new
/// overlay, and rewrite the records.
pub async fn apply(scope: &WorktreeScope) -> CliResult<ApplyReport> {
    if util::find_git_repository(None).is_some_and(|loc| loc.is_bare) {
        return Err(CliError::fatal("cannot apply layers in a bare repository")
            .with_stable_code(StableErrorCode::RepoStateInvalid));
    }
    let workdir = util::working_dir();
    verify_scope_matches_workdir(scope, &workdir)?;
    // Canonical worktree root for the source-inside-worktree check.
    let workdir_canon = std::fs::canonicalize(&workdir).unwrap_or_else(|_| workdir.clone());
    let layers = LayerStore::list(scope)
        .await
        .map_err(|e| CliError::fatal(format!("failed to load layers: {e}")))?;
    let enabled: Vec<&Layer> = layers.iter().filter(|l| l.enabled).collect();

    // Build the effective overlay map dest -> (layer, source file), higher
    // priority (later in the ordered list) overwriting lower.
    let mut overlay: BTreeMap<String, (String, PathBuf)> = BTreeMap::new();
    for layer in &enabled {
        let source_root = PathBuf::from(&layer.source);
        if !source_root.is_dir() {
            return Err(CliError::fatal(format!(
                "layer '{}' source '{}' is not a directory",
                layer.name, layer.source
            ))
            .with_stable_code(StableErrorCode::IoReadFailed));
        }
        // Reject a source dir INSIDE the worktree (Codex P1): it would
        // materialize files back onto the worktree at a different depth and
        // its own untracked source files would be swept by `add`.
        if let Ok(source_canon) = std::fs::canonicalize(&source_root)
            && source_canon.starts_with(&workdir_canon)
        {
            return Err(CliError::fatal(format!(
                "layer '{}' source '{}' is inside the working tree; layer sources must be \
                 external directories",
                layer.name, layer.source
            ))
            .with_stable_code(StableErrorCode::CliInvalidArguments));
        }
        for (dest, src) in enumerate_source(&source_root, &workdir)? {
            overlay.insert(dest, (layer.name.clone(), src));
        }
    }

    // ── VALIDATION phase (no mutation) ──
    // Collision with a tracked path. `tracked_path_set` reads the index/HEAD
    // through cwd-ambient paths — re-verify the cwd is still the bound
    // workdir after the awaits above (fail closed on drift).
    verify_workdir_unchanged(&workdir)?;
    let tracked = tracked_path_set()
        .await
        .map_err(|e| CliError::fatal(format!("failed to read tracked paths: {e}")))?;
    if let Some(dest) = overlay.keys().find(|k| tracked.contains(*k)) {
        return Err(CliError::fatal(format!(
            "layer apply aborted: '{dest}' collides with tracked content — a layer may only \
             add paths the base does not track"
        ))
        .with_stable_code(StableErrorCode::LayerConflict)
        .with_hint("rename the layer source path, or untrack the base path first"));
    }
    // Read all sources up front + prove each destination is safe to write.
    let previous = LayerStore::materialized_paths(scope)
        .await
        .map_err(|e| CliError::fatal(format!("failed to read materialized paths: {e}")))?;
    let prior: std::collections::HashMap<&str, &str> = previous
        .iter()
        .map(|r| (r.path.as_str(), r.content_hash.as_str()))
        .collect();
    let mut planned: Vec<(String, String, Vec<u8>)> = Vec::with_capacity(overlay.len());
    for (dest, (layer_name, src)) in &overlay {
        let data = std::fs::read(src).map_err(|e| {
            CliError::fatal(format!("cannot read layer source '{}': {e}", src.display()))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        // Never clobber a destination that already holds content we do NOT own
        // (Codex P1). Check by METADATA, not fs::read: a directory, symlink, or
        // unreadable occupant would else read as "absent" and be silently
        // pruned/overwritten in the mutation phase.
        let abs = workdir.join(dest);
        match std::fs::symlink_metadata(&abs) {
            Ok(meta) if meta.is_file() => {
                let existing = std::fs::read(&abs).map_err(|e| {
                    CliError::fatal(format!("cannot read '{dest}': {e}"))
                        .with_stable_code(StableErrorCode::IoReadFailed)
                })?;
                let existing_hash = hash_bytes(&existing);
                let ours_unmodified = prior.get(dest.as_str()) == Some(&existing_hash.as_str());
                if !ours_unmodified {
                    return Err(CliError::fatal(format!(
                        "layer apply aborted: '{dest}' already exists and is not an unmodified \
                         layer file — refusing to overwrite local content"
                    ))
                    .with_stable_code(StableErrorCode::LayerConflict)
                    .with_hint(
                        "move or remove the existing file, or 'libra layer unapply' first",
                    ));
                }
            }
            Ok(_) => {
                // A directory, symlink, or other non-regular occupant.
                return Err(CliError::fatal(format!(
                    "layer apply aborted: '{dest}' exists and is not a regular file"
                ))
                .with_stable_code(StableErrorCode::LayerConflict));
            }
            Err(_) => {} // absent — fine
        }
        // A parent component occupied by a NON-directory would make the
        // mutation phase's `create_dir_all` fail after pruning — reject now.
        let parts: Vec<&str> = dest.split('/').collect();
        let mut ancestor = workdir.clone();
        for part in &parts[..parts.len().saturating_sub(1)] {
            ancestor = ancestor.join(part);
            if let Ok(meta) = std::fs::symlink_metadata(&ancestor)
                && !meta.is_dir()
            {
                return Err(CliError::fatal(format!(
                    "layer apply aborted: a parent of '{dest}' exists as a non-directory"
                ))
                .with_stable_code(StableErrorCode::LayerConflict));
            }
        }
        planned.push((dest.clone(), layer_name.clone(), data));
    }

    // ── MUTATION phase (all destinations proven safe) ──
    // Last re-check before anything is pruned or written.
    verify_workdir_unchanged(&workdir)?;
    let mut report = ApplyReport {
        layers: enabled.len(),
        ..Default::default()
    };
    // Prune previously-materialized paths no longer produced. Only remove
    // UNMODIFIED files (never clobber an edit). A file stays layer-owned
    // (record carried forward) if it was EDITED, if removal FAILED (fail
    // closed — Codex P1: a file left on disk must never lose its ownership),
    // or on a non-NotFound read error; only a genuinely-gone file drops its
    // record.
    let overlay_dests: HashSet<&String> = overlay.keys().collect();
    let mut carried_records: Vec<MaterializedPath> = Vec::new();
    for record in &previous {
        if overlay_dests.contains(&record.path) {
            continue;
        }
        let abs = workdir.join(&record.path);
        match std::fs::read(&abs) {
            Ok(data) if hash_bytes(&data) == record.content_hash => {
                match std::fs::remove_file(&abs) {
                    Ok(()) => report.pruned += 1,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => report.pruned += 1,
                    // Removal failed: the file is still on disk — keep it owned.
                    Err(_) => carried_records.push(record.clone()),
                }
            }
            Ok(_) => carried_records.push(record.clone()), // edited: keep owned
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {} // genuinely gone: drop
            Err(_) => carried_records.push(record.clone()), // read error: fail-closed, keep owned
        }
    }

    // Persist OWNERSHIP for the new overlay BEFORE writing the files (Codex
    // P1): if a file write later fails, the result is a record-without-file
    // (owned → excluded/guarded, recoverable by re-apply), NEVER a
    // file-without-record (which could enter a commit). RESIDUAL (recovery
    // ergonomics, not an invariant break): if a write fails AFTER the record
    // is stored with the NEW hash, a re-apply sees the old on-disk bytes as
    // "edited" and preserves them — the user runs `layer unapply` + re-apply
    // to reconcile. The commit/clobber invariants hold throughout.
    let mut records = Vec::with_capacity(planned.len() + carried_records.len());
    for (dest, layer_name, data) in &planned {
        records.push(MaterializedPath {
            layer_name: layer_name.clone(),
            path: dest.clone(),
            content_hash: hash_bytes(data),
        });
    }
    records.extend(carried_records);
    LayerStore::rewrite_paths(scope, &records)
        .await
        .map_err(|e| CliError::fatal(format!("failed to record materialized paths: {e}")))?;

    // Now materialize the files (ownership already recorded).
    for (dest, _layer_name, data) in &planned {
        let abs = workdir.join(dest);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CliError::fatal(format!("cannot create '{}': {e}", parent.display()))
                    .with_stable_code(StableErrorCode::IoWriteFailed)
            })?;
        }
        // Find the original source path (for mode copy) via the overlay map.
        if let Some((_, src)) = overlay.get(dest) {
            crate::utils::atomic_write::write_atomic(&abs, data, false).map_err(|e| {
                CliError::fatal(format!("cannot materialize '{dest}': {e}"))
                    .with_stable_code(StableErrorCode::IoWriteFailed)
            })?;
            copy_mode(src, &abs);
        }
    }
    report.written = planned.len();
    Ok(report)
}

/// Remove materialized files (all, or one `--layer`). An UNMODIFIED file is
/// deleted and detached; a user-EDITED file is KEPT on disk AND stays
/// layer-owned (Codex P1 — an edited overlay must never silently become
/// committable via `unapply`; only an explicit `layer remove` detaches it).
/// Returns `(removed, kept_edited)`.
pub async fn unapply(
    scope: &WorktreeScope,
    layer_filter: Option<&str>,
) -> CliResult<(usize, usize)> {
    let workdir = util::working_dir();
    verify_scope_matches_workdir(scope, &workdir)?;
    let previous = LayerStore::materialized_paths(scope)
        .await
        .map_err(|e| CliError::fatal(format!("failed to read materialized paths: {e}")))?;
    let mut removed = 0usize;
    let mut skipped = 0usize;
    let mut remaining = Vec::new();
    for record in previous {
        if let Some(filter) = layer_filter
            && record.layer_name != filter
        {
            remaining.push(record);
            continue;
        }
        let abs = workdir.join(&record.path);
        match std::fs::read(&abs) {
            Ok(data) if hash_bytes(&data) == record.content_hash => {
                // Unmodified: remove and detach. If removal FAILS (not
                // NotFound), the file is still on disk — keep it owned
                // (fail-closed, Codex P1).
                match std::fs::remove_file(&abs) {
                    Ok(()) => removed += 1,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => removed += 1,
                    Err(_) => {
                        skipped += 1;
                        remaining.push(record);
                    }
                }
            }
            Ok(_) => {
                // Edited since materialization — keep the file AND KEEP the
                // record so it stays layer-owned (never silently becomes
                // committable). Only `layer remove` detaches it explicitly.
                skipped += 1;
                remaining.push(record);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Genuinely gone — detach.
                removed += 1;
            }
            Err(_) => {
                // A non-NotFound read error (permissions, etc.): keep it owned
                // rather than silently detaching a file we cannot inspect.
                skipped += 1;
                remaining.push(record);
            }
        }
    }
    LayerStore::rewrite_paths(scope, &remaining)
        .await
        .map_err(|e| CliError::fatal(format!("failed to update materialized paths: {e}")))?;
    Ok((removed, skipped))
}

#[cfg(unix)]
fn copy_mode(src: &Path, dest: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(src) {
        let _ = std::fs::set_permissions(
            dest,
            std::fs::Permissions::from_mode(meta.permissions().mode()),
        );
    }
}

#[cfg(not(unix))]
fn copy_mode(_src: &Path, _dest: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test::{ChangeDirGuard, setup_with_new_libra_in};

    #[tokio::test]
    #[serial_test::serial]
    async fn add_list_order_and_unique() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = ChangeDirGuard::new(tmp.path());
        setup_with_new_libra_in(tmp.path()).await;
        let scope = WorktreeScope::Main;

        LayerStore::add(&scope, "b", "/src/b", 5, true)
            .await
            .expect("add b");
        LayerStore::add(&scope, "a", "/src/a", 5, true)
            .await
            .expect("add a");
        LayerStore::add(&scope, "z", "/src/z", 1, false)
            .await
            .expect("add z");
        // Ordered priority ASC, name ASC: z(1), a(5), b(5).
        let layers = LayerStore::list(&scope).await.expect("list");
        let names: Vec<&str> = layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["z", "a", "b"]);
        assert!(!layers[0].enabled, "z registered disabled");

        // Duplicate name rejected.
        let err = LayerStore::add(&scope, "a", "/other", 0, true)
            .await
            .expect_err("dup");
        assert!(err.contains("already exists"), "{err}");

        // Enable/disable + remove.
        assert!(
            LayerStore::set_enabled(&scope, "z", true)
                .await
                .expect("enable")
        );
        assert!(
            LayerStore::get(&scope, "z")
                .await
                .expect("get")
                .unwrap()
                .enabled
        );
        assert!(LayerStore::remove(&scope, "z").await.expect("remove"));
        assert!(LayerStore::get(&scope, "z").await.expect("get").is_none());
        assert!(
            !LayerStore::remove(&scope, "nope")
                .await
                .expect("remove-missing")
        );
    }

    /// W1 §C.4.1.1: two scopes hold same-named layers and same-destination
    /// path records independently; one scope's remove/rewrite never touches
    /// the other's rows.
    #[tokio::test]
    #[serial_test::serial]
    async fn scopes_are_isolated() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = ChangeDirGuard::new(tmp.path());
        setup_with_new_libra_in(tmp.path()).await;
        let main = WorktreeScope::Main;
        let linked = WorktreeScope::Linked("wt-test".to_string());

        // Same layer name registers independently in both scopes.
        LayerStore::add(&main, "ov", "/src/main", 0, true)
            .await
            .expect("main add");
        LayerStore::add(&linked, "ov", "/src/linked", 0, true)
            .await
            .expect("linked add");
        assert_eq!(
            LayerStore::get(&main, "ov")
                .await
                .expect("get")
                .unwrap()
                .source,
            "/src/main"
        );
        assert_eq!(
            LayerStore::get(&linked, "ov")
                .await
                .expect("get")
                .unwrap()
                .source,
            "/src/linked"
        );

        // Same destination path is owned independently per scope, and one
        // scope's rewrite (to empty) leaves the other's ownership intact.
        let record = |hash: &str| MaterializedPath {
            layer_name: "ov".to_string(),
            path: "same/dest.txt".to_string(),
            content_hash: hash.to_string(),
        };
        LayerStore::rewrite_paths(&main, &[record("h-main")])
            .await
            .expect("main paths");
        LayerStore::rewrite_paths(&linked, &[record("h-linked")])
            .await
            .expect("linked paths");
        LayerStore::rewrite_paths(&main, &[])
            .await
            .expect("main clear");
        let linked_paths = LayerStore::materialized_paths(&linked)
            .await
            .expect("linked paths");
        assert_eq!(linked_paths.len(), 1);
        assert_eq!(linked_paths[0].content_hash, "h-linked");
        assert!(
            LayerStore::materialized_paths(&main)
                .await
                .expect("main paths")
                .is_empty()
        );

        // The exclusion snapshot is keyed by the refreshing scope: a linked
        // refresh sees linked's ownership, a main refresh sees main's cleared
        // set — never the other scope's.
        refresh_exclusion_snapshot(&linked).await;
        assert!(is_layer_owned("same/dest.txt"));
        refresh_exclusion_snapshot(&main).await;
        assert!(!is_layer_owned("same/dest.txt"));

        // Scoped remove of the linked registration (and its path records)
        // leaves main's registration row untouched.
        assert!(LayerStore::remove(&linked, "ov").await.expect("remove"));
        assert!(LayerStore::get(&main, "ov").await.expect("get").is_some());
    }

    /// W1 §C.4.1.1 scope↔workdir binding: every fail-closed verification
    /// branch refuses on drift — a wrong scope for the workdir, a moved cwd,
    /// and the combined staging-context check the `add` hard guard calls —
    /// and the mutation entry points (`apply`/`unapply`) surface the same
    /// refusal before touching anything.
    #[tokio::test]
    #[serial_test::serial]
    async fn scope_workdir_binding_fails_closed_on_drift() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = ChangeDirGuard::new(tmp.path());
        setup_with_new_libra_in(tmp.path()).await;
        let workdir = util::working_dir();
        let wrong_scope = WorktreeScope::Linked("wt-elsewhere".to_string());

        // Matching pair passes; a scope that does not own the workdir fails.
        verify_scope_matches_workdir(&WorktreeScope::Main, &workdir).expect("main scope matches");
        let err = verify_scope_matches_workdir(&wrong_scope, &workdir)
            .expect_err("wrong scope must fail closed");
        assert!(
            format!("{err:?}").contains("scope changed"),
            "actionable scope error: {err:?}"
        );

        // Unchanged cwd passes; a foreign workdir (cwd drifted) fails.
        verify_workdir_unchanged(&workdir).expect("cwd unchanged");
        let elsewhere = tempfile::tempdir().expect("elsewhere");
        let err =
            verify_workdir_unchanged(elsewhere.path()).expect_err("cwd drift must fail closed");
        assert!(
            format!("{err:?}").contains("working directory changed"),
            "actionable drift error: {err:?}"
        );

        // The combined staging-context check (the `add` hard guard's gate)
        // refuses on either half.
        verify_staging_context(&workdir, &WorktreeScope::Main).expect("bound context passes");
        assert!(verify_staging_context(&workdir, &wrong_scope).is_err());
        assert!(verify_staging_context(elsewhere.path(), &WorktreeScope::Main).is_err());

        // Mutation entry points refuse a scope that does not own the cwd's
        // worktree BEFORE touching files or rows.
        let err = apply(&wrong_scope).await.expect_err("apply refuses");
        assert!(
            format!("{err:?}").contains("scope changed"),
            "apply surfaces the binding refusal: {err:?}"
        );
        let err = unapply(&wrong_scope, None)
            .await
            .expect_err("unapply refuses");
        assert!(
            format!("{err:?}").contains("scope changed"),
            "unapply surfaces the binding refusal: {err:?}"
        );
        // Nothing was written for either scope.
        assert!(
            LayerStore::materialized_paths(&WorktreeScope::Main)
                .await
                .expect("paths")
                .is_empty()
        );
        assert!(
            LayerStore::materialized_paths(&wrong_scope)
                .await
                .expect("paths")
                .is_empty()
        );
    }

    #[test]
    fn normalize_rejects_escapes() {
        assert_eq!(
            normalize_rel(std::path::Path::new("a/b.txt")).as_deref(),
            Some("a/b.txt")
        );
        assert!(normalize_rel(std::path::Path::new("../x")).is_none());
        assert!(normalize_rel(std::path::Path::new("/abs")).is_none());
        assert!(normalize_rel(std::path::Path::new("")).is_none());
    }
}
