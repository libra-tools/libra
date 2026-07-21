//! Unified sequencer state (lore.md 2.6).
//!
//! Single owner of the `sequence_state` SQLite table: a repository has at most
//! one active multi-step sequence at a time (enforced by `CHECK(id = 1)`), and
//! this module is the ONLY code allowed to read or write it — no command may
//! `CREATE TABLE` or touch the row directly. v1 migrates **cherry-pick** onto
//! it (retiring cherry-pick's lazy in-command DDL and the never-read
//! `revert_sequence` orphan); `am` also uses this table through a crate-private
//! row type so the public `SequenceKind` enum remains source-compatible; merge
//! / revert / rebase keep their existing stores and migrate in scoped follow-ups.
//!
//! Two responsibilities:
//!
//! 1. **Storage** — [`load`] / [`save`] / [`clear`] for the migrated public
//!    consumer, plus crate-private `am` counterparts.
//!    `save` is a single `DELETE`+`INSERT` inside one transaction, so a reader
//!    sees either the full old row or the full new row, never a torn write;
//!    durability rides SQLite's `synchronous = FULL` (pinned in `db.rs`), the
//!    equal of the JSON stores' `write_atomic(.., fsync = true)`.
//!
//! 2. **Detection + the symmetric mutex** — [`detect_active`] is a strictly
//!    READ-ONLY probe (safe for `libra status`; it never mutates or triggers a
//!    migration) that resolves the one active sequence across the unified table
//!    AND the three still-legacy stores. [`ensure_none_in_progress`] is the
//!    guard every sequence-start path calls so any in-progress sequence rejects
//!    any *new* one with `LBR-CONFLICT-002` — never blocking the in-progress
//!    op's own continue/abort/skip (those paths do not call the guard).

use sea_orm::{ConnectionTrait, DbBackend, Statement};

use crate::{
    internal::db::get_db_conn_instance,
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        util,
    },
};

/// Which multi-step operation owns the active sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceKind {
    Merge,
    Revert,
    CherryPick,
    Rebase,
}

impl SequenceKind {
    /// Stable token stored in the `kind` column.
    pub fn as_str(self) -> &'static str {
        match self {
            SequenceKind::Merge => "merge",
            SequenceKind::Revert => "revert",
            SequenceKind::CherryPick => "cherry_pick",
            SequenceKind::Rebase => "rebase",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        match token {
            "merge" => Some(SequenceKind::Merge),
            "revert" => Some(SequenceKind::Revert),
            "cherry_pick" => Some(SequenceKind::CherryPick),
            "rebase" => Some(SequenceKind::Rebase),
            _ => None,
        }
    }

    /// `(human label, "conclude with … / abort with …")` — used to make the
    /// mutex rejection name the blocking op and its resume/abort commands.
    fn describe(self) -> (&'static str, &'static str) {
        match self {
            SequenceKind::Merge => (
                "a merge",
                "conclude it with 'libra merge --continue' or 'libra merge --abort'",
            ),
            SequenceKind::Revert => (
                "a revert",
                "conclude it with 'libra revert --continue' or 'libra revert --abort'",
            ),
            SequenceKind::CherryPick => (
                "a cherry-pick",
                "conclude it with 'libra cherry-pick --continue' or 'libra cherry-pick --abort'",
            ),
            SequenceKind::Rebase => (
                "a rebase",
                "conclude it with 'libra rebase --continue' or 'libra rebase --abort'",
            ),
        }
    }
}

/// The unified sequence row (superset of the per-command state structs).
#[derive(Debug, Clone)]
pub struct SequenceState {
    pub kind: SequenceKind,
    /// Branch HEAD pointed at when the sequence began.
    pub head_name: String,
    /// That branch's commit at sequence start — the `--abort` rollback target.
    pub head_orig: String,
    /// The commit whose application is currently conflicted.
    pub current_oid: String,
    /// Remaining commit OIDs to apply, in order.
    pub todo: Vec<String>,
    /// Op-specific JSON payload (cherry-pick: the serialized commit-modifier
    /// options; empty when unused).
    pub payload: String,
}

/// Crate-private `am` row. Keeping this separate avoids adding a variant to
/// the public [`SequenceKind`] enum, which would break downstream exhaustive
/// matches in a patch release, while still sharing the one-row sequencer table.
#[derive(Debug, Clone)]
pub(crate) struct AmSequenceState {
    pub(crate) head_name: String,
    pub(crate) head_orig: String,
    pub(crate) current_oid: String,
    pub(crate) todo: Vec<String>,
    pub(crate) payload: String,
}

#[derive(Debug)]
struct StoredSequenceState {
    kind: String,
    head_name: String,
    head_orig: String,
    current_oid: String,
    todo: Vec<String>,
    payload: String,
}

async fn load_stored() -> Result<Option<StoredSequenceState>, String> {
    let db = get_db_conn_instance().await;
    // Part C W1 (§C.4.2): the row is keyed by worktree scope — never read
    // another worktree's in-progress sequence. `storage_key()` is "" for main.
    let stmt = Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "SELECT kind, head_name, head_orig, current_oid, todo, payload \
         FROM sequence_state WHERE worktree_id = ?",
        [current_scope_key().into()],
    );
    let row = match db.query_one(stmt).await {
        Ok(row) => row,
        // Absence-tolerant (the facade must resolve, not error, before the
        // migration has created the table or on an old binary); real DB
        // errors still propagate.
        Err(err) if is_missing_table(&err) => return Ok(None),
        Err(err) => return Err(format!("failed to load sequence_state: {err}")),
    };
    let Some(row) = row else {
        return Ok(None);
    };
    let kind: String = row
        .try_get_by_index(0)
        .map_err(|e| format!("invalid kind: {e}"))?;
    let head_name: String = row
        .try_get_by_index(1)
        .map_err(|e| format!("invalid head_name: {e}"))?;
    let head_orig: String = row
        .try_get_by_index(2)
        .map_err(|e| format!("invalid head_orig: {e}"))?;
    let current_oid: String = row
        .try_get_by_index(3)
        .map_err(|e| format!("invalid current_oid: {e}"))?;
    let todo_str: String = row
        .try_get_by_index(4)
        .map_err(|e| format!("invalid todo: {e}"))?;
    let payload: String = row
        .try_get_by_index(5)
        .map_err(|e| format!("invalid payload: {e}"))?;
    let todo = todo_str
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    Ok(Some(StoredSequenceState {
        kind,
        head_name,
        head_orig,
        current_oid,
        todo,
        payload,
    }))
}

/// Load the active unified-table sequence, if any (v1: cherry-pick).
pub async fn load() -> Result<Option<SequenceState>, String> {
    let Some(stored) = load_stored().await? else {
        return Ok(None);
    };
    let kind = SequenceKind::from_token(&stored.kind)
        .ok_or_else(|| format!("unknown sequence kind '{}'", stored.kind))?;
    Ok(Some(SequenceState {
        kind,
        head_name: stored.head_name,
        head_orig: stored.head_orig,
        current_oid: stored.current_oid,
        todo: stored.todo,
        payload: stored.payload,
    }))
}

pub(crate) async fn load_am() -> Result<Option<AmSequenceState>, String> {
    let Some(stored) = load_stored().await? else {
        return Ok(None);
    };
    if stored.kind != "am" {
        if SequenceKind::from_token(&stored.kind).is_none() {
            return Err(format!("unknown sequence kind '{}'", stored.kind));
        }
        return Ok(None);
    }
    Ok(Some(AmSequenceState {
        head_name: stored.head_name,
        head_orig: stored.head_orig,
        current_oid: stored.current_oid,
        todo: stored.todo,
        payload: stored.payload,
    }))
}

/// Persist (upsert) the active sequence. `DELETE`+`INSERT` in one transaction:
/// atomic, and the `id = 1` replace never trips the single-row `CHECK`.
pub async fn save(state: &SequenceState) -> Result<(), String> {
    use sea_orm::TransactionTrait;
    let db = get_db_conn_instance().await;
    let txn = db
        .begin()
        .await
        .map_err(|e| format!("failed to begin sequence_state transaction: {e}"))?;
    save_with_conn(&txn, state)
        .await
        .map_err(|e| format!("failed to save sequence_state: {e}"))?;
    txn.commit()
        .await
        .map_err(|e| format!("failed to commit sequence_state transaction: {e}"))?;
    Ok(())
}

/// Replace the unified sequence row using the caller's transaction. Commands
/// that move a ref and advance their sequencer position use this to make both
/// changes commit atomically with the reflog entry.
pub(crate) async fn save_with_conn<C>(db: &C, state: &SequenceState) -> Result<(), sea_orm::DbErr>
where
    C: ConnectionTrait,
{
    save_fields(
        db,
        state.kind.as_str(),
        &state.head_name,
        &state.head_orig,
        &state.current_oid,
        &state.todo,
        &state.payload,
    )
    .await
}

pub(crate) async fn save_am(state: &AmSequenceState) -> Result<(), String> {
    use sea_orm::TransactionTrait;
    let db = get_db_conn_instance().await;
    let txn = db
        .begin()
        .await
        .map_err(|e| format!("failed to begin sequence_state transaction: {e}"))?;
    save_am_with_conn(&txn, state)
        .await
        .map_err(|e| format!("failed to save am sequence_state: {e}"))?;
    txn.commit()
        .await
        .map_err(|e| format!("failed to commit sequence_state transaction: {e}"))?;
    Ok(())
}

pub(crate) async fn save_am_with_conn<C>(
    db: &C,
    state: &AmSequenceState,
) -> Result<(), sea_orm::DbErr>
where
    C: ConnectionTrait,
{
    save_fields(
        db,
        "am",
        &state.head_name,
        &state.head_orig,
        &state.current_oid,
        &state.todo,
        &state.payload,
    )
    .await
}

async fn save_fields<C>(
    db: &C,
    kind: &str,
    head_name: &str,
    head_orig: &str,
    current_oid: &str,
    todo: &[String],
    payload: &str,
) -> Result<(), sea_orm::DbErr>
where
    C: ConnectionTrait,
{
    // Part C W1 (§C.4.2): replace only THIS worktree's row. An unscoped
    // `DELETE FROM sequence_state` would wipe every other worktree's
    // in-progress sequence.
    let scope_key = current_scope_key();
    db.execute(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "DELETE FROM sequence_state WHERE worktree_id = ?",
        [scope_key.clone().into()],
    ))
    .await?;
    let todo = todo.join("\n");
    db.execute(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO sequence_state \
         (worktree_id, kind, head_name, head_orig, current_oid, todo, payload) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        [
            scope_key.into(),
            kind.to_string().into(),
            head_name.to_string().into(),
            head_orig.to_string().into(),
            current_oid.to_string().into(),
            todo.into(),
            payload.to_string().into(),
        ],
    ))
    .await?;
    Ok(())
}

/// This process's worktree scope key for the sequencer tables (`""` = main).
///
/// Resolved through [`WorktreeScope`] so the NOT NULL empty-string convention
/// is applied in exactly one place — see `src/internal/worktree_scope.rs` for
/// why it differs from the `reference` table's nullable spelling.
fn current_scope_key() -> String {
    crate::internal::worktree_scope::WorktreeScope::current()
        .storage_key()
        .to_string()
}

/// Clear the active sequence of a SPECIFIC kind (completion or abort).
/// Scoped by `kind` so a mis-routed abort can never erase a DIFFERENT
/// consumer's row once merge/revert/rebase also migrate (Codex P1).
/// Idempotent.
pub async fn clear(kind: SequenceKind) -> Result<(), String> {
    let db = get_db_conn_instance().await;
    clear_with_conn(&db, kind)
        .await
        .map_err(|e| format!("failed to clear sequence_state: {e}"))
}

/// Transaction-scoped counterpart of [`clear`].
pub(crate) async fn clear_with_conn<C>(db: &C, kind: SequenceKind) -> Result<(), sea_orm::DbErr>
where
    C: ConnectionTrait,
{
    // Part C W1 (§C.4.2): scoped by worktree AND kind — a mis-routed abort can
    // erase neither a different consumer's row nor another worktree's sequence.
    db.execute(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "DELETE FROM sequence_state WHERE kind = ? AND worktree_id = ?",
        [kind.as_str().into(), current_scope_key().into()],
    ))
    .await?;
    Ok(())
}

pub(crate) async fn clear_am() -> Result<(), String> {
    let db = get_db_conn_instance().await;
    clear_am_with_conn(&db)
        .await
        .map_err(|e| format!("failed to clear am sequence_state: {e}"))
}

pub(crate) async fn clear_am_with_conn<C>(db: &C) -> Result<(), sea_orm::DbErr>
where
    C: ConnectionTrait,
{
    // Part C W1 (§C.4.2): scoped like `clear_with_conn` — clearing this
    // worktree's `am` sequence must not touch another worktree's.
    db.execute(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "DELETE FROM sequence_state WHERE kind = ? AND worktree_id = ?",
        ["am".into(), current_scope_key().into()],
    ))
    .await?;
    Ok(())
}

/// Whether a SQLite error is a "missing table" — the ONLY error the read-only
/// detection facade may treat as "not active". Every other error (corrupt or
/// locked DB, I/O, permissions) MUST propagate so `ensure_none_in_progress`
/// fails CLOSED rather than starting a new sequence over an undetected one.
fn is_missing_table(err: &sea_orm::DbErr) -> bool {
    err.to_string().contains("no such table")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActiveSequenceKind {
    Am,
    Known(SequenceKind),
}

impl ActiveSequenceKind {
    fn describe(self) -> (&'static str, &'static str) {
        match self {
            ActiveSequenceKind::Am => (
                "an am operation",
                "conclude it with 'libra am --continue' or 'libra am --abort'",
            ),
            ActiveSequenceKind::Known(kind) => kind.describe(),
        }
    }
}

/// READ-ONLY: does the unified table hold an active row? (No migration, no
/// write — safe on the mutex hot path and in `libra status`.)
async fn unified_active() -> Result<Option<ActiveSequenceKind>, String> {
    let Some(stored) = load_stored().await? else {
        return Ok(None);
    };
    if stored.kind == "am" {
        return Ok(Some(ActiveSequenceKind::Am));
    }
    SequenceKind::from_token(&stored.kind)
        .map(ActiveSequenceKind::Known)
        .map(Some)
        .ok_or_else(|| format!("unknown sequence kind '{}'", stored.kind))
}

/// READ-ONLY error-aware probe of a legacy `<store>` table for a single row.
/// A MISSING table (fresh repo, or a consumer never used) resolves to `false`;
/// any other DB error propagates (fail-closed). Never mutates.
async fn legacy_table_active<C: ConnectionTrait>(db: &C, table: &str) -> Result<bool, String> {
    let stmt = Statement::from_string(DbBackend::Sqlite, format!("SELECT 1 FROM {table} LIMIT 1"));
    match db.query_one(stmt).await {
        Ok(Some(_)) => Ok(true),
        Ok(None) => Ok(false),
        Err(err) if is_missing_table(&err) => Ok(false),
        Err(err) => Err(format!("failed to probe {table}: {err}")),
    }
}

/// Resolve the ONE active sequence for THIS worktree across the unified
/// table, the per-worktree stores (merge / revert JSON sidecars in the local
/// gitdir; the scoped `rebase_state` row), and the main-owned legacy stores
/// (`rebase-merge`/`rebase-apply` dirs, pre-2.6 `cherry_pick_state`).
/// Strictly read-only — `libra status` and the mutex both rely on this
/// never mutating repo state (a killed sequence must stay resumable, and
/// status must never trigger a migration).
///
/// During the compat window this deliberately also probes the migrated
/// consumer's OLD store: an intervening OLD binary can recreate
/// `revert-state.json` (or a `cherry_pick_state` row), and the mutex must see
/// it — otherwise a new sequence could start over an old-binary sequence.
pub(crate) async fn detect_active_operation() -> Result<Option<ActiveSequenceKind>, String> {
    // Unified table first (cherry-pick/am in v1) — this is already scoped to the
    // current worktree by `load_stored`.
    if let Some(kind) = unified_active().await? {
        return Ok(Some(kind));
    }
    // `merge` and `revert` state are per-worktree JSON sidecars in THIS
    // worktree's gitdir (§C.4.2/§C.4.3), and both are allowed in a linked
    // worktree — so they are probed with the worktree-local path, before the
    // main-only early-return below. For the main worktree the local gitdir ==
    // common storage, so this is unchanged.
    let scope_gitdir = util::worktree_gitdir();
    if scope_gitdir.join("merge-state.json").exists() {
        return Ok(Some(ActiveSequenceKind::Known(SequenceKind::Merge)));
    }
    if scope_gitdir.join("revert-state.json").exists() {
        return Ok(Some(ActiveSequenceKind::Known(SequenceKind::Revert)));
    }

    // Part C W1 (§C.4.2/§C.4.4): `rebase_state` is keyed by `worktree_id`
    // (migration 2026072101), so probe THIS worktree's row BEFORE the linked
    // early-return — once the rebase guard lifts, a linked worktree's own
    // rebase must occupy its own mutex, while main's rebase must not block a
    // linked worktree's sequence (and vice versa).
    let db = get_db_conn_instance().await;
    if scoped_rebase_state_active(&db).await? {
        return Ok(Some(ActiveSequenceKind::Known(SequenceKind::Rebase)));
    }
    // The remaining probes cover main-owned COMMON state only (the legacy
    // rebase dirs and the pre-2.6 cherry_pick_state table have no worktree
    // scope — ambiguous-sidecar rule: they belong to main).
    if crate::internal::worktree_scope::WorktreeScope::current().is_linked() {
        return Ok(None);
    }
    let storage = util::storage_path();
    if storage.join("rebase-merge").exists() || storage.join("rebase-apply").exists() {
        return Ok(Some(ActiveSequenceKind::Known(SequenceKind::Rebase)));
    }
    // Compat window: an old binary may have recreated the pre-2.6
    // `cherry_pick_state` table after this binary migrated it away.
    if legacy_table_active(&db, "cherry_pick_state").await? {
        return Ok(Some(ActiveSequenceKind::Known(SequenceKind::CherryPick)));
    }
    Ok(None)
}

/// READ-ONLY probe of THIS worktree's `rebase_state` row (Part C W1 §C.4.2).
/// A missing table (bare test database that never ran the migration runner)
/// resolves to `false`; any other DB error propagates (fail-closed). Never
/// mutates.
async fn scoped_rebase_state_active<C: ConnectionTrait>(db: &C) -> Result<bool, String> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "SELECT 1 FROM rebase_state WHERE worktree_id = ? LIMIT 1",
        [current_scope_key().into()],
    );
    match db.query_one(stmt).await {
        Ok(row) => Ok(row.is_some()),
        Err(err) if is_missing_table(&err) => Ok(false),
        Err(err) => Err(format!("failed to probe rebase_state: {err}")),
    }
}

/// Backward-compatible public facade for the pre-`am` enum surface. An active
/// `am` cannot be represented by [`SequenceKind`], so callers receive an error
/// instead of a misleading `None` while crate-internal consumers use
/// [`detect_active_operation`].
pub async fn detect_active() -> Result<Option<SequenceKind>, String> {
    match detect_active_operation().await? {
        Some(ActiveSequenceKind::Known(kind)) => Ok(Some(kind)),
        Some(ActiveSequenceKind::Am) => {
            Err("an am operation is active and is not representable by SequenceKind".to_string())
        }
        None => Ok(None),
    }
}

/// The symmetric start-time mutex (lore.md 2.6): reject a NEW sequence when any
/// sequence is already in progress. Called from every start path; NOT from
/// continue/abort/skip, so the in-progress op can still be concluded. The
/// error names the blocking op and how to conclude or abort it.
pub async fn ensure_none_in_progress(next: SequenceKind) -> CliResult<()> {
    ensure_none_for(ActiveSequenceKind::Known(next)).await
}

pub(crate) async fn ensure_none_for_am() -> CliResult<()> {
    ensure_none_for(ActiveSequenceKind::Am).await
}

async fn ensure_none_for(next: ActiveSequenceKind) -> CliResult<()> {
    let active = detect_active_operation().await.map_err(|e| {
        CliError::fatal(format!("failed to check for an in-progress sequence: {e}"))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    let Some(active) = active else {
        return Ok(());
    };
    if active == next {
        // Same-op already in progress is handled by the command's OWN
        // resume/abort check (with its typed message); the cross-op mutex
        // only blocks a DIFFERENT kind of sequence.
        return Ok(());
    }
    let (label, how) = active.describe();
    let (starting, _) = next.describe();
    Err(CliError::fatal(format!(
        "{label} is already in progress; cannot start {starting}"
    ))
    .with_stable_code(StableErrorCode::ConflictOperationBlocked)
    .with_hint(how))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test::{ChangeDirGuard, setup_with_new_libra_in};

    fn sample(kind: SequenceKind) -> SequenceState {
        SequenceState {
            kind,
            head_name: "main".to_string(),
            head_orig: "a".repeat(40),
            current_oid: "b".repeat(40),
            todo: vec!["c".repeat(40), "d".repeat(40)],
            payload: "{\"signoff\":true}".to_string(),
        }
    }

    fn sample_am() -> AmSequenceState {
        AmSequenceState {
            head_name: "main".to_string(),
            head_orig: "a".repeat(40),
            current_oid: "b".repeat(40),
            todo: vec!["one.patch".to_string(), "two.patch".to_string()],
            payload: "{\"current\":0}".to_string(),
        }
    }

    /// Round-trip every SequenceKind through the unified table so the superset
    /// schema is validated for all four consumers (not just the migrated one).
    #[tokio::test]
    #[serial_test::serial]
    async fn save_load_clear_round_trip_all_kinds() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = ChangeDirGuard::new(tmp.path());
        setup_with_new_libra_in(tmp.path()).await;

        for kind in [
            SequenceKind::CherryPick,
            SequenceKind::Revert,
            SequenceKind::Merge,
            SequenceKind::Rebase,
        ] {
            let state = sample(kind);
            save(&state).await.expect("save");
            let loaded = load().await.expect("load").expect("present");
            assert_eq!(loaded.kind, kind);
            assert_eq!(loaded.head_orig, state.head_orig);
            assert_eq!(loaded.current_oid, state.current_oid);
            assert_eq!(loaded.todo, state.todo);
            assert_eq!(loaded.payload, state.payload);
            // Re-save (replace) must not trip CHECK(id=1).
            save(&state).await.expect("re-save replaces");
            assert!(load().await.expect("load").is_some());
            clear(kind).await.expect("clear");
            assert!(load().await.expect("load").is_none());
            // clear() is idempotent.
            clear(kind).await.expect("idempotent clear");
        }

        let state = sample_am();
        save_am(&state).await.expect("save am");
        let loaded = load_am().await.expect("load am").expect("am present");
        assert_eq!(loaded.head_orig, state.head_orig);
        assert_eq!(loaded.current_oid, state.current_oid);
        assert_eq!(loaded.todo, state.todo);
        assert_eq!(loaded.payload, state.payload);
        save_am(&state).await.expect("re-save am replaces");
        clear_am().await.expect("clear am");
        assert!(load_am().await.expect("load cleared am").is_none());
        clear_am().await.expect("idempotent clear am");
    }

    /// The symmetric mutex blocks a DIFFERENT sequence, allows the same kind
    /// (its own command handles same-op), and passes when idle.
    #[tokio::test]
    #[serial_test::serial]
    async fn ensure_none_in_progress_cross_op_matrix() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = ChangeDirGuard::new(tmp.path());
        setup_with_new_libra_in(tmp.path()).await;

        // Idle: any start is allowed.
        ensure_none_in_progress(SequenceKind::Merge)
            .await
            .expect("idle allows start");

        // An active cherry-pick blocks every other operation, including am,
        // but not a new cherry-pick (its own InProgress check owns that).
        save(&sample(SequenceKind::CherryPick)).await.expect("save");
        for other in [
            SequenceKind::Merge,
            SequenceKind::Revert,
            SequenceKind::Rebase,
        ] {
            let err = ensure_none_in_progress(other)
                .await
                .expect_err("cross-op blocked");
            assert!(
                err.to_string().contains("cherry-pick"),
                "names the blocking op: {err}"
            );
        }
        ensure_none_in_progress(SequenceKind::CherryPick)
            .await
            .expect("same-op defers to the command's own check");
        let err = ensure_none_for_am()
            .await
            .expect_err("cherry-pick blocks am");
        assert!(err.to_string().contains("cherry-pick"), "{err}");
        assert_eq!(
            detect_active().await.expect("detect"),
            Some(SequenceKind::CherryPick)
        );
        clear(SequenceKind::CherryPick).await.expect("clear");

        // The crate-private am kind symmetrically blocks every public
        // sequencer kind without expanding the public enum.
        save_am(&sample_am()).await.expect("save am");
        for other in [
            SequenceKind::Merge,
            SequenceKind::Revert,
            SequenceKind::CherryPick,
            SequenceKind::Rebase,
        ] {
            let err = ensure_none_in_progress(other)
                .await
                .expect_err("am blocks cross-op start");
            assert!(err.to_string().contains("am operation"), "{err}");
        }
        ensure_none_for_am()
            .await
            .expect("same am defers to the command's own check");
        assert_eq!(
            detect_active_operation().await.expect("detect am"),
            Some(ActiveSequenceKind::Am)
        );
        assert!(
            detect_active().await.is_err(),
            "public legacy enum must not silently hide active am"
        );
        clear_am().await.expect("clear am");
    }
}
