//! Object-safe [`GitStore`] core reference, reflog, worktree isolation, and
//! atomic bridge publication storage trait (`ADR-RB-04`, `RB-04`).
//!
//! Covers the core reference/reflog/bridge-binding domain (`reference`,
//! `reflog`, and same-transaction `agent_bridge_publication`) with identical
//! semantics across [`SqliteGitStore`] and [`RedbGitStore`].

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::storage::{
    backend::StorageBackendKind,
    error::{StorageBackendError, StorageResult},
};

#[cfg(feature = "storage-redb")]
#[path = "redb/git_store.rs"]
pub mod redb;
#[cfg(feature = "storage-sqlite")]
#[path = "sqlite/git_store.rs"]
pub mod sqlite;

#[cfg(feature = "storage-redb")]
pub use redb::RedbGitStore;
#[cfg(feature = "storage-sqlite")]
pub use sqlite::SqliteGitStore;

/// Canonical reflog target name for a worktree-scoped `HEAD` reflog.
pub const REFLOG_HEAD_REF_NAME: &str = "HEAD";

/// Maximum allowed byte length for an `agent_bridge_publication.operation_id`.
pub const MAX_BRIDGE_OPERATION_ID_LEN: usize = 128;

/// Canonical identity of one row in the `reference` table (`RB-04`).
///
/// Enforces worktree scoping cleanly in the type system:
/// - [`Self::LocalHead`] is strictly scoped per worktree (`None` = main
///   worktree, `Some(id)` = linked worktree).
/// - [`Self::RemoteHead`], [`Self::LocalBranch`], [`Self::RemoteBranch`], and
///   [`Self::Tag`] are repository-wide shared references visible identically
///   across the main worktree and every linked worktree.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RefKey {
    /// Worktree-local `HEAD` (`kind = 'Head'`, `remote IS NULL`).
    /// `worktree_id = None` identifies the main worktree; `Some(id)` identifies
    /// a linked worktree.
    LocalHead { worktree_id: Option<String> },
    /// Remote `HEAD` (`kind = 'Head'`, `remote = Some(remote)`, `worktree_id IS NULL`).
    RemoteHead { remote: String },
    /// Shared local branch (`kind = 'Branch'`, `remote IS NULL`, `worktree_id IS NULL`).
    LocalBranch { name: String },
    /// Shared remote-tracking branch (`kind = 'Branch'`, `remote = Some(remote)`, `worktree_id IS NULL`).
    RemoteBranch { remote: String, name: String },
    /// Shared tag (`kind = 'Tag'`, `remote IS NULL`, `worktree_id IS NULL`).
    Tag { name: String },
}

impl RefKey {
    /// Returns `true` if this reference is scoped to a specific worktree
    /// (`LocalHead`) rather than shared across all worktrees.
    #[must_use]
    pub const fn is_worktree_local(&self) -> bool {
        matches!(self, Self::LocalHead { .. })
    }

    /// Return the owning worktree ID if this is a [`Self::LocalHead`], or
    /// `None` for the main worktree and all shared references.
    #[must_use]
    pub fn worktree_id(&self) -> Option<&str> {
        match self {
            Self::LocalHead { worktree_id } => worktree_id.as_deref(),
            Self::RemoteHead { .. }
            | Self::LocalBranch { .. }
            | Self::RemoteBranch { .. }
            | Self::Tag { .. } => None,
        }
    }

    /// Returns `true` if this reference is visible to a worktree scope identified
    /// by `worktree_id` (`None` = main worktree, `Some(id)` = linked worktree).
    #[must_use]
    pub fn visible_to_worktree(&self, worktree_id: Option<&str>) -> bool {
        match self {
            Self::LocalHead { worktree_id: owner } => owner.as_deref() == worktree_id,
            Self::RemoteHead { .. }
            | Self::LocalBranch { .. }
            | Self::RemoteBranch { .. }
            | Self::Tag { .. } => true,
        }
    }
}

/// Target stored in a `reference` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefTarget {
    /// Direct object ID (commit/tag OID for branches, tags, or detached `HEAD`).
    Direct(String),
    /// Symbolic branch name (valid for `LocalHead` and `RemoteHead`).
    Symbolic(String),
}

/// One materialized `reference` entry returned by listing APIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefRecord {
    /// Canonical reference key.
    pub key: RefKey,
    /// Current target (direct OID or symbolic branch name).
    pub target: RefTarget,
}

/// Input payload for appending one entry to `reflog` inside a ref update transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflogWrite {
    /// Reference name recorded in `reflog.ref_name` (e.g. `"HEAD"` or `"refs/heads/main"`).
    pub ref_name: String,
    /// Previous object ID (or all-zero OID on creation).
    pub old_oid: String,
    /// New object ID (or all-zero OID on deletion).
    pub new_oid: String,
    /// Unix timestamp in seconds.
    pub timestamp: i64,
    /// Committer display name.
    pub committer_name: String,
    /// Committer email address.
    pub committer_email: String,
    /// Short reflog action tag (e.g. `"commit"`, `"checkout"`, `"update-ref"`).
    pub action: String,
    /// Human-readable reflog message body.
    pub message: String,
    /// Worktree scope for `HEAD` reflog rows (`None` for main worktree and all
    /// shared branch reflogs; `Some(id)` only when `ref_name == "HEAD"`).
    pub worktree_id: Option<String>,
}

/// Materialized `reflog` row returned by [`GitStore::list_reflog`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflogEntry {
    /// Monotonic primary key assigned by the backend (`id`).
    pub id: i64,
    /// Reference name recorded in `reflog.ref_name`.
    pub ref_name: String,
    /// Previous object ID.
    pub old_oid: String,
    /// New object ID.
    pub new_oid: String,
    /// Unix timestamp in seconds.
    pub timestamp: i64,
    /// Committer display name.
    pub committer_name: String,
    /// Committer email address.
    pub committer_email: String,
    /// Short reflog action tag.
    pub action: String,
    /// Human-readable reflog message body.
    pub message: String,
    /// Owning worktree ID for linked-worktree `HEAD` reflog rows, or `None`.
    pub worktree_id: Option<String>,
}

/// Lifecycle phase recorded in `agent_bridge_publication.phase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BridgePublicationPhase {
    /// Commit object written before ref publication committed.
    Prepared,
    /// Reference move and operation-to-commit binding committed atomically.
    Published,
}

impl BridgePublicationPhase {
    /// Canonical SQL text representation matching `src/internal/ai/agent_bridge/publication.rs`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Published => "published",
        }
    }

    /// Parse canonical SQL text phase representation.
    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "prepared" => Ok(Self::Prepared),
            "published" => Ok(Self::Published),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid agent_bridge_publication phase value".to_string(),
            }),
        }
    }
}

/// One row in `agent_bridge_publication` binding a bridge `operation_id` to
/// its published `commit_oid` inside the same transaction that moves `HEAD`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgePublicationRecord {
    /// Bridge operation identifier (1..=128 bytes of `[A-Za-z0-9_.:-]`).
    pub operation_id: String,
    /// Published commit object ID.
    pub commit_oid: String,
    /// Publication lifecycle phase (`prepared` or `published`).
    pub phase: BridgePublicationPhase,
}

/// Result of an atomic compare-and-swap reference update (`RB-04` AC1–AC3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefCasOutcome {
    /// Expected old target matched and the ref update (plus any reflog and
    /// bridge publication records) committed atomically in a single transaction.
    Applied {
        /// Prior target before the update (`None` for creation).
        previous: Option<RefTarget>,
        /// Resulting target after the update (`None` for deletion).
        current: Option<RefTarget>,
    },
    /// Expected old target did not match; transaction aborted with zero ref,
    /// reflog, or bridge publication writes.
    CasConflict {
        /// Actual current target stored in the database.
        actual: Option<RefTarget>,
    },
}

/// Returns `true` when `operation_id` satisfies the bindable bridge identifier
/// invariant (`src/internal/ai/agent_bridge/publication.rs::operation_id_is_bindable`).
#[must_use]
pub fn is_valid_bridge_operation_id(operation_id: &str) -> bool {
    !operation_id.is_empty()
        && operation_id.len() <= MAX_BRIDGE_OPERATION_ID_LEN
        && operation_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
}

fn validate_non_empty_identifier(value: &str, label: &'static str) -> StorageResult<()> {
    if value.is_empty() || value.contains('\0') {
        return Err(StorageBackendError::BackendInternal {
            detail: format!("{label} must be non-empty and contain no NUL bytes"),
        });
    }
    Ok(())
}

/// Validate a [`RefKey`] against SQLite and `redb` storage invariants.
pub(crate) fn validate_ref_key(key: &RefKey) -> StorageResult<()> {
    match key {
        RefKey::LocalHead { worktree_id } => {
            if let Some(wt) = worktree_id {
                validate_non_empty_identifier(wt, "worktree_id")?;
            }
            Ok(())
        }
        RefKey::RemoteHead { remote } => validate_non_empty_identifier(remote, "remote"),
        RefKey::LocalBranch { name } => validate_non_empty_identifier(name, "branch name"),
        RefKey::RemoteBranch { remote, name } => {
            validate_non_empty_identifier(remote, "remote")?;
            validate_non_empty_identifier(name, "branch name")
        }
        RefKey::Tag { name } => validate_non_empty_identifier(name, "tag name"),
    }
}

/// Validate that `target` is well-formed and permitted for `key`.
pub(crate) fn validate_ref_target(key: &RefKey, target: &RefTarget) -> StorageResult<()> {
    match target {
        RefTarget::Direct(oid) => validate_non_empty_identifier(oid, "ref commit OID"),
        RefTarget::Symbolic(branch_name) => {
            if !matches!(key, RefKey::LocalHead { .. } | RefKey::RemoteHead { .. }) {
                return Err(StorageBackendError::BackendInternal {
                    detail: "symbolic targets are only valid for LocalHead or RemoteHead"
                        .to_string(),
                });
            }
            validate_non_empty_identifier(branch_name, "symbolic target branch name")
        }
    }
}

/// Validate a [`ReflogWrite`] against worktree-scoping invariants (`src/internal/reflog.rs`).
pub(crate) fn validate_reflog_write(entry: &ReflogWrite) -> StorageResult<()> {
    validate_non_empty_identifier(&entry.ref_name, "reflog ref_name")?;
    validate_non_empty_identifier(&entry.old_oid, "reflog old_oid")?;
    validate_non_empty_identifier(&entry.new_oid, "reflog new_oid")?;
    if let Some(wt) = &entry.worktree_id {
        validate_non_empty_identifier(wt, "reflog worktree_id")?;
        if entry.ref_name != REFLOG_HEAD_REF_NAME {
            return Err(StorageBackendError::BackendInternal {
                detail: "non-HEAD reflog entries must not set a per-worktree worktree_id"
                    .to_string(),
            });
        }
    }
    Ok(())
}

/// Validate that a `reflog` batch respects worktree ownership for `key` (`RB-04`).
///
/// - When `key` is `LocalHead { worktree_id }`, any `"HEAD"` reflog entry must
///   target the exact same `worktree_id` (cannot cross main ↔ linked or
///   linked-A ↔ linked-B worktree boundaries).
/// - When `key` is `RemoteHead`, `RemoteBranch`, or `Tag`, `"HEAD"` reflog
///   entries are forbidden.
/// - When `key` is `LocalBranch`, any `"HEAD"` reflog entry is further checked
///   inside the write transaction to verify that worktree's `LocalHead` points
///   symbolically to that branch.
pub(crate) fn validate_reflog_writes_for_key(
    key: &RefKey,
    reflogs: &[ReflogWrite],
) -> StorageResult<()> {
    for entry in reflogs {
        validate_reflog_write(entry)?;
        if entry.ref_name == REFLOG_HEAD_REF_NAME {
            match key {
                RefKey::LocalHead { worktree_id } => {
                    if entry.worktree_id != *worktree_id {
                        return Err(StorageBackendError::BackendInternal {
                            detail:
                                "HEAD reflog worktree_id must match the LocalHead being updated"
                                    .to_string(),
                        });
                    }
                }
                RefKey::LocalBranch { .. } => {
                    // Verified against live `LocalHead` symbolic target inside the transaction.
                }
                RefKey::RemoteHead { .. } | RefKey::RemoteBranch { .. } | RefKey::Tag { .. } => {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "cannot append a HEAD reflog entry for a non-HEAD, non-local-branch reference"
                            .to_string(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Validate that a bridge publication record is tightly bound to the reference
/// transition being committed (`ADR-RB-04` #1, `RB-04` AC3):
/// - `publication.phase` must be [`BridgePublicationPhase::Published`].
/// - `new_target` must be `Some(RefTarget::Direct(oid))` with `oid == publication.commit_oid`.
/// - `expected` must differ from `new_target` (refusing no-op publication bindings).
/// - `key` must be either [`RefKey::LocalHead`] (detached `HEAD` move) or
///   [`RefKey::LocalBranch`] (further verified inside the write transaction to
///   be the symbolic target of a worktree's `LocalHead`).
pub(crate) fn validate_bridge_publication_binding(
    key: &RefKey,
    expected: Option<&RefTarget>,
    new_target: Option<&RefTarget>,
    publication: &BridgePublicationRecord,
) -> StorageResult<()> {
    validate_bridge_publication(publication)?;
    if publication.phase != BridgePublicationPhase::Published {
        return Err(StorageBackendError::BackendInternal {
            detail: "update_ref_cas_with_bridge_publication requires phase=published".to_string(),
        });
    }
    if !matches!(key, RefKey::LocalHead { .. } | RefKey::LocalBranch { .. }) {
        return Err(StorageBackendError::BackendInternal {
            detail: "bridge publication may only bind a LocalHead or checked-out LocalBranch move"
                .to_string(),
        });
    }
    let Some(RefTarget::Direct(target_oid)) = new_target else {
        return Err(StorageBackendError::BackendInternal {
            detail: "bridge publication requires a direct commit OID ref target".to_string(),
        });
    };
    if target_oid != &publication.commit_oid {
        return Err(StorageBackendError::BackendInternal {
            detail: "bridge publication commit_oid must match the new ref target OID".to_string(),
        });
    }
    if expected == new_target {
        return Err(StorageBackendError::BackendInternal {
            detail: "bridge publication requires a ref transition (expected must differ from new_target)"
                .to_string(),
        });
    }
    Ok(())
}

/// Validate a [`BridgePublicationRecord`] before entering a write transaction.
pub(crate) fn validate_bridge_publication(record: &BridgePublicationRecord) -> StorageResult<()> {
    if !is_valid_bridge_operation_id(&record.operation_id) {
        return Err(StorageBackendError::BackendInternal {
            detail: "bridge publication operation_id is empty or contains invalid characters"
                .to_string(),
        });
    }
    validate_non_empty_identifier(&record.commit_oid, "bridge publication commit_oid")
}

/// Validate a worktree filter passed to read queries.
pub(crate) fn validate_worktree_filter(worktree_id: Option<&str>) -> StorageResult<()> {
    if let Some(wt) = worktree_id {
        validate_non_empty_identifier(wt, "worktree_id")?;
    }
    Ok(())
}

/// Object-safe core Git reference, reflog, and bridge publication store
/// abstraction backed by SQLite or `redb 5.0` (`ADR-RB-04` #1, `RB-04`).
#[async_trait]
pub trait GitStore: Send + Sync + fmt::Debug {
    /// Active storage backend kind (`Sqlite` or `Redb`).
    fn backend(&self) -> StorageBackendKind;

    /// Read a single reference target by exact [`RefKey`], or `Ok(None)` if absent.
    async fn get_ref(&self, key: &RefKey) -> StorageResult<Option<RefTarget>>;

    /// List all references visible to `worktree_id` (`None` = main worktree,
    /// `Some(id)` = linked worktree), sorted deterministically by [`RefKey`].
    ///
    /// Returns ONLY the requested worktree's own [`RefKey::LocalHead`] (if
    /// present) plus ALL shared non-local references (`RemoteHead`,
    /// `LocalBranch`, `RemoteBranch`, `Tag`).
    async fn list_refs(&self, worktree_id: Option<&str>) -> StorageResult<Vec<RefRecord>>;

    /// List every [`RefKey::LocalHead`] row across main and all linked worktrees,
    /// sorted deterministically by [`RefKey`].
    async fn list_all_local_heads(&self) -> StorageResult<Vec<RefRecord>>;

    /// Atomically compare `key` against `expected`, apply `new_target` (`Some`
    /// to upsert, `None` to delete), and append all `reflogs` in a single
    /// database write transaction (`RB-04` AC1–AC2).
    ///
    /// On mismatch, aborts the transaction with zero writes and returns
    /// [`RefCasOutcome::CasConflict`].
    async fn update_ref_cas(
        &self,
        key: &RefKey,
        expected: Option<&RefTarget>,
        new_target: Option<&RefTarget>,
        reflogs: &[ReflogWrite],
    ) -> StorageResult<RefCasOutcome>;

    /// Atomically execute [`Self::update_ref_cas`] together with binding
    /// `publication` in `agent_bridge_publication` inside the SAME database
    /// write transaction (`ADR-RB-04` #1, `RB-04` AC3).
    ///
    /// If the CAS check fails (`CasConflict`) OR `publication.operation_id` is
    /// already bound to a different `commit_oid` (or invalid), the entire
    /// transaction rolls back leaving zero orphan ref, reflog, or bridge
    /// publication rows.
    async fn update_ref_cas_with_bridge_publication(
        &self,
        key: &RefKey,
        expected: Option<&RefTarget>,
        new_target: Option<&RefTarget>,
        reflogs: &[ReflogWrite],
        publication: &BridgePublicationRecord,
    ) -> StorageResult<RefCasOutcome>;

    /// List reflog entries for `(ref_name, worktree_id)` ordered newest-first
    /// (`id DESC`, matching `HEAD@{0}` at index 0).
    ///
    /// For `ref_name == "HEAD"`, `worktree_id` strictly isolates the main
    /// worktree (`None`) from linked worktrees (`Some(id)`). For all other
    /// ref names, `worktree_id` must be `None`.
    async fn list_reflog(
        &self,
        ref_name: &str,
        worktree_id: Option<&str>,
        limit: Option<usize>,
    ) -> StorageResult<Vec<ReflogEntry>>;

    /// Record a `prepared` bridge publication row before the ref update commits
    /// (`src/internal/ai/agent_bridge/publication.rs::prepare_current`).
    async fn prepare_bridge_publication(
        &self,
        operation_id: &str,
        commit_oid: &str,
    ) -> StorageResult<()>;

    /// Look up one `agent_bridge_publication` record by `operation_id`.
    async fn get_bridge_publication(
        &self,
        operation_id: &str,
    ) -> StorageResult<Option<BridgePublicationRecord>>;

    /// List all `agent_bridge_publication` records sorted by `operation_id ASC`.
    async fn list_bridge_publications(&self) -> StorageResult<Vec<BridgePublicationRecord>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(n: u64) -> String {
        format!("{n:040x}")
    }

    fn make_reflog(
        ref_name: &str,
        old_n: u64,
        new_n: u64,
        action: &str,
        message: &str,
        worktree_id: Option<&str>,
    ) -> ReflogWrite {
        ReflogWrite {
            ref_name: ref_name.to_string(),
            old_oid: oid(old_n),
            new_oid: oid(new_n),
            timestamp: 1_795_360_000 + i64::try_from(new_n).unwrap_or(0),
            committer_name: "Test Committer".to_string(),
            committer_email: "committer@example.com".to_string(),
            action: action.to_string(),
            message: message.to_string(),
            worktree_id: worktree_id.map(str::to_string),
        }
    }

    /// `RB-04` (AC1–AC5): Verify exact dual-backend equivalence between
    /// `SqliteGitStore` and `RedbGitStore` across:
    /// - AC1: `update_ref_cas` atomically compares, updates ref, and appends reflog.
    /// - AC2: `update_ref_cas` mismatch returns `CasConflict` with zero orphan reflog rows.
    /// - AC3: `update_ref_cas_with_bridge_publication` atomically binds HEAD move
    ///   and `agent_bridge_publication` in one transaction, rolling back cleanly
    ///   with zero orphan rows on CAS conflict, conflicting `operation_id`, or
    ///   injected pre-commit abort.
    /// - AC4: Main worktree (`None`) and linked worktree (`Some(id)`) local
    ///   `HEAD` and `HEAD` reflogs are strictly isolated by `worktree_id`.
    /// - AC5: Shared non-local refs (`LocalBranch`, `RemoteBranch`, `RemoteHead`,
    ///   `Tag`) are identically visible from main and linked worktrees on both backends.
    #[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
    #[tokio::test]
    async fn dual_backend_core_refs_reflog_and_bridge_cas_equivalence() {
        use std::sync::Arc;

        use tempfile::tempdir;

        let dir = tempdir().expect("create RB-04 dual-backend tempdir");
        let sqlite_path = dir.path().join("libra.db");
        let redb_path = dir.path().join("git.redb");

        let sqlite_store = SqliteGitStore::create(&sqlite_path)
            .await
            .expect("create SqliteGitStore");
        let redb_store = RedbGitStore::create(&redb_path).expect("create RedbGitStore");

        let backends: [Arc<dyn GitStore>; 2] =
            [Arc::new(sqlite_store.clone()), Arc::new(redb_store.clone())];
        assert_eq!(backends[0].backend(), StorageBackendKind::Sqlite);
        assert_eq!(backends[1].backend(), StorageBackendKind::Redb);

        // ---------------------------------------------------------------------
        // AC1 & AC2: Atomic CAS create/update + reflog append & CasConflict
        // zero-orphan-reflog guarantee.
        // ---------------------------------------------------------------------
        let main_branch = RefKey::LocalBranch {
            name: "main".to_string(),
        };
        let c1 = RefTarget::Direct(oid(1));
        let c2 = RefTarget::Direct(oid(2));
        let c3 = RefTarget::Direct(oid(3));
        let c99 = RefTarget::Direct(oid(99));

        // Initial CAS creation on absent branch + reflog.
        let create_log = make_reflog("refs/heads/main", 0, 1, "commit", "initial commit", None);
        for store in &backends {
            let outcome = store
                .update_ref_cas(
                    &main_branch,
                    None,
                    Some(&c1),
                    std::slice::from_ref(&create_log),
                )
                .await
                .expect("AC1: initial CAS create");
            assert_eq!(
                outcome,
                RefCasOutcome::Applied {
                    previous: None,
                    current: Some(c1.clone()),
                }
            );
        }

        // AC2: Mismatched expected old value -> CasConflict, ZERO orphan reflog row!
        let conflict_log = make_reflog("refs/heads/main", 99, 2, "commit", "stale update", None);
        for store in &backends {
            let outcome = store
                .update_ref_cas(
                    &main_branch,
                    Some(&c99),
                    Some(&c2),
                    std::slice::from_ref(&conflict_log),
                )
                .await
                .expect("AC2: CAS mismatch returns Ok(CasConflict)");
            assert_eq!(
                outcome,
                RefCasOutcome::CasConflict {
                    actual: Some(c1.clone()),
                }
            );
            assert_eq!(
                store.get_ref(&main_branch).await.expect("get main branch"),
                Some(c1.clone())
            );
            let logs = store
                .list_reflog("refs/heads/main", None, None)
                .await
                .expect("list reflog after CasConflict");
            assert_eq!(
                logs.len(),
                1,
                "AC2: CasConflict must produce zero orphan reflog rows"
            );
            assert_eq!(logs[0].new_oid, oid(1));
        }

        // AC1: Matching expected old value -> advances to c2 and appends reflog #2.
        let advance_log = make_reflog("refs/heads/main", 1, 2, "commit", "second commit", None);
        for store in &backends {
            let outcome = store
                .update_ref_cas(
                    &main_branch,
                    Some(&c1),
                    Some(&c2),
                    std::slice::from_ref(&advance_log),
                )
                .await
                .expect("AC1: matching CAS update");
            assert_eq!(
                outcome,
                RefCasOutcome::Applied {
                    previous: Some(c1.clone()),
                    current: Some(c2.clone()),
                }
            );
        }
        assert_eq!(
            backends[0]
                .list_reflog("refs/heads/main", None, None)
                .await
                .expect("sqlite reflog")
                .iter()
                .map(|e| (&e.old_oid, &e.new_oid, &e.message))
                .collect::<Vec<_>>(),
            backends[1]
                .list_reflog("refs/heads/main", None, None)
                .await
                .expect("redb reflog")
                .iter()
                .map(|e| (&e.old_oid, &e.new_oid, &e.message))
                .collect::<Vec<_>>(),
        );
        // Verify newest-first ordering (`HEAD@{0}` is index 0).
        for store in &backends {
            let logs = store
                .list_reflog("refs/heads/main", None, Some(1))
                .await
                .expect("limit 1 reflog");
            assert_eq!(logs.len(), 1);
            assert_eq!(logs[0].new_oid, oid(2));
        }

        // ---------------------------------------------------------------------
        // AC4 & AC5: Main vs linked worktree local HEAD isolation + shared refs
        // visibility across both backends.
        // ---------------------------------------------------------------------
        let main_head = RefKey::LocalHead { worktree_id: None };
        let wt1_head = RefKey::LocalHead {
            worktree_id: Some("wt-alpha".to_string()),
        };
        let wt2_head = RefKey::LocalHead {
            worktree_id: Some("wt-beta".to_string()),
        };
        let feature_branch = RefKey::LocalBranch {
            name: "feature/worktree".to_string(),
        };
        let remote_branch = RefKey::RemoteBranch {
            remote: "origin".to_string(),
            name: "main".to_string(),
        };
        let remote_head = RefKey::RemoteHead {
            remote: "origin".to_string(),
        };
        let release_tag = RefKey::Tag {
            name: "v1.0.0".to_string(),
        };

        let main_sym = RefTarget::Symbolic("main".to_string());
        let wt1_sym = RefTarget::Symbolic("feature/worktree".to_string());
        let wt2_detached = RefTarget::Direct(oid(2));

        let main_head_log = make_reflog("HEAD", 0, 2, "checkout", "main checkout", None);
        let wt1_head_log = make_reflog(
            "HEAD",
            0,
            3,
            "checkout",
            "wt-alpha checkout",
            Some("wt-alpha"),
        );
        let wt2_head_log = make_reflog("HEAD", 0, 2, "checkout", "wt-beta detach", Some("wt-beta"));

        for store in &backends {
            store
                .update_ref_cas(&feature_branch, None, Some(&c3), &[])
                .await
                .expect("create feature branch");
            store
                .update_ref_cas(&remote_branch, None, Some(&c2), &[])
                .await
                .expect("create remote branch");
            store
                .update_ref_cas(&remote_head, None, Some(&main_sym), &[])
                .await
                .expect("create remote head");
            store
                .update_ref_cas(&release_tag, None, Some(&c1), &[])
                .await
                .expect("create tag");

            store
                .update_ref_cas(
                    &main_head,
                    None,
                    Some(&main_sym),
                    std::slice::from_ref(&main_head_log),
                )
                .await
                .expect("create main HEAD");
            store
                .update_ref_cas(
                    &wt1_head,
                    None,
                    Some(&wt1_sym),
                    std::slice::from_ref(&wt1_head_log),
                )
                .await
                .expect("create wt-alpha HEAD");
            store
                .update_ref_cas(
                    &wt2_head,
                    None,
                    Some(&wt2_detached),
                    std::slice::from_ref(&wt2_head_log),
                )
                .await
                .expect("create wt-beta detached HEAD");
        }

        // Duplicate symbolic branch checkout across two worktrees must fail closed on BOTH backends!
        for store in &backends {
            let dup_err = store
                .update_ref_cas(&wt2_head, Some(&wt2_detached), Some(&main_sym), &[])
                .await
                .expect_err("two worktrees must not hold symbolic HEAD to the same branch");
            assert_eq!(dup_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                store.get_ref(&wt2_head).await.expect("wt2 unchanged"),
                Some(wt2_detached.clone())
            );
        }

        // Verify AC4 (strict per-worktree HEAD & HEAD-reflog isolation) + AC5 (shared refs visible identically).
        for scope in [
            None,
            Some("wt-alpha"),
            Some("wt-beta"),
            Some("wt-uninitialized"),
        ] {
            let sq_refs = backends[0]
                .list_refs(scope)
                .await
                .expect("sqlite list_refs");
            let rb_refs = backends[1].list_refs(scope).await.expect("redb list_refs");
            assert_eq!(
                sq_refs, rb_refs,
                "AC4/AC5: list_refs({scope:?}) must match identically across SQLite and Redb"
            );

            // Count LocalHead rows returned: 1 for initialized scopes, 0 for wt-uninitialized.
            let local_heads: Vec<_> = sq_refs
                .iter()
                .filter(|rec| rec.key.is_worktree_local())
                .collect();
            if scope == Some("wt-uninitialized") {
                assert!(local_heads.is_empty());
            } else {
                assert_eq!(local_heads.len(), 1);
                assert_eq!(local_heads[0].key.worktree_id(), scope);
            }

            // All 5 shared refs (main, feature/worktree, origin/main, origin/HEAD, v1.0.0) are visible!
            let shared_keys: Vec<_> = sq_refs
                .iter()
                .filter(|rec| !rec.key.is_worktree_local())
                .map(|rec| rec.key.clone())
                .collect();
            assert_eq!(
                shared_keys,
                vec![
                    remote_head.clone(),
                    feature_branch.clone(),
                    main_branch.clone(),
                    remote_branch.clone(),
                    release_tag.clone(),
                ],
                "AC5: shared refs must be identically visible to {scope:?}"
            );

            // HEAD reflog strictly isolated per worktree.
            let sq_hlog = backends[0]
                .list_reflog("HEAD", scope, None)
                .await
                .expect("sqlite HEAD reflog");
            let rb_hlog = backends[1]
                .list_reflog("HEAD", scope, None)
                .await
                .expect("redb HEAD reflog");
            assert_eq!(
                sq_hlog
                    .iter()
                    .map(|e| (&e.message, &e.worktree_id))
                    .collect::<Vec<_>>(),
                rb_hlog
                    .iter()
                    .map(|e| (&e.message, &e.worktree_id))
                    .collect::<Vec<_>>()
            );
            match scope {
                None => {
                    assert_eq!(sq_hlog.len(), 1);
                    assert_eq!(sq_hlog[0].message, "main checkout");
                }
                Some("wt-alpha") => {
                    assert_eq!(sq_hlog.len(), 1);
                    assert_eq!(sq_hlog[0].message, "wt-alpha checkout");
                }
                Some("wt-beta") => {
                    assert_eq!(sq_hlog.len(), 1);
                    assert_eq!(sq_hlog[0].message, "wt-beta detach");
                }
                _ => assert!(sq_hlog.is_empty()),
            }
        }

        // `list_all_local_heads` returns all 3 worktree HEADs identically on both backends.
        assert_eq!(
            backends[0]
                .list_all_local_heads()
                .await
                .expect("sqlite all local heads"),
            backends[1]
                .list_all_local_heads()
                .await
                .expect("redb all local heads")
        );

        // ---------------------------------------------------------------------
        // AC3: `update_ref_cas_with_bridge_publication` atomic commit AND
        // zero-orphan rollback across both backends (plus injected redb abort).
        // ---------------------------------------------------------------------
        let c4 = RefTarget::Direct(oid(4));
        let c5 = RefTarget::Direct(oid(5));
        let c6 = RefTarget::Direct(oid(6));
        let pub_op1 = BridgePublicationRecord {
            operation_id: "op-bridge-001".to_string(),
            commit_oid: oid(4),
            phase: BridgePublicationPhase::Published,
        };
        let head_move_log = make_reflog("HEAD", 2, 4, "commit", "bridge commit 1", Some("wt-beta"));

        for store in &backends {
            // Prepare first, then promote to Published atomically with HEAD move.
            store
                .prepare_bridge_publication("op-bridge-001", &oid(4))
                .await
                .expect("prepare op-bridge-001");
            assert_eq!(
                store
                    .get_bridge_publication("op-bridge-001")
                    .await
                    .expect("get prepared"),
                Some(BridgePublicationRecord {
                    operation_id: "op-bridge-001".to_string(),
                    commit_oid: oid(4),
                    phase: BridgePublicationPhase::Prepared,
                })
            );

            // 1. CAS mismatch must NOT promote/insert publication OR append reflog!
            let pub_op_stale = BridgePublicationRecord {
                operation_id: "op-stale-never-committed".to_string(),
                commit_oid: oid(5),
                phase: BridgePublicationPhase::Published,
            };
            let stale_log = make_reflog("HEAD", 99, 5, "commit", "stale bridge", Some("wt-beta"));
            let cas_res = store
                .update_ref_cas_with_bridge_publication(
                    &wt2_head,
                    Some(&c99),
                    Some(&c5),
                    &[stale_log],
                    &pub_op_stale,
                )
                .await
                .expect("AC3: CAS conflict on bridge publication");
            assert_eq!(
                cas_res,
                RefCasOutcome::CasConflict {
                    actual: Some(wt2_detached.clone()),
                }
            );
            assert_eq!(
                store
                    .get_bridge_publication("op-stale-never-committed")
                    .await
                    .expect("stale publication absent"),
                None
            );
            assert_eq!(
                store
                    .list_reflog("HEAD", Some("wt-beta"), None)
                    .await
                    .expect("wt-beta reflog unchanged")
                    .len(),
                1
            );

            // 2. Valid CAS + matching prepared op-bridge-001 -> commits atomically!
            let ok_res = store
                .update_ref_cas_with_bridge_publication(
                    &wt2_head,
                    Some(&wt2_detached),
                    Some(&c4),
                    std::slice::from_ref(&head_move_log),
                    &pub_op1,
                )
                .await
                .expect("AC3: atomic bridge publication commit");
            assert_eq!(
                ok_res,
                RefCasOutcome::Applied {
                    previous: Some(wt2_detached.clone()),
                    current: Some(c4.clone()),
                }
            );
            assert_eq!(
                store.get_ref(&wt2_head).await.expect("wt2 moved to c4"),
                Some(c4.clone())
            );
            assert_eq!(
                store
                    .get_bridge_publication("op-bridge-001")
                    .await
                    .expect("op-bridge-001 published"),
                Some(pub_op1.clone())
            );
            assert_eq!(
                store
                    .list_reflog("HEAD", Some("wt-beta"), None)
                    .await
                    .expect("wt-beta reflog has 2 rows")
                    .len(),
                2
            );

            // 3. Reusing already-bound `op-bridge-001` for a DIFFERENT commit_oid (c5)
            //    must roll back the ENTIRE transaction: HEAD stays at c4, reflog stays 2,
            //    publication stays bound to oid(4)!
            let conflicting_pub = BridgePublicationRecord {
                operation_id: "op-bridge-001".to_string(),
                commit_oid: oid(5),
                phase: BridgePublicationPhase::Published,
            };
            let rollback_log =
                make_reflog("HEAD", 4, 5, "commit", "must rollback", Some("wt-beta"));
            let err = store
                .update_ref_cas_with_bridge_publication(
                    &wt2_head,
                    Some(&c4),
                    Some(&c5),
                    &[rollback_log],
                    &conflicting_pub,
                )
                .await
                .expect_err("AC3: conflicting bridge operation_id must abort entire transaction");
            assert_eq!(err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                store
                    .get_ref(&wt2_head)
                    .await
                    .expect("HEAD unchanged after rollback"),
                Some(c4.clone())
            );
            assert_eq!(
                store
                    .list_reflog("HEAD", Some("wt-beta"), None)
                    .await
                    .expect("reflog unchanged after rollback")
                    .len(),
                2
            );
            assert_eq!(
                store
                    .get_bridge_publication("op-bridge-001")
                    .await
                    .expect("publication unchanged after rollback"),
                Some(pub_op1.clone())
            );

            // 4. Bridge publication binding rejections (P1 #1):
            //    (a) commit_oid mismatch between ref target and publication;
            //    (b) non-`Published` phase (`Prepared`);
            //    (c) non-HEAD / unchecked-out ref (`Tag`, `RemoteBranch`, unchecked-out `LocalBranch`);
            //    (d) unchanged/deleted ref (`expected == new_target`, `new_target == None`).
            let mismatched_oid_pub = BridgePublicationRecord {
                operation_id: "op-mismatch-oid".to_string(),
                commit_oid: oid(7),
                phase: BridgePublicationPhase::Published,
            };
            assert!(
                store
                    .update_ref_cas_with_bridge_publication(
                        &wt2_head,
                        Some(&c4),
                        Some(&c5),
                        &[],
                        &mismatched_oid_pub,
                    )
                    .await
                    .is_err(),
                "mismatched publication commit_oid must be rejected"
            );
            assert_eq!(
                store
                    .get_bridge_publication("op-mismatch-oid")
                    .await
                    .expect("get op-mismatch-oid"),
                None
            );

            let wrong_phase_pub = BridgePublicationRecord {
                operation_id: "op-wrong-phase".to_string(),
                commit_oid: oid(5),
                phase: BridgePublicationPhase::Prepared,
            };
            assert!(
                store
                    .update_ref_cas_with_bridge_publication(
                        &wt2_head,
                        Some(&c4),
                        Some(&c5),
                        &[],
                        &wrong_phase_pub,
                    )
                    .await
                    .is_err(),
                "phase=Prepared must be rejected on publish"
            );
            assert_eq!(
                store
                    .get_bridge_publication("op-wrong-phase")
                    .await
                    .expect("get op-wrong-phase"),
                None
            );

            let unrelated_tag_pub = BridgePublicationRecord {
                operation_id: "op-unrelated-tag".to_string(),
                commit_oid: oid(5),
                phase: BridgePublicationPhase::Published,
            };
            assert!(
                store
                    .update_ref_cas_with_bridge_publication(
                        &release_tag,
                        Some(&c1),
                        Some(&c5),
                        &[],
                        &unrelated_tag_pub,
                    )
                    .await
                    .is_err(),
                "binding a bridge publication to a Tag must be rejected"
            );
            assert_eq!(
                store
                    .get_bridge_publication("op-unrelated-tag")
                    .await
                    .expect("get op-unrelated-tag"),
                None
            );

            // Unchecked-out LocalBranch (no HEAD points to `unchecked-out`) must be rejected.
            let unchecked_branch = RefKey::LocalBranch {
                name: "unchecked-out".to_string(),
            };
            let unchecked_pub = BridgePublicationRecord {
                operation_id: "op-unchecked-branch".to_string(),
                commit_oid: oid(5),
                phase: BridgePublicationPhase::Published,
            };
            assert!(
                store
                    .update_ref_cas_with_bridge_publication(
                        &unchecked_branch,
                        None,
                        Some(&c5),
                        &[],
                        &unchecked_pub,
                    )
                    .await
                    .is_err(),
                "binding a bridge publication to an unchecked-out branch must be rejected"
            );
            assert_eq!(
                store
                    .get_ref(&unchecked_branch)
                    .await
                    .expect("unchecked branch remains absent"),
                None
            );

            // No-op transition (`expected == new_target`) must be rejected.
            let noop_pub = BridgePublicationRecord {
                operation_id: "op-noop".to_string(),
                commit_oid: oid(4),
                phase: BridgePublicationPhase::Published,
            };
            assert!(
                store
                    .update_ref_cas_with_bridge_publication(
                        &wt2_head,
                        Some(&c4),
                        Some(&c4),
                        &[],
                        &noop_pub,
                    )
                    .await
                    .is_err(),
                "no-op ref update must be rejected for bridge publication"
            );
            assert_eq!(
                store
                    .get_bridge_publication("op-noop")
                    .await
                    .expect("get op-noop"),
                None
            );

            // 5. Symbolic HEAD branch commit + bridge publication succeeds atomically
            //    when `main` IS checked out by main worktree's `LocalHead`!
            let c8 = RefTarget::Direct(oid(8));
            let sym_pub = BridgePublicationRecord {
                operation_id: "op-bridge-symbolic-main".to_string(),
                commit_oid: oid(8),
                phase: BridgePublicationPhase::Published,
            };
            let main_branch_log =
                make_reflog("refs/heads/main", 2, 8, "commit", "sym bridge branch", None);
            let main_sym_head_log = make_reflog("HEAD", 2, 8, "commit", "sym bridge head", None);
            let sym_res = store
                .update_ref_cas_with_bridge_publication(
                    &main_branch,
                    Some(&c2),
                    Some(&c8),
                    &[main_branch_log, main_sym_head_log],
                    &sym_pub,
                )
                .await
                .expect("symbolic HEAD checked-out branch bridge publication must succeed");
            assert_eq!(
                sym_res,
                RefCasOutcome::Applied {
                    previous: Some(c2.clone()),
                    current: Some(c8.clone()),
                }
            );
            assert_eq!(
                store
                    .get_bridge_publication("op-bridge-symbolic-main")
                    .await
                    .expect("get op-bridge-symbolic-main"),
                Some(sym_pub)
            );

            // Detaching symbolic HEAD (`main` -> oid(8)) directly to the SAME resolved
            // commit `Direct(oid(8))` does NOT move HEAD to a new commit and MUST be
            // rejected for bridge publication on both backends!
            let same_resolved_pub = BridgePublicationRecord {
                operation_id: "op-same-resolved-oid".to_string(),
                commit_oid: oid(8),
                phase: BridgePublicationPhase::Published,
            };
            assert!(
                store
                    .update_ref_cas_with_bridge_publication(
                        &main_head,
                        Some(&main_sym),
                        Some(&c8),
                        &[],
                        &same_resolved_pub,
                    )
                    .await
                    .is_err(),
                "detaching symbolic HEAD onto the exact same resolved commit OID must be rejected for bridge publication"
            );
            assert_eq!(
                store
                    .get_ref(&main_head)
                    .await
                    .expect("main HEAD remains symbolic main"),
                Some(main_sym.clone())
            );
            assert_eq!(
                store
                    .get_bridge_publication("op-same-resolved-oid")
                    .await
                    .expect("get op-same-resolved-oid"),
                None
            );

            // 6. Cross-worktree HEAD reflog boundary rejection (P1 #2):
            //    - Main LocalHead update with linked worktree HEAD reflog -> rejected!
            //    - Linked LocalHead update with main (None) or other linked worktree HEAD reflog -> rejected!
            //    - Branch update with HEAD reflog for a worktree NOT checked out on that branch -> rejected!
            let cross_main_to_linked = make_reflog(
                "HEAD",
                8,
                6,
                "checkout",
                "illegal main->wt-beta log",
                Some("wt-beta"),
            );
            assert!(
                store
                    .update_ref_cas(
                        &main_head,
                        Some(&main_sym),
                        Some(&c6),
                        &[cross_main_to_linked],
                    )
                    .await
                    .is_err(),
                "main LocalHead update must reject linked-worktree HEAD reflog"
            );
            let cross_linked_to_main =
                make_reflog("HEAD", 4, 6, "checkout", "illegal wt-beta->main log", None);
            assert!(
                store
                    .update_ref_cas(&wt2_head, Some(&c4), Some(&c6), &[cross_linked_to_main],)
                    .await
                    .is_err(),
                "linked LocalHead update must reject main-worktree HEAD reflog"
            );
            let cross_linked_to_other = make_reflog(
                "HEAD",
                4,
                6,
                "checkout",
                "illegal wt-beta->wt-alpha log",
                Some("wt-alpha"),
            );
            assert!(
                store
                    .update_ref_cas(&wt2_head, Some(&c4), Some(&c6), &[cross_linked_to_other],)
                    .await
                    .is_err(),
                "linked LocalHead update must reject another linked-worktree HEAD reflog"
            );
            let wrong_wt_branch_head_log = make_reflog(
                "HEAD",
                8,
                6,
                "commit",
                "wt-alpha does not have main checked out",
                Some("wt-alpha"),
            );
            assert!(
                store
                    .update_ref_cas(
                        &main_branch,
                        Some(&c8),
                        Some(&c6),
                        &[wrong_wt_branch_head_log],
                    )
                    .await
                    .is_err(),
                "LocalBranch update must reject HEAD reflog for worktree not checked out on that branch"
            );
            // Confirm none of the rejected cross-worktree attempts mutated refs or reflogs.
            assert_eq!(
                store.get_ref(&main_head).await.expect("main HEAD intact"),
                Some(main_sym.clone())
            );
            assert_eq!(
                store.get_ref(&wt2_head).await.expect("wt-beta HEAD intact"),
                Some(c4.clone())
            );
            assert_eq!(
                store
                    .list_reflog("HEAD", None, None)
                    .await
                    .expect("main HEAD reflog intact")
                    .len(),
                2
            );
            assert_eq!(
                store
                    .list_reflog("HEAD", Some("wt-alpha"), None)
                    .await
                    .expect("wt-alpha HEAD reflog intact")
                    .len(),
                1
            );
            assert_eq!(
                store
                    .list_reflog("HEAD", Some("wt-beta"), None)
                    .await
                    .expect("wt-beta HEAD reflog intact")
                    .len(),
                2
            );
        }

        // 4. Explicit injected pre-commit rollback inside `RedbGitStore`:
        //    proves staged HEAD move + reflog + new bridge publication row are
        //    all atomically discarded when `WriteTransaction` aborts before commit.
        let aborted_pub = BridgePublicationRecord {
            operation_id: "op-bridge-aborted".to_string(),
            commit_oid: oid(6),
            phase: BridgePublicationPhase::Published,
        };
        let aborted_log = make_reflog("HEAD", 4, 6, "commit", "aborted tx", Some("wt-beta"));
        let abort_err = redb_store
            .update_ref_cas_with_bridge_publication_abort_before_commit_for_test(
                &wt2_head,
                Some(&c4),
                Some(&c6),
                &[aborted_log],
                &aborted_pub,
            )
            .await
            .expect_err("injected pre-commit abort must return Err");
        assert_eq!(abort_err.stable_code_str(), "LBR-IO-001");
        assert_eq!(
            redb_store
                .get_ref(&wt2_head)
                .await
                .expect("redb HEAD untouched after pre-commit abort"),
            Some(c4.clone())
        );
        assert_eq!(
            redb_store
                .list_reflog("HEAD", Some("wt-beta"), None)
                .await
                .expect("redb reflog untouched after pre-commit abort")
                .len(),
            2
        );
        assert_eq!(
            redb_store
                .get_bridge_publication("op-bridge-aborted")
                .await
                .expect("redb publication absent after pre-commit abort"),
            None
        );

        // Final bridge publications list matches across both backends.
        assert_eq!(
            backends[0]
                .list_bridge_publications()
                .await
                .expect("sqlite publications"),
            backends[1]
                .list_bridge_publications()
                .await
                .expect("redb publications")
        );

        // Also verify strict read-only handles on both backends see identical state and reject mutations.
        let sq_ro = SqliteGitStore::open_read_only(&sqlite_path)
            .await
            .expect("open SqliteGitStore read-only");
        let rb_ro = RedbGitStore::open_read_only(&redb_path).expect("open RedbGitStore read-only");
        assert_eq!(
            sq_ro.list_refs(None).await.expect("sqlite ro list_refs"),
            rb_ro.list_refs(None).await.expect("redb ro list_refs")
        );
        assert!(
            sq_ro
                .update_ref_cas(&main_branch, Some(&c2), Some(&c3), &[])
                .await
                .is_err()
        );
        assert!(
            rb_ro
                .update_ref_cas(&main_branch, Some(&c2), Some(&c3), &[])
                .await
                .is_err()
        );
    }

    /// `RB-04` (AC6): Real 12-process concurrent stress test against a single
    /// `git.redb` file: 4 independent writer OS processes looping
    /// `update_ref_cas` (20 successful linearizable CAS increments each = 80
    /// total) + 8 independent `open_read_only` reader OS processes continuously
    /// calling `list_refs`/`get_ref`/`list_reflog`, proving strict linearizability,
    /// MVCC non-blocking reads, zero lock errors, and zero orphan reflog rows.
    #[cfg(feature = "storage-redb")]
    #[tokio::test]
    async fn redb_multiprocess_cas_linearizability_and_mvcc_reads() {
        use std::{
            collections::BTreeSet,
            env, fs,
            path::PathBuf,
            process::{Command, Stdio},
            time::{Duration, Instant},
        };

        use tempfile::tempdir;

        const WRITERS: usize = 4;
        const READERS: usize = 8;
        const MIN_INCREMENTS_PER_WRITER: u64 = 20;
        const MAX_INCREMENTS_PER_WRITER: u64 = 2_000;
        const MIN_DISTINCT_LIVE_VALUES: usize = 3;
        const MIN_OVERLAP_READS: u64 = 20;

        fn parse_counter_oid(raw: &str) -> u64 {
            u64::from_str_radix(raw, 16).expect("counter OID must be valid hex u64")
        }

        // Child worker entrypoint (writer or reader).
        if let Ok(role) = env::var("LIBRA_RB04_CHILD_ROLE") {
            let db_path = PathBuf::from(env::var("LIBRA_RB04_DB_PATH").expect("RB04 DB path"));
            let sync_dir = PathBuf::from(env::var("LIBRA_RB04_SYNC_DIR").expect("RB04 sync dir"));
            let worker_id: usize = env::var("LIBRA_RB04_WORKER_ID")
                .expect("RB04 worker id")
                .parse()
                .expect("parse worker id");

            let go_flag = sync_dir.join("go.flag");
            let readers_overlap_done_flag = sync_dir.join("readers-overlap-done.flag");
            let done_flag = sync_dir.join("done.flag");
            let counter_key = RefKey::LocalBranch {
                name: "counter".to_string(),
            };

            match role.as_str() {
                "writer" => {
                    let store = RedbGitStore::open_read_write(&db_path)
                        .expect("child writer open RedbGitStore")
                        .with_write_timeout(Duration::from_secs(30));
                    assert!(
                        store
                            .get_ref(&counter_key)
                            .await
                            .expect("writer warmup get_ref")
                            .is_some()
                    );
                    fs::write(
                        sync_dir.join(format!("ready-writer-{worker_id}.flag")),
                        b"ready",
                    )
                    .expect("signal writer ready");

                    let barrier_deadline = Instant::now() + Duration::from_secs(30);
                    while !go_flag.exists() {
                        assert!(
                            Instant::now() < barrier_deadline,
                            "writer {worker_id} timed out waiting for go.flag"
                        );
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }

                    let mut applied = 0u64;
                    let mut conflicts = 0u64;
                    // Continuously commit without pausing until BOTH:
                    // 1. this writer has applied >= MIN_INCREMENTS_PER_WRITER CAS updates, AND
                    // 2. all 8 concurrent readers have observed >= 3 distinct live advancing commits.
                    while applied < MIN_INCREMENTS_PER_WRITER
                        || (!readers_overlap_done_flag.exists()
                            && applied < MAX_INCREMENTS_PER_WRITER)
                    {
                        let current = store
                            .get_ref(&counter_key)
                            .await
                            .expect("writer get_ref must not hit lock error")
                            .expect("counter branch must exist");
                        let cur_n = match &current {
                            RefTarget::Direct(hex) => parse_counter_oid(hex),
                            RefTarget::Symbolic(_) => panic!("counter branch must be direct"),
                        };
                        let next_n = cur_n + 1;
                        let next_target = RefTarget::Direct(oid(next_n));
                        let log = make_reflog(
                            "refs/heads/counter",
                            cur_n,
                            next_n,
                            "update-ref",
                            &format!("writer-{worker_id} step {cur_n}->{next_n}"),
                            None,
                        );
                        match store
                            .update_ref_cas(
                                &counter_key,
                                Some(&current),
                                Some(&next_target),
                                &[log],
                            )
                            .await
                            .expect("writer update_ref_cas must not return lock error")
                        {
                            RefCasOutcome::Applied { .. } => {
                                applied += 1;
                            }
                            RefCasOutcome::CasConflict { .. } => {
                                conflicts += 1;
                            }
                        }
                    }
                    assert!(
                        readers_overlap_done_flag.exists(),
                        "writer {worker_id} hit MAX_INCREMENTS_PER_WRITER before readers finished live overlap observation"
                    );
                    println!(
                        "RB04_WRITER_OK id={worker_id} applied={applied} conflicts={conflicts}"
                    );
                }
                "reader" => {
                    let store = RedbGitStore::open_read_only(&db_path)
                        .expect("child reader open RedbGitStore read-only");
                    assert!(
                        store
                            .get_ref(&counter_key)
                            .await
                            .expect("reader warmup get_ref")
                            .is_some()
                    );
                    fs::write(
                        sync_dir.join(format!("ready-reader-{worker_id}.flag")),
                        b"ready",
                    )
                    .expect("signal reader ready");

                    let barrier_deadline = Instant::now() + Duration::from_secs(30);
                    while !go_flag.exists() {
                        assert!(
                            Instant::now() < barrier_deadline,
                            "reader {worker_id} timed out waiting for go.flag"
                        );
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }

                    let mut last_seen = 0u64;
                    let mut reads_during_writes = 0u64;
                    let mut distinct_live_values = BTreeSet::new();
                    let mut overlap_signaled = false;
                    let deadline = Instant::now() + Duration::from_secs(60);
                    loop {
                        assert!(
                            Instant::now() < deadline,
                            "reader {worker_id} timed out waiting for done.flag (reads_during_writes={reads_during_writes}, distinct_live={}, last_seen={last_seen})",
                            distinct_live_values.len()
                        );
                        let done_snapshot = fs::read_to_string(&done_flag).ok();

                        let refs = store
                            .list_refs(None)
                            .await
                            .expect("reader list_refs must not fail under write contention");
                        let counter_from_list = refs
                            .iter()
                            .find(|r| r.key == counter_key)
                            .expect("counter ref must always be visible in list_refs");
                        let list_n = match &counter_from_list.target {
                            RefTarget::Direct(hex) => parse_counter_oid(hex),
                            RefTarget::Symbolic(_) => panic!("counter must be direct"),
                        };
                        assert!(
                            list_n >= last_seen,
                            "non-monotonic counter observed in list_refs: prev={last_seen}, now={list_n}"
                        );
                        last_seen = list_n;

                        let got = store
                            .get_ref(&counter_key)
                            .await
                            .expect("reader get_ref must not fail under write contention")
                            .expect("counter ref must exist");
                        let get_n = match &got {
                            RefTarget::Direct(hex) => parse_counter_oid(hex),
                            RefTarget::Symbolic(_) => panic!("counter must be direct"),
                        };
                        assert!(
                            get_n >= last_seen,
                            "non-monotonic counter observed in get_ref: prev={last_seen}, now={get_n}"
                        );
                        last_seen = get_n;

                        let logs = store
                            .list_reflog("refs/heads/counter", None, Some(1))
                            .await
                            .expect("reader list_reflog must not fail");
                        if let Some(top) = logs.first() {
                            let top_n = parse_counter_oid(&top.new_oid);
                            assert!(
                                top_n >= last_seen,
                                "reflog head regressed: last_seen={last_seen}, top={top_n}"
                            );
                            last_seen = top_n;
                        }

                        if let Some(expected_final_str) = done_snapshot {
                            let expected_final: u64 = expected_final_str
                                .trim()
                                .parse()
                                .expect("parse expected final total from done.flag");
                            assert_eq!(
                                last_seen, expected_final,
                                "reader {worker_id} must observe exact converged counter {expected_final} after done.flag"
                            );
                            break;
                        }

                        // Count ONLY reads performed after writers have begun committing (`last_seen > 0`).
                        if last_seen > 0 {
                            reads_during_writes += 1;
                            distinct_live_values.insert(last_seen);
                            if !overlap_signaled
                                && reads_during_writes >= MIN_OVERLAP_READS
                                && distinct_live_values.len() >= MIN_DISTINCT_LIVE_VALUES
                            {
                                overlap_signaled = true;
                                fs::write(
                                    sync_dir.join(format!("reader-overlap-{worker_id}.flag")),
                                    b"overlap",
                                )
                                .expect("signal reader observed live advancing writes");
                            }
                        }
                    }
                    assert!(
                        overlap_signaled
                            && reads_during_writes >= MIN_OVERLAP_READS
                            && distinct_live_values.len() >= MIN_DISTINCT_LIVE_VALUES,
                        "reader {worker_id} must observe >= {MIN_DISTINCT_LIVE_VALUES} live advancing values and >= {MIN_OVERLAP_READS} reads during active writes"
                    );
                    println!(
                        "RB04_READER_OK id={worker_id} reads_during_writes={reads_during_writes} distinct_live={} last_seen={last_seen}",
                        distinct_live_values.len()
                    );
                }
                other => panic!("unexpected RB04 child role: {other}"),
            }
            return;
        }

        // Parent orchestrator.
        let dir = tempdir().expect("create RB-04 multiprocess tempdir");
        let db_path = dir.path().join("git.redb");
        let sync_dir = dir.path().join("sync");
        fs::create_dir_all(&sync_dir).expect("create sync dir");
        let go_flag = sync_dir.join("go.flag");
        let readers_overlap_done_flag = sync_dir.join("readers-overlap-done.flag");
        let done_flag = sync_dir.join("done.flag");

        {
            let store = RedbGitStore::create(&db_path).expect("create shared git.redb");
            store
                .update_ref_cas(
                    &RefKey::LocalHead { worktree_id: None },
                    None,
                    Some(&RefTarget::Symbolic("counter".to_string())),
                    &[],
                )
                .await
                .expect("seed main HEAD");
            store
                .update_ref_cas(
                    &RefKey::LocalHead {
                        worktree_id: Some("wt-stress".to_string()),
                    },
                    None,
                    Some(&RefTarget::Direct(oid(0))),
                    &[],
                )
                .await
                .expect("seed linked worktree HEAD");
            store
                .update_ref_cas(
                    &RefKey::LocalBranch {
                        name: "counter".to_string(),
                    },
                    None,
                    Some(&RefTarget::Direct(oid(0))),
                    &[],
                )
                .await
                .expect("seed counter branch at 0");
        }

        let exe = env::current_exe().expect("locate current test executable");
        let spawn_worker = |role: &str, id: usize| {
            Command::new(&exe)
                .arg("--exact")
                .arg("storage::git_store::tests::redb_multiprocess_cas_linearizability_and_mvcc_reads")
                .arg("--nocapture")
                .env("LIBRA_RB04_CHILD_ROLE", role)
                .env("LIBRA_RB04_DB_PATH", &db_path)
                .env("LIBRA_RB04_SYNC_DIR", &sync_dir)
                .env("LIBRA_RB04_WORKER_ID", id.to_string())
                .env("LIBRA_TEST", "1")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn RB-04 worker child")
        };

        let mut writers = Vec::with_capacity(WRITERS);
        for id in 0..WRITERS {
            writers.push(spawn_worker("writer", id));
        }
        let mut readers = Vec::with_capacity(READERS);
        for id in 0..READERS {
            readers.push(spawn_worker("reader", id));
        }

        // Phase 1: Wait until ALL 12 processes (4 writers + 8 readers) have opened
        // `git.redb` and signaled readiness before releasing `go.flag`.
        let ready_deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let writers_ready =
                (0..WRITERS).all(|id| sync_dir.join(format!("ready-writer-{id}.flag")).exists());
            let readers_ready =
                (0..READERS).all(|id| sync_dir.join(format!("ready-reader-{id}.flag")).exists());
            if writers_ready && readers_ready {
                break;
            }
            assert!(
                Instant::now() < ready_deadline,
                "timed out waiting for all 12 RB-04 workers to open git.redb"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        fs::write(&go_flag, b"go").expect("write go flag");

        // Phase 2: While all 4 writers are actively committing without pausing,
        // wait until ALL 8 readers have observed >= 3 distinct live advancing
        // values and >= 20 active reads, then let writers finish cleanly.
        let overlap_deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let readers_overlapped =
                (0..READERS).all(|id| sync_dir.join(format!("reader-overlap-{id}.flag")).exists());
            if readers_overlapped {
                break;
            }
            assert!(
                Instant::now() < overlap_deadline,
                "timed out waiting for all 8 readers to observe live advancing writes"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        fs::write(&readers_overlap_done_flag, b"overlap-done")
            .expect("signal writers that reader overlap is verified");

        let mut total_applied = 0u64;
        for (id, child) in writers.into_iter().enumerate() {
            let output = child.wait_with_output().expect("wait for writer child");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let prefix = format!("RB04_WRITER_OK id={id} applied=");
            assert!(
                output.status.success()
                    && stdout.contains(&prefix)
                    && stdout.contains("1 passed; 0 failed"),
                "writer {id} failed (status={:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
                output.status
            );
            let applied: u64 = stdout
                .lines()
                .find_map(|line| line.strip_prefix(&prefix))
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|num| num.parse().ok())
                .expect("parse writer applied count");
            assert!(
                applied >= MIN_INCREMENTS_PER_WRITER,
                "writer {id} must apply >= {MIN_INCREMENTS_PER_WRITER} increments, got {applied}"
            );
            total_applied += applied;
        }
        assert!(
            total_applied >= (WRITERS as u64) * MIN_INCREMENTS_PER_WRITER,
            "total_applied must be >= {}",
            (WRITERS as u64) * MIN_INCREMENTS_PER_WRITER
        );

        fs::write(&done_flag, total_applied.to_string()).expect("write done flag with total");

        for (id, child) in readers.into_iter().enumerate() {
            let output = child.wait_with_output().expect("wait for reader child");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success()
                    && stdout.contains(&format!("RB04_READER_OK id={id}"))
                    && stdout.contains("1 passed; 0 failed"),
                "reader {id} failed (status={:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
                output.status
            );
        }

        // Verify strict linearizability & zero orphan reflog rows!
        let verify = RedbGitStore::open_read_only(&db_path).expect("open final git.redb RO");
        let final_val = verify
            .get_ref(&RefKey::LocalBranch {
                name: "counter".to_string(),
            })
            .await
            .expect("read final counter")
            .expect("counter exists");
        assert_eq!(
            final_val,
            RefTarget::Direct(oid(total_applied)),
            "AC6: all {total_applied} CAS increments across {WRITERS} processes must linearize with zero lost updates"
        );

        let reflog = verify
            .list_reflog("refs/heads/counter", None, None)
            .await
            .expect("read final reflog");
        assert_eq!(
            reflog.len() as u64,
            total_applied,
            "AC6: reflog must contain exactly {total_applied} entries (zero orphan rows from CAS conflicts)"
        );
        // Newest-first (`total_applied` down to 1): verify unbroken chain `0->1->2->...->total_applied`.
        for (idx, entry) in reflog.iter().enumerate() {
            let expected_new = total_applied - (idx as u64);
            let expected_old = expected_new - 1;
            assert_eq!(
                entry.old_oid,
                oid(expected_old),
                "broken linearizable chain at reflog[{idx}] old_oid"
            );
            assert_eq!(
                entry.new_oid,
                oid(expected_new),
                "broken linearizable chain at reflog[{idx}] new_oid"
            );
        }
    }

    /// `RB-04` single-feature SQLite regression smoke test (`--no-default-features
    /// --features storage-sqlite`).
    #[cfg(feature = "storage-sqlite")]
    #[tokio::test]
    async fn sqlite_core_refs_reflog_smoke() {
        use tempfile::tempdir;

        let dir = tempdir().expect("create sqlite smoke tempdir");
        let db_path = dir.path().join("libra.db");
        let store = SqliteGitStore::create(&db_path)
            .await
            .expect("create SqliteGitStore");
        assert_eq!(store.backend(), StorageBackendKind::Sqlite);

        let main_branch = RefKey::LocalBranch {
            name: "main".to_string(),
        };
        let main_head = RefKey::LocalHead { worktree_id: None };
        let linked_head = RefKey::LocalHead {
            worktree_id: Some("wt-1".to_string()),
        };

        let c1 = RefTarget::Direct(oid(1));
        let c2 = RefTarget::Direct(oid(2));

        let applied = store
            .update_ref_cas(
                &main_branch,
                None,
                Some(&c1),
                &[make_reflog("refs/heads/main", 0, 1, "commit", "init", None)],
            )
            .await
            .expect("create main branch");
        assert_eq!(
            applied,
            RefCasOutcome::Applied {
                previous: None,
                current: Some(c1.clone()),
            }
        );

        let conflict = store
            .update_ref_cas(
                &main_branch,
                Some(&c2),
                Some(&RefTarget::Direct(oid(3))),
                &[make_reflog(
                    "refs/heads/main",
                    2,
                    3,
                    "commit",
                    "conflict",
                    None,
                )],
            )
            .await
            .expect("cas conflict");
        assert_eq!(
            conflict,
            RefCasOutcome::CasConflict {
                actual: Some(c1.clone()),
            }
        );
        assert_eq!(
            store
                .list_reflog("refs/heads/main", None, None)
                .await
                .expect("reflog count")
                .len(),
            1
        );

        // Worktree-isolated HEAD + atomic bridge publication binding.
        store
            .update_ref_cas(
                &main_head,
                None,
                Some(&RefTarget::Symbolic("main".to_string())),
                &[make_reflog("HEAD", 0, 1, "checkout", "main", None)],
            )
            .await
            .expect("set main HEAD");
        let pub_rec = BridgePublicationRecord {
            operation_id: "sqlite-smoke-op".to_string(),
            commit_oid: oid(2),
            phase: BridgePublicationPhase::Published,
        };
        let pub_outcome = store
            .update_ref_cas_with_bridge_publication(
                &linked_head,
                None,
                Some(&c2),
                &[make_reflog(
                    "HEAD",
                    0,
                    2,
                    "commit",
                    "wt-1 bridge",
                    Some("wt-1"),
                )],
                &pub_rec,
            )
            .await
            .expect("atomic bridge publication");
        assert_eq!(
            pub_outcome,
            RefCasOutcome::Applied {
                previous: None,
                current: Some(c2.clone()),
            }
        );
        assert_eq!(
            store
                .get_bridge_publication("sqlite-smoke-op")
                .await
                .expect("get publication"),
            Some(pub_rec)
        );
        assert_eq!(
            store
                .list_refs(None)
                .await
                .expect("main refs")
                .iter()
                .filter(|r| r.key.is_worktree_local())
                .count(),
            1
        );
        assert_eq!(
            store
                .list_refs(Some("wt-1"))
                .await
                .expect("wt-1 refs")
                .iter()
                .filter(|r| r.key == main_branch)
                .count(),
            1
        );
    }
}
