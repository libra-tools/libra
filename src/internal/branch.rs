//! Branch storage layer.
//!
//! All branch state for a Libra repository lives in the SQLite `reference` table
//! (kind = `Branch`). This module is the only place that should mutate that table
//! for branch-shaped rows; callers go through `Branch::find_branch`,
//! `Branch::update_branch`, etc.
//!
//! The public API comes in two flavours:
//! - **Lossy wrappers** (e.g. `find_branch`, `list_branches`) — collapse storage
//!   errors into `None` / empty results, suitable for decoration paths where the
//!   alternative would be to abort a `git log` rendering.
//! - **`*_result` and `*_with_conn` variants** — return [`BranchStoreError`] so that
//!   transactional callers (`update_branch_with_conn` inside a `db.transaction(...)`)
//!   can roll back on failure. See the block comment above [`Branch`] for the
//!   `_with_conn` deadlock rule.
//!
//! Concurrency: SQLite serialises writers, so update/delete operations include
//! a bounded retry loop ([`SQLITE_BUSY_MAX_RETRIES`]) for transient `database is
//! locked` errors that show up under multi-task contention.

use std::{str::FromStr, time::Duration};

use git_internal::hash::ObjectHash;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DbErr, EntityTrait,
    QueryFilter,
};
use tokio::time::sleep;

use crate::internal::{db::get_db_conn_instance, model::reference};

/// The default trunk branch. Created on `libra init` and treated as a locked
/// branch (cannot be deleted while it is HEAD).
pub const DEFAULT_BRANCH: &str = "main";
/// Reserved branch used by the AI agent runtime to stage planner output (the
/// "intent" graph) before merging back to a working branch.
pub const INTENT_BRANCH: &str = "intent";
/// Reserved branch used by the external-agent capture subsystem (CEX-EntireIO)
/// to record session and checkpoint trailers on `refs/libra/traces`.
/// Treated as locked in the same sense as [`INTENT_BRANCH`]: the CLI must not
/// let `restore` / `reset` / `switch` / `checkout` move user work onto it.
pub const TRACES_BRANCH: &str = "traces";

/// Legacy name for [`TRACES_BRANCH`] (`refs/libra/agent-traces`). Repositories
/// created before the rename store the capture catalogue under this name; the
/// `2026062301_rename_agent_traces_branch` schema migration renames the row to
/// `traces`. The name is kept locked and AI-managed so any not-yet-migrated ref
/// (e.g. a repo that has not run `libra db upgrade`) keeps its destructive-op
/// protection.
pub const LEGACY_TRACES_BRANCH: &str = "agent-traces";

/// Return `true` for branches that the CLI refuses to delete, rename, or
/// otherwise route user-facing destructive ops at.
///
/// Functional scope: covers [`DEFAULT_BRANCH`], [`INTENT_BRANCH`],
/// [`TRACES_BRANCH`], and its legacy alias [`LEGACY_TRACES_BRANCH`]. The check
/// is purely syntactic — it does not consult the storage layer. Callers that
/// need a richer policy (e.g. branch protection rules) must layer additional
/// checks on top.
pub fn is_locked_branch(name: &str) -> bool {
    name == DEFAULT_BRANCH
        || name == INTENT_BRANCH
        || name == TRACES_BRANCH
        || name == LEGACY_TRACES_BRANCH
}

/// Return `true` for Libra-owned AI branches whose checked-out worktree must
/// not be used for ordinary user edits.
///
/// This deliberately excludes [`DEFAULT_BRANCH`]. `main` is locked for
/// branch-management operations such as delete/rename, but it remains the
/// normal user worktree branch.
pub fn is_ai_managed_branch(name: &str) -> bool {
    name == INTENT_BRANCH || name == TRACES_BRANCH || name == LEGACY_TRACES_BRANCH
}

/// Return `true` if the user-supplied revision string targets a locked
/// branch — including via revision suffixes (`traces~1`,
/// `intent^`, `traces@{0}`).
///
/// Boundary condition: this strips the first occurrence of any of `~`, `^`,
/// or `@` from `rev`, then re-runs [`is_locked_branch`] on the prefix. We
/// intentionally guard *all* revision shapes that resolve back to a locked
/// branch's history because the security goal is to prevent users from
/// rewinding their working tree to AI-managed commits, not just to refuse
/// the bare ref name.
pub fn is_locked_revision(rev: &str) -> bool {
    let head = rev.split(['~', '^', '@']).next().unwrap_or(rev);
    is_locked_branch(head)
}

/// In-memory branch view materialised from a [`reference::Model`] row.
///
/// `commit` is parsed into a typed [`ObjectHash`]; rows that are missing a
/// commit (just-created stubs) are filtered out before this struct is built.
#[derive(Debug)]
pub struct Branch {
    /// Short branch name, without `refs/heads/` or remote prefixes.
    pub name: String,
    /// The commit pointed to by the branch tip.
    pub commit: ObjectHash,
    /// `None` for local branches; `Some("origin")` etc. for remote-tracking
    /// branches. Forms a `(name, remote)` composite key.
    pub remote: Option<String>,
}

/// Storage-layer error surfaced by the `*_result` family of functions.
///
/// Boundary condition: all variants carry user-friendly context (`name`, `detail`)
/// so they can be displayed via `anyhow::Context` chains without leaking raw
/// sea-orm errors.
#[derive(Debug, thiserror::Error)]
pub enum BranchStoreError {
    /// Underlying SQLite query failed (connection, syntax, schema mismatch).
    #[error("failed to query branch storage: {0}")]
    Query(String),
    /// A row was found but could not be decoded into a [`Branch`] (e.g. the
    /// `commit` column held a non-hex string). Indicates database corruption
    /// or a schema/version mismatch.
    #[error("stored branch reference '{name}' is corrupt: {detail}")]
    Corrupt { name: String, detail: String },
    /// Lookup or delete targeted a branch that does not exist.
    #[error("branch '{0}' not found")]
    NotFound(String),
    /// An EXCLUSIVE create found the name already taken. Distinct from a
    /// generic write failure because the caller's contract differs: `stash
    /// branch` must refuse rather than move someone else's tip.
    #[error("branch '{0}' already exists")]
    AlreadyExists(String),
    /// Delete failed at the storage layer (FK violation, locked).
    #[error("failed to delete branch '{name}': {detail}")]
    Delete { name: String, detail: String },
    /// §C.4.4: the branch is checked out in ANOTHER worktree, so attaching a
    /// second HEAD to it (or deleting it) is refused.
    ///
    /// This is a distinct variant rather than a `Corrupt`/`Delete` with a
    /// descriptive message because the stable error code depends on it:
    /// §C.13 requires `LBR-CONFLICT-002` for a checkout collision, and every
    /// writer boundary derives its code from the variant. Folding it into a
    /// generic storage failure reported the repository as corrupt for what is
    /// an ordinary, recoverable conflict.
    ///
    /// The message deliberately uses [`CHECKED_OUT_ELSEWHERE_PHRASE`] verbatim,
    /// so a boundary that only ever sees the error as TEXT (after sea_orm or
    /// an intermediate wrapper has erased the type) still classifies it
    /// correctly.
    #[error(
        "branch '{name}' is checked out at worktree '{worktree}'; one branch is never checked \
         out in two worktrees"
    )]
    CheckedOutElsewhere { name: String, worktree: String },
}

/// The one phrase by which a checkout collision survives sea_orm's transaction
/// plumbing (plan-20260714 §C.13).
///
/// `Branch::update_branch_with_conn` is called from inside `with_reflog`
/// closures, whose error type sea_orm fixes at [`DbErr`] — a typed enum
/// cannot pass through it, and by the time the failure reaches a command it
/// has been wrapped twice more. So the collision is encoded ONCE, by
/// [`checked_out_elsewhere_db_err`], and recognised ONCE, by
/// [`checked_out_elsewhere_cli_error`]: no call site formats this text and no
/// call site matches on it. The phrase is deliberately the natural
/// user-facing wording rather than an internal tag, so nothing has to be
/// stripped before display, and it is the same wording the command-level
/// preflight already uses for the same condition.
pub const CHECKED_OUT_ELSEWHERE_PHRASE: &str = "is checked out at worktree";

/// Build the [`DbErr`] carrier for a checkout collision.
pub fn checked_out_elsewhere_db_err(branch_name: &str, worktree: &str) -> DbErr {
    DbErr::Custom(format!(
        "branch '{branch_name}' {CHECKED_OUT_ELSEWHERE_PHRASE} '{worktree}', so moving it here \
         would silently diverge that worktree's working tree from its own branch; run `libra \
         worktree list` to inspect, then switch one of them to another branch"
    ))
}

/// Boundary helper (§C.13): if `error` — at any wrapping depth — is a
/// checkout collision, produce the `LBR-CONFLICT-002` error it must be
/// reported as. Returns `None` for every other failure, so a caller keeps its
/// own mapping with `unwrap_or_else`.
///
/// A collision is not corruption and not an I/O fault: the repository is
/// intact, and the user has a next step. Reporting it with a storage code
/// sends them looking for damage that does not exist, and escalates in any
/// tooling keyed on the code.
pub fn checked_out_elsewhere_cli_error(
    error: &impl std::fmt::Display,
) -> Option<crate::utils::error::CliError> {
    let text = error.to_string();
    if !text.contains(CHECKED_OUT_ELSEWHERE_PHRASE) {
        return None;
    }
    Some(
        crate::utils::error::CliError::fatal(text)
            .with_stable_code(crate::utils::error::StableErrorCode::ConflictOperationBlocked)
            .with_hint("switch that worktree to another branch first, or run the command there"),
    )
}

/// Decode a raw `reference::Model` row into a [`Branch`].
///
/// Boundary conditions:
/// - Returns `Ok(None)` when the row has no `commit` (a transient "stub" row
///   that exists only to register a branch name). Callers treat this as
///   "branch exists but has no tip yet".
/// - Returns [`BranchStoreError::Corrupt`] when the `commit` column holds a
///   value that does not parse as an [`ObjectHash`], or when `name` is null.
/// - Otherwise returns `Ok(Some(branch))` with name/commit/remote populated.
fn branch_from_model(model: reference::Model) -> Result<Option<Branch>, BranchStoreError> {
    let Some(name) = model.name.clone() else {
        return Err(BranchStoreError::Corrupt {
            name: "<unknown>".to_string(),
            detail: "missing name field".to_string(),
        });
    };
    let Some(commit_str) = model.commit.as_ref() else {
        return Ok(None);
    };
    let commit = ObjectHash::from_str(commit_str).map_err(|e| BranchStoreError::Corrupt {
        name: name.clone(),
        detail: e.to_string(),
    })?;
    // A well-formed id of the WRONG algorithm parses cleanly and only fails
    // much later, inside object loading, as a panic. Fail closed at the read
    // boundary instead.
    let repo_kind = git_internal::hash::get_hash_kind();
    if commit.kind() != repo_kind {
        return Err(BranchStoreError::Corrupt {
            name: name.clone(),
            detail: format!(
                "commit hash is {:?} but this repository uses {:?}",
                commit.kind(),
                repo_kind
            ),
        });
    }
    Ok(Some(Branch {
        name,
        commit,
        remote: model.remote.clone(),
    }))
}

/// Fetch the raw `reference` row for `(branch_name, remote)` if it exists.
///
/// Boundary conditions:
/// - Filters explicitly on `kind = Branch` so tag/HEAD rows cannot be returned.
/// - When `remote` is `None`, filters on `remote IS NULL` (local branch).
/// - Returns `Ok(None)` if no row matches.
async fn query_reference_with_conn<C>(
    db: &C,
    branch_name: &str,
    remote: Option<&str>,
) -> Result<Option<reference::Model>, DbErr>
where
    C: ConnectionTrait,
{
    reference::Entity::find()
        .filter(reference::Column::Name.eq(branch_name))
        .filter(reference::Column::Kind.eq(reference::ConfigKind::Branch))
        .filter(match remote {
            Some(remote) => reference::Column::Remote.eq(remote),
            None => reference::Column::Remote.is_null(),
        })
        .one(db)
        .await
}

/// Maximum number of retry attempts when SQLite reports `database is locked`.
const SQLITE_BUSY_MAX_RETRIES: usize = 15;
/// Base back-off multiplier in milliseconds for the busy-retry loop.
/// The actual sleep grows linearly with attempt number.
const SQLITE_BUSY_RETRY_BASE_MS: u64 = 100;

/// Heuristic: detect a `DbErr` that corresponds to a transient SQLite lock.
///
/// SQLite surfaces `SQLITE_BUSY` and schema lock conditions through the message
/// string (sqlx wraps them in a generic `DbErr::Exec`). Pattern-matching on the
/// message keeps the retry logic provider-agnostic.
fn is_sqlite_busy(err: &DbErr) -> bool {
    let message = err.to_string();
    message.contains("database is locked") || message.contains("database schema is locked")
}

/*
 * =================================================================================
 * NOTE: Transaction Safety Pattern (`_with_conn`)
 * =================================================================================
 *
 * This module follows the `_with_conn` pattern for transaction safety.
 *
 * - Public functions (e.g., `find_branch`, `update_branch`) acquire a new database
 *   connection from the pool and are suitable for single, non-transactional operations.
 *
 * - `*_with_conn` variants (e.g., `find_branch_with_conn`, `update_branch_with_conn`)
 *   accept an existing connection or transaction handle (`&C where C: ConnectionTrait`).
 *
 * **WARNING**: To use these functions within a database transaction (e.g., inside
 * a `db.transaction(|txn| { ... })` block), you MUST call the `*_with_conn`
 * variant, passing the transaction handle `txn`. Calling a public version from
 * inside a transaction will try to acquire a second connection from the pool,
 * leading to a deadlock.
 *
 * Correct Usage (in a transaction): `Branch::update_branch_with_conn(txn, ...).await;`
 * Incorrect Usage (in a transaction): `Branch::update_branch(...).await;` // DEADLOCK!
 */
impl Branch {
    /// List every branch row scoped to a given remote, returning a
    /// [`BranchStoreError`] on storage or decode failures.
    ///
    /// Boundary conditions:
    /// - `remote = None` lists local branches; `remote = Some("origin")`
    ///   lists remote-tracking branches for that remote.
    /// - Rows that decode to `Ok(None)` (no commit yet) are skipped silently.
    /// - On the first decode error, the function returns the error and the
    ///   remaining rows are not inspected.
    pub async fn list_branches_result_with_conn<C>(
        db: &C,
        remote: Option<&str>,
    ) -> Result<Vec<Self>, BranchStoreError>
    where
        C: ConnectionTrait,
    {
        let branches = reference::Entity::find()
            .filter(reference::Column::Kind.eq(reference::ConfigKind::Branch))
            .filter(match remote {
                Some(remote) => reference::Column::Remote.eq(remote),
                None => reference::Column::Remote.is_null(),
            })
            .all(db)
            .await
            .map_err(|err| BranchStoreError::Query(err.to_string()))?;

        let mut resolved = Vec::new();
        for branch in branches {
            if let Some(branch) = branch_from_model(branch)? {
                resolved.push(branch);
            }
        }
        Ok(resolved)
    }

    /// Best-effort branch listing that skips corrupt rows instead of failing
    /// the entire query. Useful for decoration metadata (log/show refs) where
    /// partial results are more valuable than an empty set.
    pub async fn list_branches_best_effort(remote: Option<&str>) -> Vec<Self> {
        let db_conn = get_db_conn_instance().await;
        let branches = match reference::Entity::find()
            .filter(reference::Column::Kind.eq(reference::ConfigKind::Branch))
            .filter(match remote {
                Some(r) => reference::Column::Remote.eq(r),
                None => reference::Column::Remote.is_null(),
            })
            .all(&db_conn)
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "failed to query branches for decoration"
                );
                return Vec::new();
            }
        };

        let mut resolved = Vec::new();
        for branch in branches {
            match branch_from_model(branch) {
                Ok(Some(branch)) => resolved.push(branch),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "skipping corrupt branch row in decoration"
                    );
                }
            }
        }
        resolved
    }

    /// Result-returning variant of `list_branches_result_with_conn` that
    /// acquires its own connection from the pool. Use the `_with_conn` form
    /// inside transactions.
    pub async fn list_branches_result(remote: Option<&str>) -> Result<Vec<Self>, BranchStoreError> {
        let db_conn = get_db_conn_instance().await;
        Self::list_branches_result_with_conn(&db_conn, remote).await
    }

    /// Result-returning existence check.
    ///
    /// Returns `Ok(true)` when a row matching `(branch_name, remote)` exists,
    /// `Ok(false)` when none does, and a [`BranchStoreError::Query`] on storage
    /// failures. Unlike `Branch::exists_with_conn`, a corrupt row still counts
    /// as existing (it is not decoded here).
    pub async fn exists_result_with_conn<C>(
        db: &C,
        branch_name: &str,
        remote: Option<&str>,
    ) -> Result<bool, BranchStoreError>
    where
        C: ConnectionTrait,
    {
        query_reference_with_conn(db, branch_name, remote)
            .await
            .map(|branch| branch.is_some())
            .map_err(|err| BranchStoreError::Query(err.to_string()))
    }

    /// Pool-acquiring counterpart of [`Branch::exists_result_with_conn`].
    pub async fn exists_result(
        branch_name: &str,
        remote: Option<&str>,
    ) -> Result<bool, BranchStoreError> {
        let db_conn = get_db_conn_instance().await;
        Self::exists_result_with_conn(&db_conn, branch_name, remote).await
    }

    /// Result-returning branch lookup keyed by `(name, remote)`.
    ///
    /// Boundary conditions:
    /// - Returns `Ok(None)` for missing rows or rows where `commit IS NULL`.
    /// - Returns [`BranchStoreError::Corrupt`] if the row exists but its
    ///   `commit` cannot be parsed into an [`ObjectHash`].
    pub async fn find_branch_result_with_conn<C>(
        db: &C,
        branch_name: &str,
        remote: Option<&str>,
    ) -> Result<Option<Self>, BranchStoreError>
    where
        C: ConnectionTrait,
    {
        let branch = query_reference_with_conn(db, branch_name, remote)
            .await
            .map_err(|err| BranchStoreError::Query(err.to_string()))?;
        match branch {
            Some(branch) => branch_from_model(branch),
            None => Ok(None),
        }
    }

    /// Pool-acquiring counterpart of [`Branch::find_branch_result_with_conn`].
    pub async fn find_branch_result(
        branch_name: &str,
        remote: Option<&str>,
    ) -> Result<Option<Self>, BranchStoreError> {
        let db_conn = get_db_conn_instance().await;
        Self::find_branch_result_with_conn(&db_conn, branch_name, remote).await
    }

    /// Walk every `(remote, branch)` split of an ambiguous slash-delimited name
    /// and collect every existing match.
    ///
    /// Functional scope:
    /// - For a query like `"a/b/c"`, this checks: local `"a/b/c"`, then
    ///   `(remote = "a", branch = "b/c")`, then `(remote = "a/b", branch = "c")`.
    /// - The result preserves discovery order (most-specific local match first).
    ///
    /// Boundary conditions:
    /// - The empty input yields an empty `Vec`.
    /// - Returns `Ok(vec![])` when no split matches anything.
    /// - The internal `strip_prefix('/')` should always succeed because we
    ///   discovered the index via `find('/')`; if that invariant breaks the
    ///   error surfaces as [`BranchStoreError::Corrupt`].
    ///
    /// See: `tests::test_search_branch` for the multi-segment scenario.
    pub async fn search_branch_result_with_conn<C>(
        db: &C,
        branch_name: &str,
    ) -> Result<Vec<Self>, BranchStoreError>
    where
        C: ConnectionTrait,
    {
        let mut branch_name_str = branch_name.to_string();
        let mut remote = String::new();

        let mut branches = vec![];
        // First attempt: treat the entire input as a local branch name.
        if let Some(branch) = Self::find_branch_result_with_conn(db, &branch_name_str, None).await?
        {
            branches.push(branch)
        }

        // Iteratively peel off one path segment at a time and treat the prefix
        // as a remote name, the suffix as the branch under that remote.
        while let Some(index) = branch_name_str.find('/') {
            let (remote_segment, remainder) = branch_name_str.split_at(index);
            let remainder =
                remainder
                    .strip_prefix('/')
                    .ok_or_else(|| BranchStoreError::Corrupt {
                        name: branch_name.to_string(),
                        detail: format!("failed to split branch search path '{branch_name_str}'"),
                    })?;
            // Accumulate the consumed segment into the running `remote` path.
            if !remote.is_empty() {
                remote += "/";
            }
            remote += remote_segment;
            branch_name_str = remainder.to_string();
            if let Some(branch) =
                Self::find_branch_result_with_conn(db, &branch_name_str, Some(&remote)).await?
            {
                branches.push(branch);
            }
        }
        Ok(branches)
    }

    /// Pool-acquiring counterpart of [`Branch::search_branch_result_with_conn`].
    pub async fn search_branch_result(branch_name: &str) -> Result<Vec<Self>, BranchStoreError> {
        let db_conn = get_db_conn_instance().await;
        Self::search_branch_result_with_conn(&db_conn, branch_name).await
    }

    /// Upsert a branch tip with retry-on-busy semantics.
    ///
    /// Functional scope:
    /// - If a row exists for `(branch_name, remote)`, updates its `commit`.
    /// - Otherwise inserts a new `Branch`-kind row.
    /// - Each storage call is wrapped in a bounded retry loop that backs off
    ///   linearly when SQLite reports `SQLITE_BUSY` ([`SQLITE_BUSY_MAX_RETRIES`]
    ///   attempts at [`SQLITE_BUSY_RETRY_BASE_MS`] base delay).
    ///
    /// Boundary conditions:
    /// - `commit_hash` is stored verbatim; this function does not validate it
    ///   as a real [`ObjectHash`]. Garbage in, garbage out.
    /// - Returns the underlying `DbErr` if the retry loop is exhausted or a
    ///   non-busy error is returned.
    /// - The trailing `unreachable!` panic guards against a logic error in
    ///   the loop bounds and should never fire in production.
    pub async fn update_branch_with_conn<C>(
        db: &C,
        branch_name: &str,
        commit_hash: &str,
        remote: Option<&str>,
    ) -> Result<(), DbErr>
    where
        C: ConnectionTrait,
    {
        // plan-20260714 §C.4.4: THE cross-worktree guard, on the writer's own
        // connection, immediately before the write.
        //
        // It lives here rather than in each caller because "each caller"
        // turned out to mean `merge`, `rebase`, `cherry-pick`, `revert`,
        // `am`, `reset`, `commit`, `pull`, `fetch`, `fast-import`,
        // `update-ref`, `branch` and `op restore` — and six of them had no
        // guard at all while the rest checked BEFORE opening their
        // transaction, leaving a window for another worktree to attach in
        // between. Every local branch ref write in the process funnels
        // through this function, so guarding it closes both holes at once
        // and makes a future writer safe by default.
        //
        // Remote-tracking refs are exempt: nothing checks them out, and
        // `branch_checked_out_elsewhere_result_with_conn` filters
        // `remote IS NULL` anyway.
        if remote.is_none()
            && let Some(other) =
                crate::internal::head::Head::branch_checked_out_elsewhere_result_with_conn(
                    db,
                    branch_name,
                )
                .await?
        {
            return Err(checked_out_elsewhere_db_err(branch_name, &other));
        }
        for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
            let branch = match query_reference_with_conn(db, branch_name, remote).await {
                Ok(branch) => branch,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => return Err(err),
            };

            let write_result = match branch {
                Some(branch) => {
                    let mut branch: reference::ActiveModel = branch.into();
                    branch.commit = Set(Some(commit_hash.to_owned()));
                    branch.update(db).await.map(|_| ())
                }
                None => reference::ActiveModel {
                    name: Set(Some(branch_name.to_owned())),
                    kind: Set(reference::ConfigKind::Branch),
                    commit: Set(Some(commit_hash.to_owned())),
                    remote: Set(remote.map(|s| s.to_owned())),
                    ..Default::default()
                }
                .insert(db)
                .await
                .map(|_| ()),
            };

            match write_result {
                Ok(()) => return Ok(()),
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                }
                Err(err) => return Err(err),
            }
        }
        unreachable!("sqlite retry loop must return")
    }

    /// Create a branch that must NOT already exist (W2 §C.4.3).
    ///
    /// [`Branch::update_branch`] is an UPSERT: it moves an existing tip. A
    /// caller like `stash branch`, whose contract is "create this branch or
    /// refuse", cannot use it — checking existence first leaves a window in
    /// which another worktree creates the name and this call silently
    /// overwrites its tip. The existence check and the insert therefore run in
    /// ONE write-locked transaction, so a concurrent creator either loses the
    /// lock (and this call refuses it) or wins it (and this call refuses).
    pub async fn create_branch_exclusive(
        branch_name: &str,
        commit_hash: &str,
        remote: Option<&str>,
        provenance_key: Option<&str>,
    ) -> Result<(), BranchStoreError> {
        let db_conn = get_db_conn_instance().await;
        let txn = crate::internal::db::begin_write_transaction(&db_conn)
            .await
            .map_err(|error| BranchStoreError::Query(error.to_string()))?;
        let existing = query_reference_with_conn(&txn, branch_name, remote)
            .await
            .map_err(|error| BranchStoreError::Query(error.to_string()));
        let existing = match existing {
            Ok(existing) => existing,
            Err(error) => {
                let _ = txn.rollback().await;
                return Err(error);
            }
        };
        if existing.is_some() {
            let _ = txn.rollback().await;
            return Err(BranchStoreError::AlreadyExists(branch_name.to_string()));
        }
        let insert = reference::ActiveModel {
            name: Set(Some(branch_name.to_owned())),
            kind: Set(reference::ConfigKind::Branch),
            commit: Set(Some(commit_hash.to_owned())),
            remote: Set(remote.map(|value| value.to_owned())),
            ..Default::default()
        }
        .insert(&txn)
        .await;
        let inserted = match insert {
            Ok(inserted) => inserted,
            Err(error) => {
                let _ = txn.rollback().await;
                return Err(BranchStoreError::Query(error.to_string()));
            }
        };
        // Provenance, committed ATOMICALLY with the creation (W2 r7 #1): the
        // caller's crash-recovery must know whether THIS create ever
        // committed, and a file written before or after the transaction
        // cannot say that. The row also records the created reference's ID,
        // so a later rollback deletes exactly the row this create made —
        // never a same-name same-tip branch the user recreated (r7 #2).
        if let Some(provenance_key) = provenance_key {
            use sea_orm::{ConnectionTrait, Statement};
            let recorded = txn
                .execute_raw(Statement::from_sql_and_values(
                    sea_orm::DatabaseBackend::Sqlite,
                    "INSERT INTO metadata_kv \
                     (scope, target, key, value, value_type, created_at, updated_at) \
                     VALUES ('stash_branch_journal', ?, 'reference_id', ?, 'text', \
                     datetime('now'), datetime('now'))",
                    [provenance_key.into(), inserted.id.to_string().into()],
                ))
                .await;
            if let Err(error) = recorded {
                let _ = txn.rollback().await;
                return Err(BranchStoreError::Query(error.to_string()));
            }
        }
        txn.commit()
            .await
            .map_err(|error| BranchStoreError::Query(error.to_string()))
    }

    /// The fate a journaled `stash branch` creation is concluded to, by
    /// PROVENANCE rather than by name+tip (W2 r7 #1/#2).
    ///
    /// One write-locked transaction reads the provenance row and, when it
    /// exists, deletes the branch BY ITS RECORDED ROW ID, conditional on the
    /// tip still being the journaled base — then removes the provenance row.
    /// A branch the user deleted and recreated at the same base has a
    /// DIFFERENT row id, so it is kept: name and tip are reusable, a rowid is
    /// not.
    pub async fn conclude_journaled_branch(
        provenance_key: &str,
        base_tip: &str,
    ) -> Result<JournaledBranchFate, BranchStoreError> {
        use sea_orm::{ConnectionTrait, Statement};
        let db_conn = get_db_conn_instance().await;
        let txn = crate::internal::db::begin_write_transaction(&db_conn)
            .await
            .map_err(|error| BranchStoreError::Query(error.to_string()))?;
        let row = txn
            .query_one_raw(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Sqlite,
                "SELECT value FROM metadata_kv WHERE scope = 'stash_branch_journal' \
                 AND target = ? AND key = 'reference_id'",
                [provenance_key.into()],
            ))
            .await
            .map_err(|error| BranchStoreError::Query(error.to_string()))?;
        let Some(row) = row else {
            // The create never COMMITTED: there is nothing of ours to remove.
            let _ = txn.rollback().await;
            return Ok(JournaledBranchFate::NeverCreated);
        };
        let reference_id: String = row
            .try_get_by_index(0)
            .map_err(|error| BranchStoreError::Query(error.to_string()))?;
        let deleted = txn
            .execute_raw(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Sqlite,
                "DELETE FROM reference WHERE id = ? AND kind = 'Branch' AND `commit` = ?",
                [reference_id.clone().into(), base_tip.into()],
            ))
            .await
            .map_err(|error| BranchStoreError::Query(error.to_string()))?;
        let fate = if deleted.rows_affected() > 0 {
            JournaledBranchFate::Deleted
        } else {
            // Row gone (user deleted it) or its tip moved (user committed):
            // either way it is not ours to touch any more.
            JournaledBranchFate::KeptOrGone
        };
        let cleared = txn
            .execute_raw(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Sqlite,
                "DELETE FROM metadata_kv WHERE scope = 'stash_branch_journal' AND target = ?",
                [provenance_key.into()],
            ))
            .await;
        if let Err(error) = cleared {
            let _ = txn.rollback().await;
            return Err(BranchStoreError::Query(error.to_string()));
        }
        txn.commit()
            .await
            .map_err(|error| BranchStoreError::Query(error.to_string()))?;
        Ok(fate)
    }

    /// Whether a journaled creation's provenance row EXISTS — the read-only
    /// probe recovery runs before touching anything (W2 r8 #1): no row means
    /// the create never committed, and recovery must then leave HEAD alone
    /// too (the branch the user might be standing on is not ours).
    pub async fn journaled_provenance_exists(
        provenance_key: &str,
    ) -> Result<bool, BranchStoreError> {
        use sea_orm::{ConnectionTrait, Statement};
        let db_conn = get_db_conn_instance().await;
        let row = db_conn
            .query_one_raw(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Sqlite,
                "SELECT 1 FROM metadata_kv WHERE scope = 'stash_branch_journal' \
                 AND target = ? AND key = 'reference_id'",
                [provenance_key.into()],
            ))
            .await
            .map_err(|error| BranchStoreError::Query(error.to_string()))?;
        Ok(row.is_some())
    }

    /// Remove a journaled creation's provenance row WITHOUT touching the
    /// branch — the success path's conclusion (the branch is the user's).
    pub async fn clear_journaled_branch_provenance(
        provenance_key: &str,
    ) -> Result<(), BranchStoreError> {
        use sea_orm::{ConnectionTrait, Statement};
        let db_conn = get_db_conn_instance().await;
        db_conn
            .execute_raw(Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Sqlite,
                "DELETE FROM metadata_kv WHERE scope = 'stash_branch_journal' AND target = ?",
                [provenance_key.into()],
            ))
            .await
            .map_err(|error| BranchStoreError::Query(error.to_string()))?;
        Ok(())
    }

    /// Pool-acquiring counterpart of [`Branch::update_branch_with_conn`].
    /// Must NOT be called from within an active transaction (would deadlock —
    /// see the block comment near the top of `impl Branch`).
    pub async fn update_branch(
        branch_name: &str,
        commit_hash: &str,
        remote: Option<&str>,
    ) -> Result<(), DbErr> {
        let db_conn = get_db_conn_instance().await;
        // §C.4.4: run the checked-out probe and the write in ONE transaction.
        // On a pool connection they are two implicit transactions, so another
        // worktree can attach the branch in between — the probe says "free",
        // the write moves the tip, and that worktree is left pointing at the
        // old one. SQLite's rollback journal makes the read half of this
        // transaction block a concurrent attach until we commit, so the pair
        // is genuinely atomic against it.
        // The update READS the existing row before writing it, so the write
        // lock is taken up front (`db::begin_write_transaction`).
        let txn = crate::internal::db::begin_write_transaction(&db_conn).await?;
        Self::update_branch_with_conn(&txn, branch_name, commit_hash, remote).await?;
        txn.commit().await
    }

    /// Result-returning branch delete.
    ///
    /// Boundary conditions:
    /// - Returns [`BranchStoreError::NotFound`] when no row matches.
    /// - Returns [`BranchStoreError::Query`] if the lookup itself fails, or
    ///   [`BranchStoreError::Delete`] if the row is found but deletion fails.
    /// - Does not check `is_locked_branch` — that policy lives in the CLI layer.
    pub async fn delete_branch_result_with_conn<C>(
        db: &C,
        branch_name: &str,
        remote: Option<&str>,
    ) -> Result<(), BranchStoreError>
    where
        C: ConnectionTrait,
    {
        // §C.4.4: deleting a branch another worktree has checked out leaves
        // that worktree's HEAD dangling. `switch -C` reached here directly —
        // deleting first and recreating after — so the guard on the UPDATE
        // path alone let the delete through and then failed the recreate,
        // which is the worst of both.
        if remote.is_none()
            && let Some(other) =
                crate::internal::head::Head::branch_checked_out_elsewhere_result_with_conn(
                    db,
                    branch_name,
                )
                .await
                .map_err(|err| BranchStoreError::Query(err.to_string()))?
        {
            return Err(BranchStoreError::CheckedOutElsewhere {
                name: branch_name.to_string(),
                worktree: other,
            });
        }
        let branch = query_reference_with_conn(db, branch_name, remote)
            .await
            .map_err(|err| BranchStoreError::Query(err.to_string()))?;
        let Some(branch) = branch else {
            return Err(BranchStoreError::NotFound(branch_name.to_string()));
        };
        let branch: reference::ActiveModel = branch.into();
        branch
            .delete(db)
            .await
            .map(|_| ())
            .map_err(|err| BranchStoreError::Delete {
                name: branch_name.to_string(),
                detail: err.to_string(),
            })?;
        // Cascade branch metadata (lore.md §1.5) for LOCAL branches only —
        // pruning a remote-tracking ref (remote=Some) must never wipe the
        // same-named local branch's metadata. On a pool connection the ref
        // delete and this cascade are two implicit transactions; a crash
        // between them can orphan rows, which are inert (all reads are
        // target-keyed) and swept up if the branch name is ever reused.
        if remote.is_none() {
            crate::internal::metadata::MetadataKv::delete_all_for_target_with_conn(
                db,
                crate::internal::metadata::MetadataScope::Branch,
                branch_name,
            )
            .await
            .map_err(|err| BranchStoreError::Delete {
                name: branch_name.to_string(),
                detail: format!("branch deleted but metadata cascade failed: {err}"),
            })?;
        }
        Ok(())
    }

    /// Pool-acquiring counterpart of [`Branch::delete_branch_result_with_conn`].
    ///
    /// Must NOT be called from within an active transaction (would deadlock —
    /// see the block comment near the top of `impl Branch`).
    pub async fn delete_branch_result(
        branch_name: &str,
        remote: Option<&str>,
    ) -> Result<(), BranchStoreError> {
        let db_conn = get_db_conn_instance().await;
        // §C.4.4: probe, delete and metadata cascade in ONE transaction. On a
        // pool connection they are three implicit transactions, so a worktree
        // that attaches to this branch after the probe — `switch -C` reaches
        // this entry point directly — passes its own probe (the branch is
        // still there), and then loses the branch under it. The same
        // transaction also makes the metadata cascade atomic with the ref
        // delete, which the `_with_conn` form could only document as a gap.
        // Failing to ACQUIRE the write lock (or to commit) is a failure OF
        // THE DELETE — e.g. a read-only repository database must surface as
        // "failed to prune/delete branch X" (IoWriteFailed), not as a
        // query/list failure: `remote prune` maps `Query` to its LIST error
        // (LBR-IO-001) and `Delete` to the prune error (LBR-IO-002), and the
        // probe-first transaction had silently moved the read-only failure
        // from the DELETE statement to the probe (W5-09 gate regression).
        let txn = crate::internal::db::begin_write_transaction(&db_conn)
            .await
            .map_err(|e| BranchStoreError::Delete {
                name: branch_name.to_string(),
                detail: e.to_string(),
            })?;
        Self::delete_branch_result_with_conn(&txn, branch_name, remote).await?;
        txn.commit().await.map_err(|e| BranchStoreError::Delete {
            name: branch_name.to_string(),
            detail: e.to_string(),
        })
    }

    /// ATOMIC tip-conditional delete (W2 §C.4.3 `stash branch` rollback):
    /// the tip check and the delete run in ONE transaction, so a branch a
    /// concurrent writer moved between "looks unchanged" and "delete" can
    /// never be destroyed — the whole check-then-delete either sees the
    /// moved tip (and keeps the branch) or deletes the unmoved one.
    pub async fn delete_branch_if_tip_result(
        branch_name: &str,
        expected_tip: &git_internal::hash::ObjectHash,
    ) -> Result<ConditionalDeleteOutcome, BranchStoreError> {
        let db_conn = get_db_conn_instance().await;
        // Compare-and-swap on the tip: reads the row, then deletes it.
        let txn = crate::internal::db::begin_write_transaction(&db_conn)
            .await
            .map_err(|e| BranchStoreError::Query(e.to_string()))?;
        let found = Self::find_branch_result_with_conn(&txn, branch_name, None).await?;
        let outcome = match found {
            None => ConditionalDeleteOutcome::NotFound,
            Some(branch) if branch.commit != *expected_tip => ConditionalDeleteOutcome::TipMoved,
            Some(_) => {
                Self::delete_branch_result_with_conn(&txn, branch_name, None).await?;
                ConditionalDeleteOutcome::Deleted
            }
        };
        txn.commit()
            .await
            .map_err(|e| BranchStoreError::Query(e.to_string()))?;
        Ok(outcome)
    }
}

/// What [`Branch::conclude_journaled_branch`] found (W2 r7 #1/#2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournaledBranchFate {
    /// No provenance row: the journaled create never committed.
    NeverCreated,
    /// The provenance-recorded row still sat at the journaled base — deleted.
    Deleted,
    /// The recorded row is gone or its tip moved: the user's now, untouched.
    KeptOrGone,
}

/// Outcome of [`Branch::delete_branch_if_tip_result`].
#[derive(Debug, PartialEq, Eq)]
pub enum ConditionalDeleteOutcome {
    /// The branch still pointed at the expected tip and was deleted.
    Deleted,
    /// The branch tip moved concurrently — left in place.
    TipMoved,
    /// No branch of that name exists (already gone).
    NotFound,
}

#[cfg(test)]
mod tests {
    use git_internal::hash::{HashKind, get_hash_kind, set_hash_kind_for_test};
    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::utils::test;

    /// A ref mutation QUEUES behind a concurrent writer instead of failing.
    ///
    /// Both `Branch::update_branch` and `Head::update_result` read the current
    /// row before replacing it. A deferred transaction that upgrades read→write
    /// gets `SQLITE_BUSY` immediately — SQLite does not run the busy handler
    /// for that case — so before `db::begin_write_transaction` a second writer
    /// anywhere in the repository made these fail outright rather than wait.
    #[tokio::test]
    #[serial]
    async fn a_ref_mutation_waits_for_a_concurrent_writer() {
        use std::time::Duration;

        let temp_path = tempdir().unwrap();
        test::setup_with_new_libra_in(temp_path.path()).await;
        let _guard = test::ChangeDirGuard::new(temp_path.path());

        // A SECOND connection to the same file, as another process would have.
        let db_path = crate::utils::path::database();
        let holder = crate::internal::db::establish_connection(db_path.to_str().expect("utf-8"))
            .await
            .expect("second connection");
        let held = crate::internal::db::begin_write_transaction(&holder)
            .await
            .expect("hold the write lock");
        held.execute_unprepared("INSERT INTO `config_kv` (`key`, `value`) VALUES ('holder', '1')")
            .await
            .expect("write under the held lock");

        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            held.commit().await.expect("release the lock");
        });

        let tip = ObjectHash::zero_str(get_hash_kind()).to_string();
        let started = std::time::Instant::now();
        Branch::update_branch("queued", &tip, None)
            .await
            .expect("the branch write waits for the other writer instead of failing");
        crate::internal::head::Head::update_result(
            crate::internal::head::Head::Branch("queued".to_string()),
            None,
        )
        .await
        .expect("and so does the HEAD write");
        assert!(
            started.elapsed() >= Duration::from_millis(250),
            "it WAITED rather than racing past the holder"
        );
        release.await.expect("holder task");

        assert!(
            Branch::find_branch_result("queued", None)
                .await
                .expect("read back the branch")
                .is_some(),
            "and the write landed"
        );
    }

    /// W2 §C.4.3: the tip-conditional delete is ATOMIC — a branch whose tip
    /// moved is kept (TipMoved), an unmoved one is deleted, a missing one
    /// reports NotFound; no query→delete window exists (single txn).
    #[tokio::test]
    #[serial]
    async fn conditional_delete_only_removes_the_unmoved_tip() {
        let temp_path = tempdir().unwrap();
        test::setup_with_new_libra_in(temp_path.path()).await;
        let _guard = test::ChangeDirGuard::new(temp_path.path());

        let base = ObjectHash::zero_str(get_hash_kind()).to_string();
        Branch::update_branch("rb", &base, None)
            .await
            .expect("create branch");
        let base_hash = ObjectHash::from_str(&base).unwrap();
        let moved_hash = {
            // Any different valid hash.
            let mut hex = base.clone();
            hex.replace_range(0..2, "aa");
            ObjectHash::from_str(&hex).unwrap()
        };

        // Wrong expected tip → kept.
        let outcome = Branch::delete_branch_if_tip_result("rb", &moved_hash)
            .await
            .expect("conditional delete");
        assert_eq!(outcome, ConditionalDeleteOutcome::TipMoved);
        assert!(
            Branch::find_branch_result("rb", None)
                .await
                .expect("find")
                .is_some(),
            "moved-tip branch survives"
        );

        // Matching tip → deleted; second call reports NotFound.
        let outcome = Branch::delete_branch_if_tip_result("rb", &base_hash)
            .await
            .expect("conditional delete");
        assert_eq!(outcome, ConditionalDeleteOutcome::Deleted);
        let outcome = Branch::delete_branch_if_tip_result("rb", &base_hash)
            .await
            .expect("conditional delete");
        assert_eq!(outcome, ConditionalDeleteOutcome::NotFound);
    }

    /// Scenario: a branch name like `"upstream/origin/master"` is ambiguous —
    /// it could be a local name, or any of three `(remote, branch)` splits.
    /// This test seeds three matching rows and one decoy that shares a prefix
    /// but a non-matching branch suffix, then asserts that
    /// [`Branch::search_branch_result`] returns exactly the three real matches.
    #[tokio::test]
    #[serial]
    async fn test_search_branch() {
        let _guard = set_hash_kind_for_test(HashKind::Sha256);
        let temp_path = tempdir().unwrap();
        test::setup_with_new_libra_in(temp_path.path()).await;
        let _guard = test::ChangeDirGuard::new(temp_path.path());

        let commit_hash = ObjectHash::zero_str(get_hash_kind()).to_string();
        Branch::update_branch("upstream/origin/master", &commit_hash, None)
            .await
            .unwrap(); // should match
        Branch::update_branch("origin/master", &commit_hash, Some("upstream"))
            .await
            .unwrap(); // should match
        Branch::update_branch("master", &commit_hash, Some("upstream/origin"))
            .await
            .unwrap(); // should match
        Branch::update_branch("feature", &commit_hash, Some("upstream/origin/master"))
            .await
            .unwrap(); // should not match

        let branches = Branch::search_branch_result("upstream/origin/master")
            .await
            .expect("search_branch_result should not fail on a freshly-seeded test DB");
        assert_eq!(branches.len(), 3);
    }

    /// Pin the `Display` format contract for [`BranchStoreError`].
    /// The variants are produced via `thiserror` `#[error(...)]`
    /// attributes and feed `LBR-REPO-002` / `LBR-REPO-003` mappings
    /// at the CLI boundary (see `src/utils/error.rs`). The exact
    /// strings are also used by the `--json` envelope so they form
    /// part of the public output contract.
    #[test]
    fn branch_store_error_display_pins_each_variant() {
        let query = BranchStoreError::Query("connection lost".to_string());
        assert_eq!(
            query.to_string(),
            "failed to query branch storage: connection lost",
        );

        let corrupt = BranchStoreError::Corrupt {
            name: "refs/heads/main".to_string(),
            detail: "invalid object id".to_string(),
        };
        assert_eq!(
            corrupt.to_string(),
            "stored branch reference 'refs/heads/main' is corrupt: invalid object id",
        );

        let not_found = BranchStoreError::NotFound("topic/x".to_string());
        assert_eq!(not_found.to_string(), "branch 'topic/x' not found");

        let delete = BranchStoreError::Delete {
            name: "feature/y".to_string(),
            detail: "FK violation".to_string(),
        };
        assert_eq!(
            delete.to_string(),
            "failed to delete branch 'feature/y': FK violation",
        );
    }

    /// CEX-EntireIO: regression — `is_locked_branch` must include the capture
    /// ref `traces` (and its legacy `agent-traces` alias) so `restore`,
    /// `reset`, `switch`, and `checkout` reject it as a target.
    #[test]
    fn is_locked_branch_covers_traces() {
        assert!(is_locked_branch(DEFAULT_BRANCH));
        assert!(is_locked_branch(INTENT_BRANCH));
        assert!(is_locked_branch(TRACES_BRANCH));
        assert!(is_locked_branch(LEGACY_TRACES_BRANCH));
        assert!(!is_locked_branch("traces-feature"));
        assert!(!is_locked_branch("agent-traces-feature"));
        assert!(!is_locked_branch("not-locked"));
        assert!(!is_locked_branch(""));
    }

    #[test]
    fn is_ai_managed_branch_excludes_main() {
        assert!(!is_ai_managed_branch(DEFAULT_BRANCH));
        assert!(is_ai_managed_branch(INTENT_BRANCH));
        assert!(is_ai_managed_branch(TRACES_BRANCH));
        assert!(is_ai_managed_branch(LEGACY_TRACES_BRANCH));
        assert!(!is_ai_managed_branch("traces-feature"));
        assert!(!is_ai_managed_branch("agent-traces-feature"));
        assert!(!is_ai_managed_branch(""));
    }

    /// CEX-EntireIO: `is_locked_revision` must strip `~` / `^` / `@`
    /// suffixes before lockup so a bypass like `traces~1` is still
    /// rejected. Codex review P1 #1, plus round-2 expansions for
    /// double suffixes, upstream shortcuts, and reflog selectors.
    #[test]
    fn is_locked_revision_strips_suffixes() {
        // Bare locked names (canonical + legacy alias).
        assert!(is_locked_revision("traces"));
        assert!(is_locked_revision("agent-traces"));
        assert!(is_locked_revision("intent"));
        assert!(is_locked_revision(DEFAULT_BRANCH));

        // Single-suffix variants.
        assert!(is_locked_revision("traces~1"));
        assert!(is_locked_revision("traces^"));
        assert!(is_locked_revision("traces^^"));
        assert!(is_locked_revision("agent-traces~1"));
        assert!(is_locked_revision("intent~1"));
        assert!(is_locked_revision("intent^2"));

        // Double-suffix and reordered combinations (round-2 BLOCK #1).
        assert!(is_locked_revision("traces~1^2"));
        assert!(is_locked_revision("traces^1~3"));
        assert!(is_locked_revision("intent~10^"));

        // Reflog / upstream / negative selectors.
        assert!(is_locked_revision("traces@{0}"));
        assert!(is_locked_revision("traces@{upstream}"));
        assert!(is_locked_revision("traces@{push}"));
        assert!(is_locked_revision("traces@{-1}"));
        assert!(is_locked_revision("intent@{1.day.ago}"));

        // Negative controls — nothing to do with locked branches.
        assert!(!is_locked_revision("HEAD~1"));
        assert!(!is_locked_revision("HEAD@{0}"));
        assert!(!is_locked_revision("feature/x~1"));
        assert!(!is_locked_revision("traces-feature"));
        assert!(!is_locked_revision("agent-traces-feature"));
        assert!(!is_locked_revision("agent-traces-feature^"));
        assert!(!is_locked_revision("not-locked@{0}"));
        assert!(!is_locked_revision(""));
    }
}
