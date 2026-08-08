//! HEAD management backed by the database, supporting local and remote heads, detached states, and transaction-safe query/update helpers.

use std::{str::FromStr, time::Duration};

use git_internal::hash::ObjectHash;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DbErr, EntityTrait,
    QueryFilter,
};
use tokio::time::sleep;

use crate::internal::{
    branch::{Branch, BranchStoreError},
    db::get_db_conn_instance,
    model::reference,
};

#[derive(Debug, Clone)]
pub enum Head {
    Detached(ObjectHash),
    Branch(String),
}

/*
 * =================================================================================
 * NOTE: Transaction Safety Pattern (`_with_conn`)
 * =================================================================================
 *
 * This module follows the `_with_conn` pattern for transaction safety.
 *
 * - Public functions (e.g., `get`, `update`) acquire a new database
 *   connection from the pool and are suitable for single, non-transactional operations.
 *
 * - `*_with_conn` variants (e.g., `get_with_conn`, `update_with_conn`)
 *   accept an existing connection or transaction handle (`&C where C: ConnectionTrait`).
 *
 * **WARNING**: To use these functions within a database transaction (e.g., inside
 * a `db.transaction(|txn| { ... })` block), you MUST call the `*_with_conn`
 * variant, passing the transaction handle `txn`. Calling a public version from
 * inside a transaction will try to acquire a second connection from the pool,
 * leading to a deadlock.
 *
 * Correct Usage (in a transaction): `Head::update_with_conn(txn, ...).await;`
 * Incorrect Usage (in a transaction): `Head::update(...).await;` // DEADLOCK!
 */

/// Parse a stored commit id AND verify it belongs to this repository's hash
/// algorithm.
///
/// A well-formed id of the wrong algorithm parses cleanly — the length is
/// valid for ITS kind — and only fails much later, as a panic inside object
/// loading. Every ref-read boundary funnels through here so none can be
/// updated without the check.
/// Parse a stored ref commit and validate it against the repository's hash
/// algorithm. `what` names the hash for the user — "detached HEAD commit
/// hash", not just "commit hash" — because the same helper serves the
/// local, linked-worktree, and remote HEAD boundaries and the message is
/// the only thing that tells them apart.
fn parse_ref_commit(raw: &str, ref_name: &str, what: &str) -> Result<ObjectHash, BranchStoreError> {
    let commit = ObjectHash::from_str(raw).map_err(|error| BranchStoreError::Corrupt {
        name: ref_name.to_string(),
        detail: format!("invalid {what}: {error}"),
    })?;
    let repo_kind = git_internal::hash::get_hash_kind();
    if commit.kind() != repo_kind {
        return Err(BranchStoreError::Corrupt {
            name: ref_name.to_string(),
            detail: format!(
                "{what} is {:?} but this repository uses {:?}",
                commit.kind(),
                repo_kind
            ),
        });
    }
    Ok(commit)
}

impl Head {
    const SQLITE_BUSY_MAX_RETRIES: usize = 15;
    const SQLITE_BUSY_RETRY_BASE_MS: u64 = 100;

    fn is_sqlite_busy(err: &DbErr) -> bool {
        let message = err.to_string();
        message.contains("database is locked") || message.contains("database schema is locked")
    }

    /// This scope's HEAD row, or `Ok(None)` when it genuinely has none yet.
    ///
    /// The distinction matters at the WRITE seam: an absent row means
    /// "insert", while a corrupt or AMBIGUOUS one (two rows for one scope)
    /// must abort. Collapsing them — as `Err(Corrupt) => None` did — turned
    /// an ambiguous scope into a third row.
    async fn query_local_head_optional_with_conn<C>(
        db: &C,
    ) -> Result<Option<reference::Model>, BranchStoreError>
    where
        C: ConnectionTrait,
    {
        Self::query_local_head_rows_with_conn(db).await
    }

    /// This scope's HEAD row, or an actionable error when it has none.
    ///
    /// W0 §C.4.1/C.13: for a linked worktree whose identity the registry does
    /// not know, "HEAD reference is missing from storage" describes a corrupt
    /// repository — which this is not. Name the actual fault and the repair
    /// route, so the first thing the user sees after a damaged `worktree_id`
    /// is not an error implying their objects are gone.
    async fn query_local_head_result_with_conn<C>(
        db: &C,
    ) -> Result<reference::Model, BranchStoreError>
    where
        C: ConnectionTrait,
    {
        if let Some(model) = Self::query_local_head_rows_with_conn(db).await? {
            return Ok(model);
        }
        // `current()`, NOT `for_request()`, and deliberately: `worktree add`
        // seeds the NEW worktree's HEAD by entering its directory, so this must
        // follow the cwd. Answering with the invoking worktree's pinned scope
        // leaves the new worktree with no HEAD row at all — every command in it
        // then fails `LBR-REPO-002`. Worktree-local ADVISORY state (sequencer,
        // dirty, layer, sparse) uses `for_request()`; HEAD is the exception
        // because a command legitimately writes another scope's row.
        let scope = crate::internal::worktree_scope::WorktreeScope::current();
        let detail = match scope.worktree_id() {
            Some(id)
                if crate::command::worktree::registry_knows_linked_worktree(id) == Some(false) =>
            {
                format!(
                    "this linked worktree's identity '{id}' is not in the worktree registry, so \
                     it has no HEAD of its own — run `libra worktree repair --confirm` from \
                     the main worktree to restore it"
                )
            }
            _ => "HEAD reference is missing from storage".to_string(),
        };
        Err(BranchStoreError::Corrupt {
            name: "HEAD".to_string(),
            detail,
        })
    }

    /// This scope's HEAD row, or `Ok(None)` when it genuinely has none.
    ///
    /// An ABSENT row and an AMBIGUOUS one are different answers: the write
    /// seam inserts for the first and must abort on the second. They used to
    /// share `Err(Corrupt)`, which is how "two HEAD rows for this scope"
    /// became "you have none" and then a third row.
    async fn query_local_head_rows_with_conn<C>(
        db: &C,
    ) -> Result<Option<reference::Model>, BranchStoreError>
    where
        C: ConnectionTrait,
    {
        // lore.md 2.1: scope HEAD to the CURRENT worktree (ambient, cwd-derived
        // like `path::index()`). None = main (worktree_id IS NULL); a linked
        // worktree resolves ONLY its own HEAD row, so every caller — public AND
        // `_with_conn` (commit/switch/reset/reflog inside a txn) — reads the
        // same worktree's HEAD and can never leak the main worktree's.
        // Part C §C.4.1: resolve the scope through the single `WorktreeScope`
        // value object rather than re-interpreting a bare `Option<String>` —
        // `worktree_id()` encodes the `reference` table's convention that main
        // is spelled NULL, so a linked worktree can never alias onto main.
        // `current()`, NOT `for_request()`, and deliberately: `worktree add`
        // seeds the NEW worktree's HEAD by entering its directory, so this must
        // follow the cwd. Answering with the invoking worktree's pinned scope
        // leaves the new worktree with no HEAD row at all — every command in it
        // then fails `LBR-REPO-002`. Worktree-local ADVISORY state (sequencer,
        // dirty, layer, sparse) uses `for_request()`; HEAD is the exception
        // because a command legitimately writes another scope's row.
        let scope = crate::internal::worktree_scope::WorktreeScope::current();
        let worktree_id = scope.worktree_id().map(str::to_string);
        for attempt in 0..=Self::SQLITE_BUSY_MAX_RETRIES {
            let mut query = reference::Entity::find()
                .filter(reference::Column::Kind.eq(reference::ConfigKind::Head))
                .filter(reference::Column::Remote.is_null());
            query = match &worktree_id {
                Some(id) => query.filter(reference::Column::WorktreeId.eq(id.clone())),
                None => query.filter(reference::Column::WorktreeId.is_null()),
            };
            // ADR-0714-08: read up to TWO rows, not one. `.one()` resolves a
            // duplicate scope by returning whichever row SQLite hands back
            // first, so a repository that predates migration 2026072901's
            // unique index would silently operate on an arbitrary HEAD — and
            // the next ref write would move that one. An ambiguous scope is
            // never resolved by guessing.
            match query.all(db).await {
                Ok(rows) if rows.len() > 1 => {
                    return Err(BranchStoreError::Corrupt {
                        name: "HEAD".to_string(),
                        detail: match &worktree_id {
                            Some(id) => format!(
                                "more than one HEAD row exists for worktree '{id}', so which \
                                 one this worktree is on cannot be determined; inspect the \
                                 duplicates and remove the row that does not belong to it"
                            ),
                            None => "more than one HEAD row exists for the main worktree, so \
                                     which one it is on cannot be determined; inspect the \
                                     duplicates and remove the row that does not belong to it"
                                .to_string(),
                        },
                    });
                }
                Ok(mut rows) if !rows.is_empty() => return Ok(Some(rows.remove(0))),
                Ok(_) => return Ok(None),
                Err(err)
                    if Self::is_sqlite_busy(&err) && attempt < Self::SQLITE_BUSY_MAX_RETRIES =>
                {
                    sleep(Duration::from_millis(
                        Self::SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                }
                Err(err) => return Err(BranchStoreError::Query(err.to_string())),
            }
        }

        unreachable!("sqlite retry loop must return")
    }

    async fn query_remote_head_with_conn<C>(db: &C, remote: &str) -> Option<reference::Model>
    where
        C: ConnectionTrait,
    {
        for attempt in 0..=Self::SQLITE_BUSY_MAX_RETRIES {
            match reference::Entity::find()
                .filter(reference::Column::Kind.eq(reference::ConfigKind::Head))
                .filter(reference::Column::Remote.eq(remote))
                .one(db)
                .await
            {
                Ok(model) => return model,
                Err(err)
                    if Self::is_sqlite_busy(&err) && attempt < Self::SQLITE_BUSY_MAX_RETRIES =>
                {
                    sleep(Duration::from_millis(
                        Self::SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                }
                Err(err) => {
                    tracing::error!(
                        remote,
                        error = %err,
                        "Failed to query remote HEAD"
                    );
                    return None;
                }
            }
        }

        None
    }

    pub async fn current_with_conn<C>(db: &C) -> Head
    where
        C: ConnectionTrait,
    {
        // INVARIANT: HEAD is always either a branch reference or a detached
        // commit whose hash is a valid 40-char hex string stored by Libra's
        // own write path. A failure here means the SQLite `reference` table
        // is corrupt (HEAD row exists but lacks both a name and a parseable
        // commit hash). The Result-returning `current_result_with_conn`
        // siblings surface this case as `BranchStoreError::Corrupt` for
        // callers that want graceful handling; lossy callers panic.
        Self::current_result_with_conn(db)
            .await
            .expect("HEAD row in reference table is corrupt")
    }

    pub async fn current() -> Head {
        let db_conn = get_db_conn_instance().await;
        Self::current_with_conn(&db_conn).await
    }

    pub async fn current_result_with_conn<C>(db: &C) -> Result<Head, BranchStoreError>
    where
        C: ConnectionTrait,
    {
        let head = Self::query_local_head_result_with_conn(db).await?;
        match head.name {
            Some(name) => Ok(Head::Branch(name)),
            None => {
                let commit_hash = head.commit.ok_or_else(|| BranchStoreError::Corrupt {
                    name: "HEAD".to_string(),
                    detail: "detached HEAD is missing commit hash".to_string(),
                })?;
                let commit_hash =
                    parse_ref_commit(commit_hash.as_str(), "HEAD", "detached HEAD commit hash")?;
                Ok(Head::Detached(commit_hash))
            }
        }
    }

    pub async fn current_result() -> Result<Head, BranchStoreError> {
        let db_conn = get_db_conn_instance().await;
        Self::current_result_with_conn(&db_conn).await
    }

    /// Resolve the HEAD of a SPECIFIC worktree scope (not the ambient cwd
    /// scope): `None` = the main worktree (`worktree_id IS NULL`), `Some(id)` =
    /// a linked worktree. Returns the resolved [`Head`] and its commit hash
    /// (the branch tip, or the detached commit). `Ok(None)` when that scope has
    /// no HEAD row (e.g. a legacy-symlink worktree) so callers can render
    /// deterministically without mislabeling another worktree's HEAD (Part C
    /// §C.3.3 `worktree list --porcelain` per-worktree HEAD).
    pub async fn head_for_worktree_scope(
        worktree_id: Option<&str>,
    ) -> Result<Option<(Head, Option<ObjectHash>)>, BranchStoreError> {
        let db = get_db_conn_instance().await;
        let mut query = reference::Entity::find()
            .filter(reference::Column::Kind.eq(reference::ConfigKind::Head))
            .filter(reference::Column::Remote.is_null());
        query = match worktree_id {
            Some(id) => query.filter(reference::Column::WorktreeId.eq(id.to_string())),
            None => query.filter(reference::Column::WorktreeId.is_null()),
        };
        let Some(model) = query
            .one(&db)
            .await
            .map_err(|err| BranchStoreError::Query(err.to_string()))?
        else {
            return Ok(None);
        };
        match model.name {
            Some(name) => {
                // Branch HEAD: resolve the branch tip commit (shared refs).
                let commit = Branch::find_branch_result(&name, None)
                    .await
                    .ok()
                    .flatten()
                    .map(|b| b.commit);
                Ok(Some((Head::Branch(name), commit)))
            }
            None => {
                let commit_hash = model.commit.ok_or_else(|| BranchStoreError::Corrupt {
                    name: "HEAD".to_string(),
                    detail: "detached HEAD is missing commit hash".to_string(),
                })?;
                let commit =
                    parse_ref_commit(commit_hash.as_str(), "HEAD", "detached HEAD commit hash")?;
                Ok(Some((Head::Detached(commit), Some(commit))))
            }
        }
    }

    pub async fn remote_current_with_conn<C>(db: &C, remote: &str) -> Option<Head>
    where
        C: ConnectionTrait,
    {
        // INVARIANT: like `current_with_conn`, this fails the process when
        // the persisted remote HEAD row is corrupt (no name and no parseable
        // commit hash). Callers that want graceful handling should use
        // `remote_current_result_with_conn`.
        Self::remote_current_result_with_conn(db, remote)
            .await
            .expect("remote HEAD row in reference table is corrupt")
    }

    pub async fn remote_current_result_with_conn<C>(
        db: &C,
        remote: &str,
    ) -> Result<Option<Head>, BranchStoreError>
    where
        C: ConnectionTrait,
    {
        let Some(head) = Self::query_remote_head_with_conn(db, remote).await else {
            return Ok(None);
        };
        match head.name {
            Some(name) => Ok(Some(Head::Branch(name))),
            None => {
                let commit_hash = head.commit.ok_or_else(|| BranchStoreError::Corrupt {
                    name: format!("refs/remotes/{remote}/HEAD"),
                    detail: "detached remote HEAD is missing commit hash".to_string(),
                })?;
                let commit_hash = parse_ref_commit(
                    commit_hash.as_str(),
                    &format!("refs/remotes/{remote}/HEAD"),
                    "detached remote HEAD commit hash",
                )?;
                Ok(Some(Head::Detached(commit_hash)))
            }
        }
    }

    pub async fn remote_current(remote: &str) -> Option<Head> {
        let db_conn = get_db_conn_instance().await;
        Self::remote_current_with_conn(&db_conn, remote).await
    }

    /// Resolve HEAD to its current commit hash.
    ///
    /// Returns `Ok(None)` when HEAD is an **unborn branch** — i.e. HEAD points
    /// to a branch name that has no row in the reference table yet.  This is the
    /// normal state after `libra init` before the first commit, and mirrors
    /// Git's semantics (HEAD → refs/heads/main, but the ref file does not
    /// exist).  It is **not** corruption; callers should treat `None` as
    /// "no commits yet" (e.g. use a zero OID for reflog entries).
    ///
    /// Actual storage failures (DB query errors, corrupt data) are surfaced as
    /// `Err(BranchStoreError)`.
    pub async fn current_commit_result_with_conn<C>(
        db: &C,
    ) -> Result<Option<ObjectHash>, BranchStoreError>
    where
        C: ConnectionTrait,
    {
        match Self::current_result_with_conn(db).await? {
            Head::Branch(name) => Ok(Branch::find_branch_result_with_conn(db, &name, None)
                .await?
                .map(|branch| branch.commit)),
            Head::Detached(commit_hash) => Ok(Some(commit_hash)),
        }
    }

    pub async fn current_commit_result() -> Result<Option<ObjectHash>, BranchStoreError> {
        let db_conn = get_db_conn_instance().await;
        Self::current_commit_result_with_conn(&db_conn).await
    }

    /// Lossy compatibility wrapper. Prefer `current_commit_result_with_conn`
    /// in production paths so storage failures are not downgraded to `None`.
    pub async fn current_commit_with_conn<C>(db: &C) -> Option<ObjectHash>
    where
        C: ConnectionTrait,
    {
        match Self::current_commit_result_with_conn(db).await {
            Ok(commit) => commit,
            Err(error) => {
                tracing::error!("failed to resolve HEAD commit: {error}");
                None
            }
        }
    }

    /// Lossy compatibility wrapper. Prefer `current_commit_result` in
    /// production paths so storage failures are not downgraded to `None`.
    pub async fn current_commit() -> Option<ObjectHash> {
        let db_conn = get_db_conn_instance().await;
        Self::current_commit_with_conn(&db_conn).await
    }

    pub async fn update_result_with_conn<C>(
        db: &C,
        new_head: Self,
        remote: Option<&str>,
    ) -> Result<(), BranchStoreError>
    where
        C: ConnectionTrait,
    {
        // §C.4.4: attaching THIS worktree's HEAD to a branch another worktree
        // already has checked out creates the duplicate checkout Libra
        // categorically refuses. The check lives at the seam, on the caller's
        // connection, because the call sites that pre-flighted it — `switch`,
        // `checkout`, `symbolic-ref`, `stash branch`, `worktree add` — each
        // did so in a SEPARATE transaction from the write, leaving a window
        // for the other worktree to attach in between.
        if remote.is_none()
            && let Head::Branch(branch_name) = &new_head
            && let Some(other) =
                Self::branch_checked_out_elsewhere_result_with_conn(db, branch_name)
                    .await
                    .map_err(|error| BranchStoreError::Query(error.to_string()))?
        {
            return Err(BranchStoreError::CheckedOutElsewhere {
                name: branch_name.clone(),
                worktree: other,
            });
        }
        for attempt in 0..=Self::SQLITE_BUSY_MAX_RETRIES {
            let head = match remote {
                Some(remote) => Self::query_remote_head_with_conn(db, remote).await,
                // lore.md 2.1: a linked worktree has no HEAD row until first
                // seeded — a missing per-worktree HEAD falls to the INSERT
                // branch (which tags the row with the current worktree id)
                // rather than erroring.
                // A genuinely ABSENT row falls to the INSERT branch. A
                // CORRUPT or AMBIGUOUS one must not: swallowing
                // `Corrupt` here turned "this scope has two HEAD rows and I
                // cannot tell which is yours" into "you have none", and
                // inserted a third.
                None => match Self::query_local_head_optional_with_conn(db).await {
                    Ok(model) => model,
                    Err(e) => return Err(e),
                },
            };

            let write_result = match head {
                Some(head) => {
                    // update
                    let mut head: reference::ActiveModel = head.into();
                    if remote.is_some() {
                        head.remote = Set(remote.map(|s| s.to_owned()));
                    }
                    match &new_head {
                        Head::Detached(commit_hash) => {
                            head.commit = Set(Some(commit_hash.to_string()));
                            head.name = Set(None);
                        }
                        Head::Branch(branch_name) => {
                            head.name = Set(Some(branch_name.clone()));
                            head.commit = Set(None);
                        }
                    }
                    head.update(db).await.map(|_| ())
                }
                None => {
                    let mut head = reference::ActiveModel {
                        kind: Set(reference::ConfigKind::Head),
                        ..Default::default()
                    };
                    if remote.is_some() {
                        head.remote = Set(remote.map(|s| s.to_owned()));
                    } else {
                        // lore.md 2.1: a NEW local HEAD row is tagged with the
                        // current worktree id (NULL for main) so it is private
                        // to this worktree.
                        head.worktree_id = Set(crate::utils::util::current_worktree_id());
                    }
                    match &new_head {
                        Head::Detached(commit_hash) => {
                            head.commit = Set(Some(commit_hash.to_string()));
                        }
                        Head::Branch(branch_name) => {
                            head.name = Set(Some(branch_name.clone()));
                        }
                    }
                    head.save(db).await.map(|_| ())
                }
            };

            match write_result {
                Ok(()) => return Ok(()),
                Err(err)
                    if Self::is_sqlite_busy(&err) && attempt < Self::SQLITE_BUSY_MAX_RETRIES =>
                {
                    sleep(Duration::from_millis(
                        Self::SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                }
                Err(err) => return Err(BranchStoreError::Query(err.to_string())),
            }
        }

        Err(BranchStoreError::Query(
            "failed to update HEAD reference after sqlite busy retries".to_string(),
        ))
    }

    /// lore.md 2.1: the worktree id (if any) OTHER than the current one that
    /// currently has `branch` checked out as HEAD. Branches are SHARED across
    /// worktrees, so two worktrees on one branch would both move the same
    /// pointer — `switch`/`checkout` use this to refuse (git parity).
    /// Result-returning probe over EVERY scope (including the caller's):
    /// which worktree, if any, has `branch` checked out. Recovery paths use
    /// this so a query failure FAILS CLOSED instead of reading as
    /// "not attached anywhere" (the infallible probe below folds errors
    /// into an empty result, which is fine for its advisory callers but
    /// not before a destructive ref delete).
    pub async fn branch_checked_out_anywhere_result(
        branch: &str,
    ) -> Result<Option<String>, sea_orm::DbErr> {
        if crate::utils::util::fault_injected("branch-attach-probe") {
            return Err(sea_orm::DbErr::Custom(
                "injected branch-attach probe fault".to_string(),
            ));
        }
        let db = get_db_conn_instance().await;
        let rows = reference::Entity::find()
            .filter(reference::Column::Kind.eq(reference::ConfigKind::Head))
            .filter(reference::Column::Remote.is_null())
            .filter(reference::Column::Name.eq(branch))
            .all(&db)
            .await?;
        Ok(rows
            .into_iter()
            .next()
            .map(|row| row.worktree_id.unwrap_or_else(|| "(main)".to_string())))
    }

    // §C.4.4: the fail-OPEN convenience wrapper that used to live here is
    // GONE. Documenting that destructive writers "MUST" use the fallible
    // variant did not stop nine of them from using this one, because the
    // infallible signature is the more convenient of the two at every call
    // site. Deleting it makes the safe choice the only choice.

    /// Fail-CLOSED cross-worktree checkout probe: a query failure is an
    /// error the caller must surface, never "nobody has it checked out".
    pub async fn branch_checked_out_elsewhere_result(
        branch: &str,
    ) -> Result<Option<String>, sea_orm::DbErr> {
        let db = get_db_conn_instance().await;
        Self::branch_checked_out_elsewhere_result_with_conn(&db, branch).await
    }

    /// Transaction-safe form of [`Self::branch_checked_out_elsewhere_result`].
    ///
    /// A ref writer that already holds a write transaction MUST use this. The
    /// public form opens the pooled connection, which on SQLite blocks behind
    /// the caller's own open write — the probe and the write deadlock each
    /// other. Running on the caller's connection also makes the check and the
    /// ref update atomic, which is the point: a probe committed separately
    /// could be stale by the time the write lands.
    pub async fn branch_checked_out_elsewhere_result_with_conn<C>(
        db: &C,
        branch: &str,
    ) -> Result<Option<String>, sea_orm::DbErr>
    where
        C: ConnectionTrait,
    {
        let current = crate::utils::util::current_worktree_id();
        let rows = reference::Entity::find()
            .filter(reference::Column::Kind.eq(reference::ConfigKind::Head))
            .filter(reference::Column::Remote.is_null())
            .filter(reference::Column::Name.eq(branch))
            .all(db)
            .await?;
        Ok(rows
            .into_iter()
            .find(|row| row.worktree_id != current)
            .map(|row| row.worktree_id.unwrap_or_else(|| "(main)".to_string())))
    }

    /// Deprecated shim retained ONLY for call sites that have already
    /// decided a HEAD write failure is not their problem.
    ///
    /// There are none left in production code, and there should not be: this
    /// swallowed the error, so a command whose HEAD update failed still
    /// committed the work around it and reported success — a repository left
    /// pointing at the wrong commit, silently. Use
    /// [`Head::update_result_with_conn`].
    #[cfg(test)]
    pub async fn update_with_conn<C>(db: &C, new_head: Self, remote: Option<&str>)
    where
        C: ConnectionTrait,
    {
        if let Err(error) = Self::update_result_with_conn(db, new_head, remote).await {
            tracing::error!(
                remote = ?remote,
                error = %error,
                "Failed to update HEAD reference"
            );
        }
    }

    /// Must NOT be called from within an active transaction (it acquires its
    /// own pooled connection — see the `_with_conn` contract in `branch.rs`).
    pub async fn update_result(
        new_head: Self,
        remote: Option<&str>,
    ) -> Result<(), BranchStoreError> {
        let db_conn = get_db_conn_instance().await;
        // §C.4.4: the checked-out-elsewhere probe inside
        // `update_result_with_conn` and the HEAD write must be ONE
        // transaction. On a pool connection they are two implicit ones, so
        // two worktrees attaching to the same branch can both pass the probe
        // and both write — the exact duplicate checkout the guard exists to
        // prevent. Callers that already hold a transaction (`switch`,
        // `checkout`, `worktree add`) were always atomic; this closes the
        // pooled entry point that `symbolic-ref` and `stash branch` use.
        // HEAD's update reads the current row before replacing it.
        let txn = crate::internal::db::begin_write_transaction(&db_conn)
            .await
            .map_err(|e| BranchStoreError::Query(e.to_string()))?;
        Self::update_result_with_conn(&txn, new_head, remote).await?;
        txn.commit()
            .await
            .map_err(|e| BranchStoreError::Query(e.to_string()))
    }

    // HEAD is unique, update if exists, insert if not
    pub async fn update(new_head: Self, remote: Option<&str>) {
        // Routed through the transactional form so the infallible entry point
        // is not the one that skips the §C.4.4 atomicity the fallible one has.
        if let Err(error) = Self::update_result(new_head, remote).await {
            tracing::error!(
                remote = ?remote,
                error = %error,
                "Failed to update HEAD reference"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::utils::test::{self, ChangeDirGuard};

    #[tokio::test]
    #[serial]
    async fn current_commit_result_with_conn_returns_corrupt_when_head_row_missing() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());

        let db = get_db_conn_instance().await;
        reference::Entity::delete_many()
            .filter(reference::Column::Kind.eq(reference::ConfigKind::Head))
            .filter(reference::Column::Remote.is_null())
            .exec(&db)
            .await
            .unwrap();

        let error = Head::current_commit_result_with_conn(&db)
            .await
            .expect_err("missing HEAD row should surface as corruption");
        assert!(matches!(error, BranchStoreError::Corrupt { .. }));
        assert!(
            error.to_string().contains("HEAD reference is missing"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    #[serial]
    async fn current_commit_result_with_conn_returns_corrupt_for_invalid_detached_hash() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());

        let db = get_db_conn_instance().await;
        let head = reference::Entity::find()
            .filter(reference::Column::Kind.eq(reference::ConfigKind::Head))
            .filter(reference::Column::Remote.is_null())
            .one(&db)
            .await
            .unwrap()
            .expect("expected HEAD row");
        let mut head: reference::ActiveModel = head.into();
        head.name = Set(None);
        head.commit = Set(Some("not-a-valid-hash".to_string()));
        head.update(&db).await.unwrap();

        let error = Head::current_commit_result_with_conn(&db)
            .await
            .expect_err("invalid detached HEAD hash should surface as corruption");
        assert!(matches!(error, BranchStoreError::Corrupt { .. }));
        assert!(
            error
                .to_string()
                .contains("invalid detached HEAD commit hash"),
            "unexpected error: {error}"
        );
    }

    /// Regression for v0.17.238: when no row exists for the requested remote
    /// HEAD, `remote_current_result_with_conn` returns `Ok(None)` rather
    /// than producing a `BranchStoreError`. This mirrors the historical
    /// behavior of the lossy `remote_current_with_conn` (which returned
    /// `Option::None`) and lets callers cleanly distinguish "no remote HEAD
    /// recorded" from "remote HEAD is corrupt".
    #[tokio::test]
    #[serial]
    async fn remote_current_result_with_conn_returns_none_for_unknown_remote() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());

        let db = get_db_conn_instance().await;
        let result = Head::remote_current_result_with_conn(&db, "no-such-remote")
            .await
            .expect("unknown remote should not surface as error");
        assert!(
            result.is_none(),
            "expected Ok(None) for unrecorded remote HEAD, got {result:?}"
        );
    }

    /// Regression for v0.17.238: a remote HEAD row that is detached
    /// (`name = NULL`) but whose stored commit hash is unparseable must
    /// surface as `BranchStoreError::Corrupt`. Before v0.17.238 the lossy
    /// `remote_current_with_conn` would panic via `ObjectHash::from_str(...).unwrap()`.
    /// The error message names the canonical refspec
    /// (`refs/remotes/<remote>/HEAD`) so operators can locate the bad row.
    #[tokio::test]
    #[serial]
    async fn remote_current_result_with_conn_returns_corrupt_for_invalid_detached_hash() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());

        let db = get_db_conn_instance().await;
        let remote = "origin";
        let row = reference::ActiveModel {
            kind: Set(reference::ConfigKind::Head),
            remote: Set(Some(remote.to_string())),
            name: Set(None),
            commit: Set(Some("not-a-valid-hash".to_string())),
            ..Default::default()
        };
        reference::Entity::insert(row).exec(&db).await.unwrap();

        let error = Head::remote_current_result_with_conn(&db, remote)
            .await
            .expect_err("invalid remote HEAD hash should surface as corruption");
        assert!(matches!(error, BranchStoreError::Corrupt { .. }));
        let msg = error.to_string();
        assert!(
            msg.contains("invalid detached remote HEAD commit hash"),
            "unexpected error: {msg}"
        );
        assert!(
            msg.contains(&format!("refs/remotes/{remote}/HEAD")),
            "error should name the canonical refspec: {msg}"
        );
    }

    /// Regression for v0.17.238: the lossy `Head::current_with_conn` must
    /// panic with the INVARIANT message `"HEAD row in reference table is
    /// corrupt"` when the underlying `current_result_with_conn` returns
    /// `Err(BranchStoreError::Corrupt { .. })`. This pins the panic
    /// message contract so a future refactor of the Result-returning
    /// helper cannot silently drift the lossy variant's diagnostic output.
    #[tokio::test]
    #[serial]
    #[should_panic(expected = "HEAD row in reference table is corrupt")]
    async fn current_with_conn_panics_with_invariant_message_when_head_corrupt() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());

        let db = get_db_conn_instance().await;
        reference::Entity::delete_many()
            .filter(reference::Column::Kind.eq(reference::ConfigKind::Head))
            .filter(reference::Column::Remote.is_null())
            .exec(&db)
            .await
            .unwrap();

        let _ = Head::current_with_conn(&db).await;
    }

    /// Regression for v0.17.238 (parallel to `current_with_conn_panics_*`):
    /// the lossy `Head::remote_current_with_conn` must panic with the
    /// INVARIANT message `"remote HEAD row in reference table is corrupt"`
    /// when its Result-returning sibling surfaces a
    /// `BranchStoreError::Corrupt { .. }`. Insert a detached remote HEAD
    /// row whose stored commit hash is unparseable, then call the lossy
    /// variant.
    #[tokio::test]
    #[serial]
    #[should_panic(expected = "remote HEAD row in reference table is corrupt")]
    async fn remote_current_with_conn_panics_with_invariant_message_when_remote_head_corrupt() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());

        let db = get_db_conn_instance().await;
        let remote = "origin";
        let row = reference::ActiveModel {
            kind: Set(reference::ConfigKind::Head),
            remote: Set(Some(remote.to_string())),
            name: Set(None),
            commit: Set(Some("not-a-valid-hash".to_string())),
            ..Default::default()
        };
        reference::Entity::insert(row).exec(&db).await.unwrap();

        let _ = Head::remote_current_with_conn(&db, remote).await;
    }
}
