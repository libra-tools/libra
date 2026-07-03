//! Unified sequencer state (lore.md 2.6).
//!
//! Single owner of the `sequence_state` SQLite table: a repository has at most
//! one active multi-step sequence at a time (enforced by `CHECK(id = 1)`), and
//! this module is the ONLY code allowed to read or write it — no command may
//! `CREATE TABLE` or touch the row directly. v1 migrates **cherry-pick** onto
//! it (retiring cherry-pick's lazy in-command DDL and the never-read
//! `revert_sequence` orphan); merge / revert / rebase keep their existing
//! stores and migrate in scoped follow-ups.
//!
//! Two responsibilities:
//!
//! 1. **Storage** — [`load`] / [`save`] / [`clear`] for the migrated consumer.
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

/// Load the active unified-table sequence, if any (v1: cherry-pick).
pub async fn load() -> Result<Option<SequenceState>, String> {
    let db = get_db_conn_instance().await;
    let stmt = Statement::from_string(
        DbBackend::Sqlite,
        "SELECT kind, head_name, head_orig, current_oid, todo, payload \
         FROM sequence_state WHERE id = 1"
            .to_string(),
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
    let kind_token: String = row
        .try_get_by_index(0)
        .map_err(|e| format!("invalid kind: {e}"))?;
    let kind = SequenceKind::from_token(&kind_token)
        .ok_or_else(|| format!("unknown sequence kind '{kind_token}'"))?;
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
    Ok(Some(SequenceState {
        kind,
        head_name,
        head_orig,
        current_oid,
        todo,
        payload,
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
    txn.execute(Statement::from_string(
        DbBackend::Sqlite,
        "DELETE FROM sequence_state".to_string(),
    ))
    .await
    .map_err(|e| format!("failed to clear sequence_state: {e}"))?;
    let todo = state.todo.join("\n");
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO sequence_state \
         (id, kind, head_name, head_orig, current_oid, todo, payload) \
         VALUES (1, ?, ?, ?, ?, ?, ?)",
        [
            state.kind.as_str().into(),
            state.head_name.clone().into(),
            state.head_orig.clone().into(),
            state.current_oid.clone().into(),
            todo.into(),
            state.payload.clone().into(),
        ],
    ))
    .await
    .map_err(|e| format!("failed to save sequence_state: {e}"))?;
    txn.commit()
        .await
        .map_err(|e| format!("failed to commit sequence_state transaction: {e}"))?;
    Ok(())
}

/// Clear the active sequence of a SPECIFIC kind (completion or abort).
/// Scoped by `kind` so a mis-routed abort can never erase a DIFFERENT
/// consumer's row once merge/revert/rebase also migrate (Codex P1).
/// Idempotent.
pub async fn clear(kind: SequenceKind) -> Result<(), String> {
    let db = get_db_conn_instance().await;
    db.execute(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "DELETE FROM sequence_state WHERE kind = ?",
        [kind.as_str().into()],
    ))
    .await
    .map_err(|e| format!("failed to clear sequence_state: {e}"))?;
    Ok(())
}

/// Whether a SQLite error is a "missing table" — the ONLY error the read-only
/// detection facade may treat as "not active". Every other error (corrupt or
/// locked DB, I/O, permissions) MUST propagate so `ensure_none_in_progress`
/// fails CLOSED rather than starting a new sequence over an undetected one.
fn is_missing_table(err: &sea_orm::DbErr) -> bool {
    err.to_string().contains("no such table")
}

/// READ-ONLY: does the unified table hold an active row? (No migration, no
/// write — safe on the mutex hot path and in `libra status`.)
async fn unified_active() -> Result<Option<SequenceKind>, String> {
    Ok(load().await?.map(|state| state.kind))
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

/// Resolve the ONE active sequence across the unified table and the three
/// still-legacy stores (merge / revert JSON sidecars; rebase table + legacy
/// dir). Strictly read-only — `libra status` and the mutex both rely on this
/// never mutating repo state (a killed sequence must stay resumable, and
/// status must never trigger a migration).
///
/// During the compat window this deliberately also probes the migrated
/// consumer's OLD store: an intervening OLD binary can recreate
/// `revert-state.json` (or a `cherry_pick_state` row), and the mutex must see
/// it — otherwise a new sequence could start over an old-binary sequence.
pub async fn detect_active() -> Result<Option<SequenceKind>, String> {
    // Unified table first (cherry-pick in v1).
    if let Some(kind) = unified_active().await? {
        return Ok(Some(kind));
    }
    let storage = util::storage_path();
    // Legacy JSON sidecars (merge, revert).
    if storage.join("merge-state.json").exists() {
        return Ok(Some(SequenceKind::Merge));
    }
    if storage.join("revert-state.json").exists() {
        return Ok(Some(SequenceKind::Revert));
    }
    // Legacy rebase: DB table row or the on-disk rebase-merge dir.
    let db = get_db_conn_instance().await;
    if legacy_table_active(&db, "rebase_state").await?
        || storage.join("rebase-merge").exists()
        || storage.join("rebase-apply").exists()
    {
        return Ok(Some(SequenceKind::Rebase));
    }
    // Compat window: an old binary may have recreated the pre-2.6
    // `cherry_pick_state` table after this binary migrated it away.
    if legacy_table_active(&db, "cherry_pick_state").await? {
        return Ok(Some(SequenceKind::CherryPick));
    }
    Ok(None)
}

/// The symmetric start-time mutex (lore.md 2.6): reject a NEW sequence when any
/// sequence is already in progress. Called from every start path; NOT from
/// continue/abort/skip, so the in-progress op can still be concluded. The
/// error names the blocking op and how to conclude or abort it.
pub async fn ensure_none_in_progress(next: SequenceKind) -> CliResult<()> {
    let active = detect_active().await.map_err(|e| {
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

    /// Round-trip every SequenceKind through the unified table so the superset
    /// schema is validated for all four consumers (not just the migrated one).
    #[tokio::test]
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
    }

    /// The symmetric mutex blocks a DIFFERENT sequence, allows the same kind
    /// (its own command handles same-op), and passes when idle.
    #[tokio::test]
    async fn ensure_none_in_progress_cross_op_matrix() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = ChangeDirGuard::new(tmp.path());
        setup_with_new_libra_in(tmp.path()).await;

        // Idle: any start is allowed.
        ensure_none_in_progress(SequenceKind::Merge)
            .await
            .expect("idle allows start");

        // An active cherry-pick blocks a merge/revert/rebase start but not a
        // new cherry-pick (its own InProgress check owns that).
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
        assert_eq!(
            detect_active().await.expect("detect"),
            Some(SequenceKind::CherryPick)
        );
    }
}
