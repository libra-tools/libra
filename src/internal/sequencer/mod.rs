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

use crate::utils::{
    error::{CliError, CliResult, StableErrorCode},
    util,
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
    load_stored_in_scope(&current_scope_key()).await
}

/// [`load_stored`] against an ALREADY-RESOLVED scope key (§C.4.2
/// resolve-once): the detection path resolves the scope once and threads it
/// through, so two probes can never disagree about which worktree they are
/// answering for.
async fn load_stored_in_scope(scope_key: &str) -> Result<Option<StoredSequenceState>, String> {
    let db = request_db_checked().await?;
    load_stored_with_conn(&db, scope_key).await
}

/// [`load_stored_in_scope`] against an already-opened connection, so a caller
/// that resolved the repository once does not resolve it again.
async fn load_stored_with_conn<C: ConnectionTrait>(
    db: &C,
    scope_key: &str,
) -> Result<Option<StoredSequenceState>, String> {
    // Part C W1 (§C.4.2): the row is keyed by worktree scope — never read
    // another worktree's in-progress sequence. `storage_key()` is "" for main.
    let stmt = Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "SELECT kind, head_name, head_orig, current_oid, todo, payload \
         FROM sequence_state WHERE worktree_id = ?",
        [scope_key.into()],
    );
    let row = match db.query_one_raw(stmt).await {
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

/// [`load`] for an ALREADY-RESOLVED scope (§C.4.2).
///
/// The pseudo-ref service (§C.5) projects `CHERRY_PICK_HEAD`/`REVERT_HEAD`
/// from this row and must answer for the scope its CALLER resolved, not for
/// whichever worktree the process cwd currently names.
pub async fn load_for_scope(
    scope: &crate::internal::worktree_scope::WorktreeScope,
) -> Result<Option<SequenceState>, String> {
    let Some(stored) = load_stored_in_scope(scope.storage_key()).await? else {
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

/// Claim this worktree's sequence slot for a STARTING sequence (§C.4.4).
///
/// `ensure_none_for_control` is a check, and a check followed by a replace is
/// a TOCTOU window: two `cherry-pick` starts racing in one worktree both see
/// no row, and the loser's `DELETE`+`INSERT` then erases the winner's todo
/// while the winner's checkout stays on disk. The claim is a bare `INSERT`
/// against `worktree_id PRIMARY KEY`, so exactly one starter can win and the
/// loser is told an operation is already in progress — the same refusal it
/// would have received a moment earlier.
///
/// Progress updates keep using [`save`]: by then the caller IS the owner, and
/// replacing its own row is what advancing a sequence means.
pub async fn claim_start(state: &SequenceState) -> Result<(), String> {
    claim_fields(
        state.kind.as_str(),
        &state.head_name,
        &state.head_orig,
        &state.current_oid,
        &state.todo,
        &state.payload,
    )
    .await
}

/// The shared INSERT behind [`claim_start`] and [`claim_start_am`]: `am` is
/// stored in the same table under its own `kind`, and both starts need the
/// same all-or-nothing claim.
async fn claim_fields(
    kind: &str,
    head_name: &str,
    head_orig: &str,
    current_oid: &str,
    todo: &[String],
    payload: &str,
) -> Result<(), String> {
    let db = request_db_checked().await?;
    let scope_key = current_scope_key();
    let todo = todo.join("\n");
    let result = db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO sequence_state \
             (worktree_id, kind, head_name, head_orig, current_oid, todo, payload) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            [
                scope_key.into(),
                kind.into(),
                head_name.into(),
                head_orig.into(),
                current_oid.into(),
                todo.into(),
                payload.into(),
            ],
        ))
        .await;
    match result {
        Ok(_) => Ok(()),
        Err(err) if is_unique_violation(&err) => Err(format!(
            "another {kind} is already in progress in this worktree"
        )),
        Err(err) => Err(format!("failed to claim sequence_state: {err}")),
    }
}

/// Open the operation-log boundary for one sequencer control action
/// (§C.9, §C.11 W1).
///
/// The digest is the invocation's argv, which is what makes two worktrees
/// running the identical `--continue` distinguishable only by scope — the
/// case the scope-aware dedup key exists for. It also makes a repeated
/// `--continue` in the SAME worktree, while the first is still running,
/// refusable: the claim is a unique index, not a check.
///
/// Returns `None` when the repository has no operation log to write to (a
/// command run outside a repository is refused long before this, but the
/// helper must not turn a missing repo id into a control-action failure).
pub(crate) async fn begin_control_operation(
    control: SequencerControl,
    argv: &[String],
) -> CliResult<Option<crate::internal::operation_wrapper::OperationBoundary>> {
    use crate::internal::operation_wrapper::{OperationMeta, OperationScope, begin_operation};

    // The enumeration is the authority on what a control action IS: anything
    // entering the operation log must be one of the declared ones, or the
    // §C.9 list has drifted from the code it describes.
    debug_assert!(
        SequencerControl::ALL.contains(&control),
        "undeclared sequencer control entered the operation log: {control:?}"
    );
    let (command_name, description) = control.describe_operation();
    // Fail CLOSED: without an identity there is no boundary, and without a
    // boundary there is no worktree-wide control mutex.
    let repo_id = control_repo_id().await.map_err(|message| {
        CliError::fatal(message).with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;
    let meta = OperationMeta {
        command_name,
        description,
        actor: control_actor().await,
        repo_id,
        args_digest: Some(control_args_digest(argv, &control_position(control).await)),
    };
    // A boundary-recorded operation is never restorable, so snapshotting every
    // branch and workspace pointer would write rows nothing can ever read —
    // per control action, on the hot path of a `bisect` that marks dozens of
    // candidates. Record the head pointer, which is what `op log`/`op show`
    // display, and nothing else (§C.14).
    let scope = OperationScope {
        include_refs: false,
        include_workspace: false,
        // No control action is subject to the five-second succeeded-window.
        //
        // The window guesses that an identical command repeated within five
        // seconds is an accidental double submission. That guess does not hold
        // for a sequence, in either direction, and the suite proves both:
        //
        //   * a RESUMPTION legitimately repeats at an unchanged position —
        //     `test_rebase_empty_drop_survives_conflict_resume` drives two
        //     `rebase --continue` calls where the first dropped an empty
        //     commit, so the position never moved;
        //   * a fresh START legitimately repeats too —
        //     `readded_worktree_does_not_inherit_bisect_session` removes and
        //     re-adds a worktree and starts the same bisect again, and
        //     `bisect reset` followed by `bisect start <same args>` is simply
        //     how a user starts over.
        //
        // Nothing is lost by dropping it: a genuine double start is refused by
        // the start-time mutex and the atomic claim, with a message that says
        // what is actually wrong ("a bisect is already in progress") instead of
        // "duplicate operation". Overlap is excluded by the worktree-wide
        // control slot, which is a real mutex rather than a heuristic.
        duplicate_window: false,
        ..OperationScope::default()
    };
    match begin_operation(meta, scope).await {
        Ok(boundary) => Ok(Some(boundary)),
        Err(err) => Err(
            CliError::fatal(format!("cannot start this operation: {err}"))
                .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                .with_hint(
                    "another identical command is running or just completed in this worktree; \
             wait for it to finish, or inspect it with `libra op log`",
                ),
        ),
    }
}

/// SHA-256 over the invocation's argv AND the sequence position it acts on,
/// NUL-separated so no two payloads can collide by concatenation.
///
/// The position is what keeps duplicate suppression honest. `libra bisect
/// good` twice in a row is the NORMAL way to drive a bisect, and the two
/// invocations have byte-identical argv — without the position, the second
/// would land inside the five-second succeeded-window and be refused as a
/// repeat of the first. Two runs that act on the SAME position really are the
/// same operation; two that act on different ones are not.
fn control_args_digest(argv: &[String], position: &str) -> String {
    let payload = format!("{}\0@{position}", argv.join("\0"));
    let digest = ring::digest::digest(&ring::digest::SHA256, payload.as_bytes());
    format!("sha256:{}", hex::encode(digest.as_ref()))
}

/// Where this worktree's sequence currently stands, as the dedup identity sees
/// it: the commit a sequence stopped on, or the bisect candidate checked out.
/// `"none"` when nothing is in progress — which is right for a start, where
/// two racers genuinely ARE the same operation.
async fn control_position(control: SequencerControl) -> String {
    // The position is a log label now that no control enters thefive-second
    // window, so a read that races the slot cannot affect exclusion.
    if control.is_fresh_start() {
        return "none".to_string();
    }
    let position = match control {
        SequencerControl::BisectStart
        | SequencerControl::BisectMark
        | SequencerControl::BisectSkip
        | SequencerControl::BisectReset
        | SequencerControl::BisectRun => scoped_bisect_position().await,
        _ => load_stored()
            .await
            .ok()
            .flatten()
            .map(|stored| stored.current_oid),
    };
    position
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "none".to_string())
}

/// The candidate this worktree's bisect currently has checked out.
async fn scoped_bisect_position() -> Option<String> {
    let Ok(db) = request_db_checked().await else {
        // The position is a LOG LABEL, not an exclusion key (§C.9): the control
        // slot is what excludes. A database we cannot open is reported by the
        // command's own path a moment later with actionable context, so this
        // must not abort — it just has no position to record.
        return None;
    };
    let scope_key = current_scope_key();
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT current FROM bisect_state WHERE worktree_id = ? LIMIT 1",
            [scope_key.into()],
        ))
        .await
        .ok()??;
    row.try_get_by_index::<Option<String>>(0).ok()?
}

async fn control_actor() -> String {
    crate::internal::config::ConfigKv::get("user.name")
        .await
        .ok()
        .flatten()
        .map(|entry| entry.value)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "libra-user".to_string())
}

/// The repository id, WITHOUT creating one: a control action must not be the
/// thing that first writes `libra.repoid`.
///
/// Read through the REQUEST-BOUND connection (§C.4.2) and fallible: the ambient
/// `ConfigKv::get` opens the cwd's database and aborts if it cannot, and an
/// absent or unreadable identity used to mean "no boundary" — which silently
/// dropped the worktree-wide control mutex, letting a concurrent `--continue`
/// and `--abort` run together.
async fn control_repo_id() -> Result<String, String> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    let db = request_db_checked().await?;
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT `value` FROM `config_kv` WHERE `key` = 'libra.repoid' \
             ORDER BY `id` DESC LIMIT 1",
            [],
        ))
        .await
        .map_err(|error| format!("cannot read this repository's identity: {error}"))?;
    let value: Option<String> = match row {
        Some(row) => row
            .try_get_by_index(0)
            .map_err(|error| format!("this repository's identity is unreadable: {error}"))?,
        None => None,
    };
    value
        .filter(|value| !value.trim().is_empty() && value != "unknown-repo")
        .ok_or_else(|| {
            "this repository has no recorded identity (`libra.repoid`), so a sequencer control \
             action cannot claim its worktree's control slot — run `libra status` once to \
             record one, or `libra worktree doctor` to inspect the repository"
                .to_string()
        })
}

/// Whether a database error is the PRIMARY KEY/UNIQUE violation that means
/// "someone else claimed this scope first" rather than a real failure.
pub(crate) fn is_unique_violation(err: &sea_orm::DbErr) -> bool {
    is_unique_violation_text(&err.to_string())
}

/// The same recognition against an already-rendered message, for the layers
/// that have flattened their error to a `String`.
pub(crate) fn is_unique_violation_text(message: &str) -> bool {
    let text = message.to_ascii_lowercase();
    text.contains("unique constraint failed") || text.contains("constraint violation")
}

/// Persist (upsert) the active sequence. `DELETE`+`INSERT` in one transaction:
/// atomic, and the `id = 1` replace never trips the single-row `CHECK`.
pub async fn save(state: &SequenceState) -> Result<(), String> {
    use sea_orm::TransactionTrait;
    let db = request_db_checked().await?;
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

/// The FIRST write of a starting `am`, as an atomic claim (§C.4.4) — see
/// [`claim_start`].
pub(crate) async fn claim_start_am(state: &AmSequenceState) -> Result<(), String> {
    claim_fields(
        "am",
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
    let db = request_db_checked().await?;
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
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "DELETE FROM sequence_state WHERE worktree_id = ?",
        [scope_key.clone().into()],
    ))
    .await?;
    let todo = todo.join("\n");
    db.execute_raw(Statement::from_sql_and_values(
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
/// The database of the repository THIS INVOCATION pinned, as a `Result`.
///
/// NOT a fallback when a pin exists: `request_db` already returns the ambient
/// connection for an unpinned caller, so an `Err` here means the PINNED
/// repository could not be resolved — and substituting whichever repository the
/// cwd points at is exactly the write this pairing exists to prevent.
///
/// The realistic trigger is a linked worktree whose `commondir` is missing or
/// corrupt, which is a repairable user-facing condition rather than a bug, so
/// the message names the worktree and the command that fixes it.
pub(crate) async fn request_db_checked() -> Result<sea_orm::DatabaseConnection, String> {
    crate::internal::worktree_scope::request_db()
        .await
        .map_err(|error| {
            let workdir = crate::internal::worktree_scope::WorktreeScope::request_scope()
                .map(|pinned| pinned.workdir.display().to_string())
                .unwrap_or_else(|| "this worktree".to_string());
            format!(
                "cannot open the repository database for '{workdir}': {error}. If this is a \
                 linked worktree, its `.libra/commondir` may be missing or corrupt — run \
                 `libra worktree repair --confirm {workdir}` from the main worktree"
            )
        })
}

/// This INVOCATION's scope key (§C.4.2 resolve-once): the pinned request scope
/// when dispatch resolved one, the cwd otherwise. Every sequencer read and
/// write goes through here, so a cwd that moves mid-command cannot send a save
/// into another worktree's row.
fn current_scope_key() -> String {
    crate::internal::worktree_scope::WorktreeScope::for_request()
        .storage_key()
        .to_string()
}

/// Clear the active sequence of a SPECIFIC kind (completion or abort).
/// Scoped by `kind` so a mis-routed abort can never erase a DIFFERENT
/// consumer's row once merge/revert/rebase also migrate (Codex P1).
/// Idempotent.
pub async fn clear(kind: SequenceKind) -> Result<(), String> {
    let db = request_db_checked().await?;
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
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "DELETE FROM sequence_state WHERE kind = ? AND worktree_id = ?",
        [kind.as_str().into(), current_scope_key().into()],
    ))
    .await?;
    Ok(())
}

pub(crate) async fn clear_am() -> Result<(), String> {
    let db = request_db_checked().await?;
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
    db.execute_raw(Statement::from_sql_and_values(
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActiveSequenceKind {
    Am,
    /// A bisect owns this scope. Crate-private for the same reason `Am` is:
    /// adding a variant to the public [`SequenceKind`] would break downstream
    /// exhaustive matches in a patch release.
    Bisect,
    Known(SequenceKind),
    /// A legacy common `rebase-merge/` or `rebase-apply/` directory exists,
    /// linked worktrees are registered, and nothing can say whose rebase it
    /// is (§C.4.2 / ADR-0714-08).
    ///
    /// Deliberately NOT `Known(Rebase)`: this worktree has no scoped state,
    /// so a reporting command (`status`) must keep working and say what it
    /// found, while a command that would START a sequence must refuse — the
    /// directory is neither adopted nor cleared.
    ///
    /// The observed path travels WITH the variant: the refusal and the status
    /// advisory both have to name the directory that actually exists, and
    /// re-probing at render time could name a different one (or none).
    AmbiguousLegacy(AmbiguousLegacyState),
}

/// WHERE the ambiguous legacy state lives, so the guidance can be true.
///
/// A DB table is not a file: telling a user to remove a directory that does not
/// exist is guidance they cannot follow, and telling them to `rm` something
/// that is actually a table would be worse if they found a way to try.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AmbiguousLegacyState {
    /// A crash-recovery DIRECTORY in common storage (`rebase-merge/`,
    /// `rebase-apply/`).
    Directory(std::path::PathBuf),
    /// A per-operation sidecar FILE in common storage
    /// (`merge-state.json`, `revert-state.json`).
    File(std::path::PathBuf),
    /// An unscoped legacy TABLE in the repository database.
    Table(&'static str),
}

impl AmbiguousLegacyState {
    /// `(what it is, how to clear it)` — accurate for each medium.
    pub(crate) fn describe(&self) -> (String, String) {
        match self {
            // Names the directory that actually EXISTS: a refusal the user
            // cannot locate is a refusal they cannot clear.
            Self::Directory(path) => (
                format!(
                    "a legacy rebase state directory at '{}', whose owning worktree cannot be \
                     determined",
                    path.display()
                ),
                "finish or abort that rebase in the worktree that owns it, or remove that \
                 directory once you have confirmed it is stale"
                    .to_string(),
            ),
            Self::File(path) => (
                format!(
                    "a legacy state file at '{}' in shared storage, whose owning worktree \
                     cannot be determined",
                    path.display()
                ),
                "conclude that operation in the worktree that owns it, or remove that file \
                 once you have confirmed it is stale"
                    .to_string(),
            ),
            Self::Table(table) => (
                format!(
                    "an unscoped legacy `{table}` row in the repository database, whose owning \
                     worktree cannot be determined"
                ),
                "conclude that operation in the worktree that owns it. This is a DATABASE row, \
                 not a file — there is nothing to delete on disk, and clearing it needs an \
                 explicit repair. Inspect it with `libra worktree doctor`"
                    .to_string(),
            ),
        }
    }
}

impl ActiveSequenceKind {
    fn describe(&self) -> (String, String) {
        match self {
            ActiveSequenceKind::Am => (
                "an am operation".to_string(),
                "conclude it with 'libra am --continue' or 'libra am --abort'".to_string(),
            ),
            ActiveSequenceKind::Bisect => (
                "a bisect".to_string(),
                "conclude it with 'libra bisect reset'".to_string(),
            ),
            ActiveSequenceKind::Known(kind) => {
                let (label, how) = kind.describe();
                (label.to_string(), how.to_string())
            }
            // Names the directory that actually EXISTS: a refusal the user
            // cannot locate is a refusal they cannot clear.
            ActiveSequenceKind::AmbiguousLegacy(state) => state.describe(),
        }
    }
}

/// READ-ONLY: does the unified table hold an active row? (No migration, no
/// write — safe on the mutex hot path and in `libra status`.)
async fn unified_active_in_scope<C: ConnectionTrait>(
    db: &C,
    scope_key: &str,
) -> Result<Option<ActiveSequenceKind>, String> {
    let Some(stored) = load_stored_with_conn(db, scope_key).await? else {
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
    match db.query_one_raw(stmt).await {
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
    // §C.4.2 resolve-once: the scope is read from the process CWD, so
    // resolving it separately in each probe lets a CWD change between two
    // awaits combine one worktree's unified row with another's sidecar or
    // `rebase_state` probe — and a start could then pass a mutex that should
    // have refused it. Resolved ONCE here and threaded through every probe
    // below. (The wider cleanup of ambient resolution across the sequencer,
    // dirty cache, layer and sparse stores stays with LR-02; this function is
    // the one W1 changes, so it is the one W1 fixes.)
    // ONE observation of the working directory, and everything derived from
    // THAT — not three independent ambient reads. "No await between them" is
    // not enough: `set_current_dir` is process-global, so another thread can
    // move the CWD between two of those reads and produce scope A with
    // gitdir/database B, bypassing or misapplying the mutex.
    //
    // The invocation's PINNED context wins when there is one (§C.4.2): dispatch
    // resolved it before any handler ran, so a cwd that has since moved cannot
    // point the mutex at another worktree's rows. Only an unpinned caller — a
    // library user, the `libra code` server, a unit test — reads the cwd, and
    // then both halves come from that one read.
    // A pinned invocation uses the context the pin ALREADY resolved. An
    // unpinned one — a library user, the `libra code` server, a unit test —
    // resolves ONE through the very same constructor, so both branches produce
    // a scope, gitdir and storage that came from a single filesystem walk.
    //
    // Three separate walks here was the defect: `for_workdir`, then a gitdir
    // lookup, then a storage lookup, each re-reading the tree. A repository
    // replaced (or a cwd moved) between them handed the mutex one worktree's
    // scope key and another's gitdir, which is exactly what the mutex must
    // never do — it would take a lock in repository B on behalf of A.
    let context = match crate::internal::worktree_scope::WorktreeScope::request_scope() {
        Some(pinned) => pinned,
        None => {
            let workdir = std::env::current_dir()
                .map_err(|error| format!("failed to resolve the current directory: {error}"))?;
            crate::internal::worktree_scope::RequestScope::resolve(workdir).ok_or_else(|| {
                "failed to resolve this worktree: the current directory is not inside a libra                  repository"
                    .to_string()
            })?
        }
    };
    let scope_gitdir = context.gitdir;
    let common_storage = context.storage;
    let scope_key = context.scope.storage_key().to_string();
    let scope = context.scope;
    let db =
        crate::internal::db::get_db_conn_instance_for_path(&common_storage.join(util::DATABASE))
            .await
            .map_err(|error| format!("failed to open the repository database: {error}"))?;
    // Unified table first (cherry-pick/am in v1), against the resolved scope.
    if let Some(kind) = unified_active_in_scope(&db, &scope_key).await? {
        return Ok(Some(kind));
    }
    // `merge` and `revert` state are per-worktree JSON sidecars in THIS
    // worktree's gitdir (§C.4.2/§C.4.3), and both are allowed in a linked
    // worktree — so they are probed with the worktree-local path, before the
    // main-only early-return below. For the main worktree the local gitdir ==
    // common storage, so this is unchanged.
    for (name, kind) in [
        ("merge-state.json", SequenceKind::Merge),
        ("revert-state.json", SequenceKind::Revert),
    ] {
        let sidecar = scope_gitdir.join(name);
        if !sidecar.exists() {
            continue;
        }
        // §C.4.3: for a LINKED worktree the sidecar is in its own gitdir, so it
        // is unambiguously that worktree's. For MAIN the local gitdir IS common
        // storage — and a sidecar there could have been written by a linked
        // worktree that has since been removed, in which case continuing or
        // aborting it would reset MAIN's HEAD, index and working tree from
        // another worktree's state and then delete the evidence. With
        // linked-worktree history and no way to prove ownership, it is
        // AMBIGUOUS: status reports it, control paths refuse.
        // W2: a sidecar whose writer RECORDED main's scope is proven main's,
        // even in a repository with linked history — the guess W1 had to make
        // is only made for files that carry no record (an old binary's).
        let proven_main = sidecar_recorded_owner(&sidecar)?.as_deref() == Some("");
        if !proven_main
            && !scope.is_linked()
            && crate::command::maintenance::repository_had_linked_worktrees()
        {
            return Ok(Some(ActiveSequenceKind::AmbiguousLegacy(
                AmbiguousLegacyState::File(sidecar),
            )));
        }
        return Ok(Some(ActiveSequenceKind::Known(kind)));
    }

    // Part C W1 (§C.4.2/§C.4.4): `rebase_state` is keyed by `worktree_id`
    // (migration 2026072101), so probe THIS worktree's row BEFORE the linked
    // early-return — once the rebase guard lifts, a linked worktree's own
    // rebase must occupy its own mutex, while main's rebase must not block a
    // linked worktree's sequence (and vice versa).
    if scoped_rebase_state_active(&db, &scope_key).await? {
        return Ok(Some(ActiveSequenceKind::Known(SequenceKind::Rebase)));
    }
    // §C.4.4 bisect/sequencer mutual exclusion: a bisect owns the scope just
    // as a rebase does. It was missing here entirely, so `bisect start` could
    // begin beside an active sequence (and vice versa) in the same worktree.
    if scoped_bisect_state_active(&db, &scope_key).await? {
        return Ok(Some(ActiveSequenceKind::Bisect));
    }
    // The remaining probes cover main-owned COMMON state only (the legacy
    // rebase dirs and the pre-2.6 cherry_pick_state table have no worktree
    // scope — ambiguous-sidecar rule: they belong to main).
    if scope.is_linked() {
        return Ok(None);
    }
    // The storage this invocation RESOLVED, not the ambient one: the unified
    // row above was read from the pinned database, and pairing it with a
    // sidecar directory found under a cwd that has since moved is precisely
    // the mismatch §C.4.2 exists to prevent.
    if let Some(legacy_dir) = ambiguous_legacy_dir(&common_storage) {
        // The common legacy dirs carry no owner metadata. Treating one as
        // MAIN's rebase is only correct when main is the unambiguous owner.
        // With linked worktrees registered it may be any of them — and since
        // `rebase --abort` now PRESERVES such a directory instead of deleting
        // it (§C.4.2 / ADR-0714-08), reading it as "a rebase is in progress"
        // would block main's next cherry-pick forever on state that is not
        // main's and that main is not allowed to clear.
        //
        // Reported as its OWN kind rather than as an error: a command that
        // only READS state must keep working and say what it found, while a
        // command that would START a sequence refuses.
        if crate::command::maintenance::repository_had_linked_worktrees() {
            return Ok(Some(ActiveSequenceKind::AmbiguousLegacy(
                AmbiguousLegacyState::Directory(legacy_dir),
            )));
        }
        return Ok(Some(ActiveSequenceKind::Known(SequenceKind::Rebase)));
    }
    // Compat window: an old binary may have recreated the pre-2.6
    // `cherry_pick_state` table after this binary migrated it away.
    if legacy_table_active(&db, "cherry_pick_state").await? {
        // Same rule: an unscoped legacy table cannot prove it was main's.
        if !scope.is_linked() && crate::command::maintenance::repository_had_linked_worktrees() {
            return Ok(Some(ActiveSequenceKind::AmbiguousLegacy(
                AmbiguousLegacyState::Table("cherry_pick_state"),
            )));
        }
        return Ok(Some(ActiveSequenceKind::Known(SequenceKind::CherryPick)));
    }
    Ok(None)
}

/// READ-ONLY probe of THIS scope's `bisect_state` row (§C.4.4).
///
/// A missing table (a database that never ran the migration runner) resolves
/// to `false`; any other DB error propagates. Never mutates.
async fn scoped_bisect_state_active<C: ConnectionTrait>(
    db: &C,
    scope_key: &str,
) -> Result<bool, String> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Sqlite,
        // ANY retained row, converged or not: a completed bisect deliberately
        // keeps its row and `orig_head` so `bisect reset` can return HEAD
        // there. If a rebase could start in between, that reset would move
        // HEAD away from the new work. Only `reset` clears ownership.
        "SELECT 1 FROM bisect_state WHERE worktree_id = ? LIMIT 1",
        [scope_key.into()],
    );
    match db.query_one_raw(stmt).await {
        Ok(row) => Ok(row.is_some()),
        Err(err) if is_missing_table(&err) => Ok(false),
        Err(err) => Err(format!("failed to probe bisect_state: {err}")),
    }
}

/// The legacy common rebase directory that ACTUALLY exists, if either does.
///
/// Callers name this rather than a fixed `rebase-merge`: pointing a user at a
/// path that is not there leaves them unable to clear the block.
/// The scope RECORDED inside a merge/revert sidecar, if the writing binary
/// recorded one (W2, ADR-0714-08).
///
/// W1 had to treat every common-storage sidecar with linked-worktree history
/// as ambiguous, because ownership was a guess. W2 removes the guess for its
/// own files: `merge`/`revert` record the writer's storage key as
/// `owner_scope` when they save, so a sidecar main just wrote is PROVEN
/// main's and stays operable. An absent field (an old binary's file) or an
/// unreadable document answers `None`, which every caller treats as "cannot
/// prove" — exactly the W1 rule.
pub(crate) fn sidecar_recorded_owner(path: &std::path::Path) -> Result<Option<String>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        // An UNREADABLE file is not evidence of anything — calling it
        // "ownerless" would tell the user to inspect-or-delete a file whose
        // real problem is a read error they need to see.
        Err(error) => return Err(format!("cannot read '{}': {error}", path.display())),
    };
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
        format!(
            "'{}' is not valid JSON ({error}); the sidecar is corrupt, not merely \
             unowned — repair or remove it after inspecting a backup",
            path.display()
        )
    })?;
    Ok(value
        .get("owner_scope")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string))
}

pub(crate) fn ambiguous_legacy_dir(storage: &std::path::Path) -> Option<std::path::PathBuf> {
    ["rebase-merge", "rebase-apply"]
        .into_iter()
        .map(|name| storage.join(name))
        .find(|candidate| candidate.exists())
}

/// READ-ONLY probe of THIS worktree's `rebase_state` row (Part C W1 §C.4.2).
/// A missing table (bare test database that never ran the migration runner)
/// resolves to `false`; any other DB error propagates (fail-closed). Never
/// mutates.
async fn scoped_rebase_state_active<C: ConnectionTrait>(
    db: &C,
    scope_key: &str,
) -> Result<bool, String> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "SELECT 1 FROM rebase_state WHERE worktree_id = ? LIMIT 1",
        [scope_key.into()],
    );
    match db.query_one_raw(stmt).await {
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
        Some(ActiveSequenceKind::Bisect) => {
            Err("a bisect is active and is not representable by SequenceKind".to_string())
        }
        // Not a sequence this worktree owns, and not representable as one:
        // the public facade reports "none active" for THIS scope, which is
        // the truth. The mutex path handles the refusal.
        Some(ActiveSequenceKind::AmbiguousLegacy(_)) | None => Ok(None),
    }
}

/// Every sequencer CONTROL ACTION, and the mutable state it may touch.
///
/// §C.9 asks W1 for exactly this: "`worktree add/move/remove/repair/migrate`
/// 与 sequencer start/continue/skip/abort 声明 mutation scope，为 LR-02
/// wrapper coverage guard 提供枚举" — the declaration, so LR-02's coverage
/// guard has a complete list to check the wrapper against. Entering the
/// operation wrapper itself is LR-02 work: `with_operation_log` runs its
/// business closure INSIDE a `DatabaseTransaction`, while these actions
/// check out files and open their own pooled transactions, which the
/// `_with_conn` contract in `internal/branch.rs` documents as a deadlock.
///
/// The match in [`SequencerControl::mutation_scope`] is exhaustive with no
/// wildcard, so a new control action does not compile until it declares.
// The continue/skip/abort variants and `ALL` are the ENUMERATION §C.9 asks
// for, not call-site helpers: their consumer is LR-02's wrapper-coverage
// guard, which does not exist yet, plus the declaration test below. They are
// listed here — rather than derived later from whatever the commands happen to
// do — precisely so that guard has something authoritative to check against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SequencerControl {
    Start(SequenceKind),
    Continue(SequenceKind),
    Skip(SequenceKind),
    Abort(SequenceKind),
    /// `merge --restart`: discards the conflicted attempt and re-runs the
    /// merge from its recorded target. It resets this worktree and rewrites the
    /// sequencer row, so it is a control action like any other.
    Restart(SequenceKind),
    /// `cherry-pick --quit`: clears the sequencer row and leaves
    /// the working tree where it is. It changes no ref, but it DOES end the
    /// sequence, so it is a control action like any other.
    Quit(SequenceKind),
    AmStart,
    AmContinue,
    AmSkip,
    AmAbort,
    BisectStart,
    BisectMark,
    BisectSkip,
    BisectReset,
    /// `bisect run <cmd>`: drives mark after mark automatically. It is the
    /// most mutating control of all — it checks out a candidate per step —
    /// so leaving it undeclared would exempt the one that changes the most.
    BisectRun,
}

/// What a control action mutates (§C.9 / §C.4.1.1 inventory).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ControlMutationScope {
    /// THIS worktree's HEAD, index or working files.
    pub(crate) worktree_state: bool,
    /// Repository-wide refs (branch tips, notes, reflogs).
    pub(crate) repository_refs: bool,
    /// This worktree's sequencer row / sidecars.
    pub(crate) sequencer_state: bool,
}

impl SequencerControl {
    pub(crate) fn mutation_scope(self) -> ControlMutationScope {
        match self {
            // Every start writes the scoped sequencer row and moves this
            // worktree's HEAD/index; only the ones that can land a commit
            // touch repository refs.
            SequencerControl::Start(_) | SequencerControl::AmStart => ControlMutationScope {
                worktree_state: true,
                repository_refs: true,
                sequencer_state: true,
            },
            SequencerControl::Continue(_)
            | SequencerControl::AmContinue
            | SequencerControl::Skip(_)
            | SequencerControl::AmSkip => ControlMutationScope {
                worktree_state: true,
                repository_refs: true,
                sequencer_state: true,
            },
            // An abort restores this worktree and clears its row; it moves the
            // branch back to where the sequence started, so refs too.
            SequencerControl::Restart(_) => ControlMutationScope {
                worktree_state: true,
                repository_refs: true,
                sequencer_state: true,
            },
            SequencerControl::Abort(_) | SequencerControl::AmAbort => ControlMutationScope {
                worktree_state: true,
                repository_refs: true,
                sequencer_state: true,
            },
            // `--quit` forgets the sequence and keeps whatever is already in
            // the tree: the row goes, nothing is restored, no ref moves.
            SequencerControl::Quit(_) => ControlMutationScope {
                worktree_state: false,
                repository_refs: false,
                sequencer_state: true,
            },
            // Bisect checks out candidates in THIS worktree and keeps its own
            // scoped row; it never moves a branch.
            SequencerControl::BisectStart
            | SequencerControl::BisectMark
            | SequencerControl::BisectSkip
            | SequencerControl::BisectReset
            | SequencerControl::BisectRun => ControlMutationScope {
                worktree_state: true,
                repository_refs: false,
                sequencer_state: true,
            },
        }
    }

    /// Whether this control BEGINS a sequence, as opposed to driving or
    /// ending one already in progress.
    ///
    /// `bisect run` is NOT one: it requires an existing session and drives it,
    /// so re-running a script the user just fixed is a continuation and must
    /// not be refused as a repeat.
    pub(crate) fn is_fresh_start(self) -> bool {
        matches!(
            self,
            SequencerControl::Start(_) | SequencerControl::AmStart | SequencerControl::BisectStart
        )
    }

    /// The operation-log identity of this control: the command name recorded
    /// in `operation`, and its human description.
    pub(crate) fn describe_operation(self) -> (String, String) {
        let (command, action) = match self {
            SequencerControl::Start(kind) => (kind.as_str(), "start"),
            SequencerControl::Continue(kind) => (kind.as_str(), "continue"),
            SequencerControl::Skip(kind) => (kind.as_str(), "skip"),
            SequencerControl::Abort(kind) => (kind.as_str(), "abort"),
            SequencerControl::Quit(kind) => (kind.as_str(), "quit"),
            SequencerControl::Restart(kind) => (kind.as_str(), "restart"),
            SequencerControl::AmStart => ("am", "start"),
            SequencerControl::AmContinue => ("am", "continue"),
            SequencerControl::AmSkip => ("am", "skip"),
            SequencerControl::AmAbort => ("am", "abort"),
            SequencerControl::BisectStart => ("bisect", "start"),
            SequencerControl::BisectMark => ("bisect", "mark"),
            SequencerControl::BisectSkip => ("bisect", "skip"),
            SequencerControl::BisectReset => ("bisect", "reset"),
            SequencerControl::BisectRun => ("bisect", "run"),
        };
        (command.to_string(), format!("{command} {action}"))
    }

    /// The full enumeration, for the coverage guard and its test.
    pub(crate) const ALL: &'static [SequencerControl] = &[
        SequencerControl::Start(SequenceKind::Merge),
        SequencerControl::Start(SequenceKind::Revert),
        SequencerControl::Start(SequenceKind::CherryPick),
        SequencerControl::Start(SequenceKind::Rebase),
        SequencerControl::Continue(SequenceKind::Merge),
        SequencerControl::Continue(SequenceKind::Revert),
        SequencerControl::Continue(SequenceKind::CherryPick),
        SequencerControl::Continue(SequenceKind::Rebase),
        SequencerControl::Skip(SequenceKind::CherryPick),
        SequencerControl::Skip(SequenceKind::Rebase),
        SequencerControl::Skip(SequenceKind::Revert),
        SequencerControl::Abort(SequenceKind::Merge),
        SequencerControl::Abort(SequenceKind::Revert),
        SequencerControl::Abort(SequenceKind::CherryPick),
        SequencerControl::Abort(SequenceKind::Rebase),
        SequencerControl::Restart(SequenceKind::Merge),
        SequencerControl::Quit(SequenceKind::CherryPick),
        SequencerControl::AmStart,
        SequencerControl::AmContinue,
        SequencerControl::AmSkip,
        SequencerControl::AmAbort,
        SequencerControl::BisectStart,
        SequencerControl::BisectMark,
        SequencerControl::BisectSkip,
        SequencerControl::BisectReset,
        SequencerControl::BisectRun,
    ];
}

/// The other half of §C.9's declaration requirement: what a `worktree`
/// lifecycle action mutates.
///
/// The plan names `worktree add/move/remove/repair/migrate` alongside the
/// sequencer controls, for the same reason — LR-02's wrapper-coverage guard
/// needs an authoritative enumeration to check against, and deriving one later
/// from whatever the commands happen to do would encode the drift rather than
/// catch it. Registry and workspace are their own axes here: a `worktree`
/// action can rewrite `worktrees.json` and the workspace lease without
/// touching any scoped sequencer row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorktreeControl {
    Add,
    Move,
    Remove,
    /// `worktree prune`: retires entries whose path is gone. It writes the
    /// registry and drops those scopes' rows, so it is a lifecycle action like
    /// the rest — leaving it undeclared let a replayable journal op escape the
    /// inventory the declaration exists to close.
    Prune,
    Repair,
    MigrateLayout,
}

/// What a worktree lifecycle action mutates (§C.9 / §C.4.1.1 inventory).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WorktreeMutationScope {
    /// `worktrees.json` — the registry of which worktrees exist.
    pub(crate) registry: bool,
    /// A worktree's own gitdir, HEAD row, index or files.
    pub(crate) worktree_state: bool,
    /// Repository-wide refs (a `-b` branch created by `add`).
    pub(crate) repository_refs: bool,
    /// Rows scoped to a worktree that is being retired or moved.
    pub(crate) scoped_rows: bool,
}

impl WorktreeControl {
    pub(crate) fn mutation_scope(self) -> WorktreeMutationScope {
        match self {
            // `add` writes the registry, seeds the new worktree's own HEAD and
            // index, and with `-b` creates a branch.
            WorktreeControl::Add => WorktreeMutationScope {
                registry: true,
                worktree_state: true,
                repository_refs: true,
                scoped_rows: true,
            },
            // `move` rewrites the entry's path and the gitdir pointers; the
            // scoped rows travel with it untouched in content but re-homed.
            WorktreeControl::Move => WorktreeMutationScope {
                registry: true,
                worktree_state: true,
                repository_refs: false,
                scoped_rows: true,
            },
            // `remove` retires the entry and, with `--delete-dir`, the scoped
            // rows and the directory. It never moves a branch.
            WorktreeControl::Remove => WorktreeMutationScope {
                registry: true,
                worktree_state: true,
                repository_refs: false,
                scoped_rows: true,
            },
            // `prune` retires entries whose directory is gone: the registry
            // and those scopes' rows. It touches no surviving worktree's HEAD
            // or index, and moves no branch.
            WorktreeControl::Prune => WorktreeMutationScope {
                registry: true,
                worktree_state: false,
                repository_refs: false,
                scoped_rows: true,
            },
            // `repair` restores a worktree's identity files and reconciles the
            // registry; it does not touch that worktree's HEAD or index.
            WorktreeControl::Repair => WorktreeMutationScope {
                registry: true,
                worktree_state: true,
                repository_refs: false,
                scoped_rows: false,
            },
            // `repair --migrate-layout` replaces a legacy symlink gitdir with
            // a real one and seeds the migrated worktree's own scoped rows.
            WorktreeControl::MigrateLayout => WorktreeMutationScope {
                registry: true,
                worktree_state: true,
                repository_refs: false,
                scoped_rows: true,
            },
        }
    }

    /// The full enumeration, for the coverage guard and its test.
    pub(crate) const ALL: &'static [WorktreeControl] = &[
        WorktreeControl::Add,
        WorktreeControl::Move,
        WorktreeControl::Remove,
        WorktreeControl::Prune,
        WorktreeControl::Repair,
        WorktreeControl::MigrateLayout,
    ];

    pub(crate) fn command_name(self) -> &'static str {
        match self {
            WorktreeControl::Add => "worktree add",
            WorktreeControl::Move => "worktree move",
            WorktreeControl::Remove => "worktree remove",
            WorktreeControl::Prune => "worktree prune",
            WorktreeControl::Repair => "worktree repair",
            WorktreeControl::MigrateLayout => "worktree repair --migrate-layout",
        }
    }

    /// The declaration, asserted at the point of use.
    ///
    /// Every lifecycle action states what it is about to mutate before it
    /// writes its journal row, so the §C.9 inventory is exercised by the code
    /// it describes rather than only by its own test.
    pub(crate) fn declare(self) -> &'static str {
        let scope = self.mutation_scope();
        debug_assert!(
            scope.registry,
            "{} is a registry mutation: {scope:?}",
            self.command_name()
        );
        debug_assert!(
            Self::ALL.contains(&self),
            "undeclared worktree lifecycle action: {}",
            self.command_name()
        );
        self.journal_op()
    }

    /// The `op` value this action writes into `worktree_intent_journal`.
    ///
    /// The journal is what crash recovery replays, so the declaration and the
    /// durable record are the SAME value rather than two strings that can
    /// drift: a declared action with no journal op, or a journal op nothing
    /// declares, is a gap in the §C.9 inventory by construction.
    pub(crate) fn journal_op(self) -> &'static str {
        match self {
            WorktreeControl::Add => "add",
            WorktreeControl::Move => "move",
            WorktreeControl::Remove => "remove",
            WorktreeControl::Prune => "prune",
            WorktreeControl::Repair => "repair",
            WorktreeControl::MigrateLayout => "migrate",
        }
    }
}

/// The symmetric start-time mutex (lore.md 2.6): reject a NEW sequence when any
/// sequence is already in progress. Called from every start path; NOT from
/// continue/abort/skip, so the in-progress op can still be concluded. The
/// error names the blocking op and how to conclude or abort it.
pub async fn ensure_none_in_progress(next: SequenceKind) -> CliResult<()> {
    ensure_none_for_control(SequencerControl::Start(next)).await
}

/// The mutex, entered by a DECLARED control action (§C.9).
///
/// Only controls whose declared `mutation_scope` includes `sequencer_state`
/// are subject to it — that declaration is what makes an action a sequencer
/// control rather than an ordinary command, and reading it here keeps the
/// enumeration honest instead of decorative.
pub(crate) async fn ensure_none_for_control(control: SequencerControl) -> CliResult<()> {
    debug_assert!(
        control.mutation_scope().sequencer_state,
        "only sequencer-state controls enter the mutex: {control:?}"
    );
    let next = match control {
        SequencerControl::Start(kind)
        | SequencerControl::Continue(kind)
        | SequencerControl::Skip(kind)
        | SequencerControl::Abort(kind)
        | SequencerControl::Quit(kind)
        | SequencerControl::Restart(kind) => ActiveSequenceKind::Known(kind),
        SequencerControl::AmStart
        | SequencerControl::AmContinue
        | SequencerControl::AmSkip
        | SequencerControl::AmAbort => ActiveSequenceKind::Am,
        SequencerControl::BisectStart
        | SequencerControl::BisectMark
        | SequencerControl::BisectSkip
        | SequencerControl::BisectReset
        | SequencerControl::BisectRun => ActiveSequenceKind::Bisect,
    };
    ensure_none_for(next).await
}

pub(crate) async fn ensure_none_for_am() -> CliResult<()> {
    ensure_none_for_control(SequencerControl::AmStart).await
}

/// The bisect side of the symmetric mutex (§C.4.4).
pub(crate) async fn ensure_none_for_bisect() -> CliResult<()> {
    ensure_none_for_control(SequencerControl::BisectStart).await
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

    /// §C.4.2: active-operation detection reads the sidecars of the
    /// repository it is PINNED to, not of the one the cwd wandered into.
    ///
    /// The unified/scoped rows come from the pinned database; if the legacy
    /// sidecar probe re-resolved storage from the cwd, a repository with a
    /// `rebase-merge` directory could be reported as the pinned repository's
    /// in-progress rebase — blocking a sequence that has nothing to resume,
    /// or resuming against another repository's files.
    #[tokio::test]
    #[serial_test::serial]
    async fn detection_probes_the_pinned_repository_not_the_cwd() {
        let quiet = tempfile::tempdir().expect("quiet repo");
        let noisy = tempfile::tempdir().expect("noisy repo");
        let original = std::env::current_dir().expect("cwd");
        for repo in [quiet.path(), noisy.path()] {
            let _cd = crate::utils::test::ChangeDirGuard::new(repo);
            crate::utils::test::setup_with_new_libra_in(repo).await;
        }
        // The OTHER repository has a legacy rebase directory; the pinned one
        // has nothing in progress.
        std::fs::create_dir_all(noisy.path().join(util::ROOT_DIR).join("rebase-merge"))
            .expect("legacy dir");

        let _pin = crate::internal::worktree_scope::WorktreeScope::pin_request_scope(
            quiet.path().to_path_buf(),
        );
        std::env::set_current_dir(noisy.path()).expect("move the cwd");

        let detected = detect_active_operation()
            .await
            .expect("detection reads the pinned repository");
        std::env::set_current_dir(&original).expect("restore the cwd");
        assert_eq!(
            detected, None,
            "the other repository's legacy rebase directory is not this one's sequence"
        );
    }

    /// §C.9: every sequencer control action DECLARES its mutation scope, and
    /// every one of them touches this worktree's sequencer state — that is
    /// what makes them scope-bearing actions at all. The enumeration is what
    /// LR-02's wrapper-coverage guard consumes.
    #[test]
    fn every_sequencer_control_declares_its_mutation_scope() {
        assert_eq!(
            SequencerControl::ALL.len(),
            26,
            "the enumeration is the contract: add the new control here too"
        );
        // No duplicates: a repeated entry inflates the count and lets the
        // cardinality assertion above pass while a real control is missing.
        for (index, control) in SequencerControl::ALL.iter().enumerate() {
            assert!(
                !SequencerControl::ALL[..index].contains(control),
                "{control:?} appears twice in ALL"
            );
        }
        for control in SequencerControl::ALL {
            let scope = control.mutation_scope();
            assert!(
                scope.sequencer_state,
                "{control:?} owns scoped sequencer state"
            );
            // `--quit` is the exception that proves the rule: it forgets the
            // sequence and deliberately leaves the tree exactly as it is.
            if !matches!(control, SequencerControl::Quit(_)) {
                assert!(
                    scope.worktree_state,
                    "{control:?} touches THIS worktree's HEAD/index/files"
                );
            }
        }
        // Every declared control has an operation-log identity, or boundary
        // recording could not name it.
        for control in SequencerControl::ALL {
            let (command, description) = control.describe_operation();
            assert!(!command.is_empty(), "{control:?} needs a command name");
            assert!(
                description.contains(&command),
                "{control:?} description should name its command: {description}"
            );
        }
        // Bisect never moves a branch; the sequence controls do.
        assert!(
            !SequencerControl::BisectMark
                .mutation_scope()
                .repository_refs,
            "bisect checks out candidates without moving refs"
        );
        assert!(
            SequencerControl::Continue(SequenceKind::Rebase)
                .mutation_scope()
                .repository_refs,
            "a rebase --continue can land commits on the branch"
        );
    }
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

    /// Every control the CLI can DISPATCH must be declared, or
    /// `begin_control_operation`'s debug assertion aborts the command.
    ///
    /// This is not hypothetical: `revert --skip` dispatched
    /// `Skip(SequenceKind::Revert)` while `ALL` listed only the cherry-pick and
    /// rebase skips, so a debug build panicked on a perfectly ordinary command.
    /// The declaration and the dispatch table have to be checked against each
    /// other, not merely each against itself.
    #[test]
    fn every_dispatchable_control_is_declared() {
        // The controls `cli::sequencer_control_for` can produce, by kind.
        let dispatched = [
            SequencerControl::Start(SequenceKind::CherryPick),
            SequencerControl::Continue(SequenceKind::CherryPick),
            SequencerControl::Skip(SequenceKind::CherryPick),
            SequencerControl::Abort(SequenceKind::CherryPick),
            SequencerControl::Quit(SequenceKind::CherryPick),
            SequencerControl::Start(SequenceKind::Revert),
            SequencerControl::Continue(SequenceKind::Revert),
            SequencerControl::Skip(SequenceKind::Revert),
            SequencerControl::Abort(SequenceKind::Revert),
            SequencerControl::Start(SequenceKind::Rebase),
            SequencerControl::Continue(SequenceKind::Rebase),
            SequencerControl::Skip(SequenceKind::Rebase),
            SequencerControl::Abort(SequenceKind::Rebase),
            SequencerControl::Start(SequenceKind::Merge),
            SequencerControl::Continue(SequenceKind::Merge),
            SequencerControl::Restart(SequenceKind::Merge),
            SequencerControl::Abort(SequenceKind::Merge),
            SequencerControl::AmStart,
            SequencerControl::AmContinue,
            SequencerControl::AmSkip,
            SequencerControl::AmAbort,
            SequencerControl::BisectStart,
            SequencerControl::BisectMark,
            SequencerControl::BisectSkip,
            SequencerControl::BisectReset,
            SequencerControl::BisectRun,
        ];
        for control in dispatched {
            assert!(
                SequencerControl::ALL.contains(&control),
                "{control:?} is dispatched by the CLI but missing from ALL — a debug build \
                 aborts on it"
            );
        }
        // SET EQUALITY, not one-way containment: a declaration nothing
        // dispatches is dead weight that hides a missing dispatch arm, and the
        // count check alone cannot tell the two apart.
        for control in SequencerControl::ALL {
            assert!(
                dispatched.contains(control),
                "{control:?} is declared but never dispatched — either wire it up or drop it"
            );
        }
        assert_eq!(
            dispatched.len(),
            SequencerControl::ALL.len(),
            "the dispatch list and the declaration must be the same size"
        );
    }

    /// §C.9: `worktree add/move/remove/repair/migrate` declare their mutation
    /// scope too — the plan names them in the same sentence as the sequencer
    /// controls, and a guard that enumerated only one family would pass while
    /// a worktree action went uncovered.
    #[test]
    fn every_worktree_control_declares_its_mutation_scope() {
        assert_eq!(
            WorktreeControl::ALL.len(),
            6,
            "the enumeration is the contract: add the new action here too"
        );
        for control in WorktreeControl::ALL {
            let scope = control.mutation_scope();
            assert!(
                scope.registry,
                "{control:?} is a registry mutation — that is what makes it a \
                 lifecycle action"
            );
            assert!(
                !control.command_name().is_empty(),
                "{control:?} needs a command name"
            );
        }
        // Only `add` can create a branch (`-b`); the rest never move a ref.
        assert!(WorktreeControl::Add.mutation_scope().repository_refs);
        for control in [
            WorktreeControl::Move,
            WorktreeControl::Remove,
            WorktreeControl::Prune,
            WorktreeControl::Repair,
            WorktreeControl::MigrateLayout,
        ] {
            assert!(
                !control.mutation_scope().repository_refs,
                "{control:?} must not move a branch"
            );
        }
        // `repair` restores identity FILES; it must not claim to rewrite the
        // scoped rows, which would license it to delete a worktree's HEAD.
        assert!(!WorktreeControl::Repair.mutation_scope().scoped_rows);
        // Journal ops are distinct — two actions sharing one would make crash
        // recovery replay the wrong one.
        let mut ops: Vec<&str> = WorktreeControl::ALL
            .iter()
            .map(|control| control.journal_op())
            .collect();
        ops.sort_unstable();
        let unique = ops.len();
        ops.dedup();
        assert_eq!(unique, ops.len(), "journal ops must be distinct: {ops:?}");
        // Every op string the journal can hold is declared — the direction
        // that matters, since crash recovery dispatches on those strings and
        // an op with no declaration is a replayable mutation outside the
        // inventory. The converse does not hold: `repair` declares but writes
        // no journal row, because each of its writes is a single atomic
        // rename with no crash window to roll forward.
        let mut declared: Vec<&str> = WorktreeControl::ALL
            .iter()
            .map(|control| control.journal_op())
            .collect();
        declared.sort_unstable();
        assert_eq!(
            declared,
            vec!["add", "migrate", "move", "prune", "remove", "repair"],
            "the declared journal ops must match what the journal can contain"
        );
    }

    /// §C.4.4: the start-time exclusion is the INSERT, not the check before
    /// it. Two starts racing in one worktree both pass `ensure_none_for_*`
    /// (nothing is in progress yet); if the first persistence were an upsert,
    /// the loser would silently replace the winner's todo while the winner's
    /// checkout stayed on disk. Exactly one claim may win.
    #[tokio::test]
    #[serial_test::serial]
    async fn only_one_start_can_claim_this_worktrees_sequence() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = ChangeDirGuard::new(tmp.path());
        setup_with_new_libra_in(tmp.path()).await;

        let first = sample(SequenceKind::CherryPick);
        claim_start(&first).await.expect("the first start wins");

        let mut second = sample(SequenceKind::Rebase);
        second.current_oid = "e".repeat(40);
        let refused = claim_start(&second)
            .await
            .expect_err("the second start must be refused, not silently applied");
        assert!(
            refused.contains("already in progress"),
            "the loser gets the ordinary refusal: {refused}"
        );

        // The winner's row is intact — this is the assertion an upsert fails.
        let loaded = load().await.expect("load").expect("present");
        assert_eq!(loaded.kind, SequenceKind::CherryPick);
        assert_eq!(loaded.current_oid, first.current_oid);

        // The OWNER can still advance its own sequence.
        let mut advanced = first.clone();
        advanced.current_oid = "f".repeat(40);
        save(&advanced).await.expect("the owner advances");
        assert_eq!(
            load().await.expect("load").expect("present").current_oid,
            "f".repeat(40)
        );
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
