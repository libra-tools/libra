//! Versioned schema migration runner — CEX-12.5 deliverable.
//!
//! Provides a single, reusable abstraction every future persistence-touching
//! CEX (CEX-13b ContextFrame, CEX-15 automation_log, CEX-16
//! `agent_usage_stats`, plus Step 2 `schema_versions` extensions) plugs into,
//! so we don't end up with four separate `CREATE TABLE IF NOT EXISTS` hacks
//! scattered across [`crate::internal::db`].
//!
//! # Concepts
//!
//! - [`Migration`] — one named, versioned schema change. Carries an `up`
//!   forward DDL and an optional `down` rollback DDL. The DDL **must be
//!   idempotent** at the SQL level (`CREATE TABLE IF NOT EXISTS`,
//!   `CREATE INDEX IF NOT EXISTS`) so re-running on a partially-applied
//!   database does not error.
//! - [`MigrationRunner`] — owns the registered migration set and applies
//!   pending migrations in monotonic version order. Tracks applied
//!   migrations in a dedicated `schema_versions` table.
//!
//! # Concurrency model
//!
//! All three operations (`run_pending` / `current_version` / `rollback_to`)
//! run inside a SQLite transaction so a crash mid-migration cannot leave the
//! database in an inconsistent state. SQLite serializes writers; concurrent
//! callers wait on the busy timeout already configured in
//! [`crate::internal::db::establish_connection_with_busy_timeout`].
//!
//! # Backward compatibility
//!
//! Pre-CEX-12.5 databases were initialized via `sqlite_20260309_init.sql`
//! plus the legacy `ensure_*_schema` helpers. CEX-12.5 keeps those paths
//! intact and adds the migration runner on top. The runner sees those
//! databases as "schema_version is empty" and applies any registered
//! migration whose `up` DDL is idempotent against the pre-existing tables.
//! Future CEXes only touch the runner — no new `ensure_*` helpers should be
//! added.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement, TransactionTrait};
use thiserror::Error;

/// One named, versioned schema change.
///
/// `up` is required; `down` is optional and only used by
/// [`MigrationRunner::rollback_to`]. Both DDL bodies are executed verbatim
/// inside the migration transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Migration {
    /// Monotonic version. Versions must be **strictly increasing** within a
    /// runner; duplicate or out-of-order registrations are rejected at
    /// register time.
    pub version: i64,

    /// Human-readable name shown in the `schema_versions` table and audit
    /// logs. Should match the `<version>_<name>` filename if the migration
    /// is loaded from `sql/migrations/`.
    pub name: &'static str,

    /// Forward DDL. Must be idempotent (use `IF NOT EXISTS` for tables /
    /// indexes; tolerate columns that already exist).
    pub up: &'static str,

    /// Optional rollback DDL for [`MigrationRunner::rollback_to`]. `None`
    /// means the migration is forward-only; calling `rollback_to` past such
    /// a migration returns [`MigrationError::IrreversibleMigration`].
    pub down: Option<&'static str>,
}

/// Errors raised by the migration runner.
#[derive(Debug, Error)]
pub enum MigrationError {
    /// Two registered migrations share the same `version`. The runner does
    /// not auto-resolve this; the caller must rename one.
    #[error("duplicate migration version {version} (existing name: {existing}, new name: {new})")]
    DuplicateVersion {
        version: i64,
        existing: &'static str,
        new: &'static str,
    },

    /// A migration was registered with a version smaller than or equal to
    /// the previous one. The runner requires monotonic registration so
    /// `applied_at` ordering matches version ordering.
    #[error(
        "migration versions must be strictly increasing; got {new_version} ({new_name}) after {prev_version} ({prev_name})"
    )]
    NonMonotonicRegistration {
        prev_version: i64,
        prev_name: &'static str,
        new_version: i64,
        new_name: &'static str,
    },

    /// `rollback_to` reached a migration without a `down` DDL.
    #[error("migration {version} ({name}) has no down DDL — cannot rollback past it")]
    IrreversibleMigration { version: i64, name: &'static str },

    /// `rollback_to(target)` was called but `target` is greater than the
    /// current version (i.e. there's nothing to roll back).
    #[error("rollback target {target} is at or above current version {current}")]
    RollbackTargetNotBelowCurrent { target: i64, current: i64 },

    /// `rollback_to(target)` was called on a database with no applied
    /// migrations. Distinct from [`Self::RollbackTargetNotBelowCurrent`]
    /// (which compares against a real `current` version) so callers — and
    /// future migrations using legitimate negative version numbers — can
    /// distinguish "empty database" from "rollback target too high"
    /// without colliding on a sentinel `current` value.
    #[error("rollback target {target} requested but no migrations are applied")]
    RollbackOnEmptyDatabase { target: i64 },

    /// A SQL operation failed.
    #[error("database error: {0}")]
    Database(#[from] DbErr),

    /// A higher-level wrapper for context-rich failures (e.g.
    /// "could not insert into schema_versions").
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

/// SQL bootstrap for the `schema_versions` tracking table.
///
/// Idempotent: safe to run on every connect. Stored as a `&'static str` so
/// the runner has a single source of truth.
const SCHEMA_VERSIONS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS `schema_versions` (
    `version` INTEGER PRIMARY KEY,
    `name` TEXT NOT NULL,
    `applied_at` TEXT NOT NULL
);
"#;

/// Versioned schema migration runner.
///
/// Build one with [`MigrationRunner::new`], register migrations via
/// [`MigrationRunner::register`], then call
/// [`MigrationRunner::run_pending`] to apply everything pending against a
/// live `DatabaseConnection`.
///
/// The runner is **registration-time** validated — duplicate versions and
/// non-monotonic insertions error out before any SQL runs.
#[derive(Default, Debug)]
pub struct MigrationRunner {
    migrations: Vec<Migration>,
}

impl MigrationRunner {
    /// Create an empty runner. Callers register migrations explicitly via
    /// [`MigrationRunner::register`] (or [`MigrationRunner::extend`]).
    pub fn new() -> Self {
        Self {
            migrations: Vec::new(),
        }
    }

    /// Register a single migration. Returns
    /// [`MigrationError::DuplicateVersion`] if a migration with the same
    /// version is already registered, or
    /// [`MigrationError::NonMonotonicRegistration`] if `version` is not
    /// strictly greater than the most-recent registered version.
    pub fn register(&mut self, migration: Migration) -> Result<(), MigrationError> {
        if let Some(prev) = self.migrations.last() {
            if migration.version == prev.version {
                return Err(MigrationError::DuplicateVersion {
                    version: migration.version,
                    existing: prev.name,
                    new: migration.name,
                });
            }
            if migration.version <= prev.version {
                return Err(MigrationError::NonMonotonicRegistration {
                    prev_version: prev.version,
                    prev_name: prev.name,
                    new_version: migration.version,
                    new_name: migration.name,
                });
            }
        }
        // Also catch duplicates anywhere earlier in the list (not just
        // adjacent), since callers may register out-of-order then expect
        // the runner to sort. We choose strict-monotonic-only above; this
        // additional sweep is belt-and-braces.
        if let Some(existing) = self
            .migrations
            .iter()
            .find(|m| m.version == migration.version)
        {
            return Err(MigrationError::DuplicateVersion {
                version: migration.version,
                existing: existing.name,
                new: migration.name,
            });
        }
        self.migrations.push(migration);
        Ok(())
    }

    /// Register many migrations in order. Stops at the first error and
    /// returns it; previously-accepted migrations stay in the runner.
    pub fn extend<I>(&mut self, migrations: I) -> Result<(), MigrationError>
    where
        I: IntoIterator<Item = Migration>,
    {
        for migration in migrations {
            self.register(migration)?;
        }
        Ok(())
    }

    /// Number of registered migrations. Diagnostics-only.
    pub fn len(&self) -> usize {
        self.migrations.len()
    }

    /// `true` when no migrations are registered.
    pub fn is_empty(&self) -> bool {
        self.migrations.is_empty()
    }

    /// Highest registered version, or `None` for an empty runner.
    pub fn max_registered_version(&self) -> Option<i64> {
        self.migrations.last().map(|m| m.version)
    }

    /// Read the highest applied version from `schema_versions`. Returns
    /// `Ok(None)` for a fresh database (or one initialized before
    /// CEX-12.5).
    pub async fn current_version(
        &self,
        conn: &DatabaseConnection,
    ) -> Result<Option<i64>, MigrationError> {
        ensure_schema_versions_table(conn).await?;
        max_schema_version(conn).await
    }

    /// Read the highest applied version without creating or mutating
    /// `schema_versions`.
    ///
    /// This is the preflight path for normal CLI commands: when a newer Libra
    /// binary sees an older repository, the check must report "upgrade
    /// required" instead of silently creating tracking tables or applying
    /// migrations.
    pub async fn current_version_readonly(
        &self,
        conn: &DatabaseConnection,
    ) -> Result<Option<i64>, MigrationError> {
        if !schema_versions_table_exists(conn).await? {
            return Ok(None);
        }
        max_schema_version(conn).await
    }

    /// Apply every registered migration whose version is greater than the
    /// current applied version. Each migration runs inside its own
    /// transaction, with both the `up` DDL and the `schema_versions` row
    /// insert atomic together.
    ///
    /// Returns the list of versions that were newly applied **by this
    /// call** (empty when the database is already up to date, or when a
    /// concurrent process beat us to every pending migration).
    /// Migrations that lost the race in `INSERT OR IGNORE` are NOT
    /// included in the return value even though their up-DDL ran
    /// idempotently.
    pub async fn run_pending(&self, conn: &DatabaseConnection) -> Result<Vec<i64>, MigrationError> {
        ensure_schema_versions_table(conn).await?;
        let current = self.current_version(conn).await?;
        let mut applied = Vec::new();

        for migration in &self.migrations {
            if let Some(current) = current
                && migration.version <= current
            {
                continue;
            }
            // No pre-flight `migration_already_applied` check here: that
            // would be a TOCTOU race with concurrent processes (Codex r1
            // P1#2). `apply_one_migration` uses `INSERT OR IGNORE` and
            // reports whether this call actually wrote the row.
            let inserted = apply_one_migration(conn, migration).await?;
            if inserted {
                applied.push(migration.version);
            }
        }

        Ok(applied)
    }

    /// Roll the schema back to `target` by running each migration's `down`
    /// DDL in reverse version order. Errors with
    /// [`MigrationError::IrreversibleMigration`] if any migration in the
    /// rollback range has no `down` DDL.
    ///
    /// `target` must be strictly less than the current applied version;
    /// passing the same or a larger value returns
    /// [`MigrationError::RollbackTargetNotBelowCurrent`].
    ///
    /// **Atomicity** (Codex r1 P1#8 fix): the rollback plan is
    /// pre-validated before any `down` DDL runs. If any migration in the
    /// `(target, current]` range is irreversible (no `down` DDL), the
    /// runner returns [`MigrationError::IrreversibleMigration`] **without
    /// having executed any down migration**, so the database stays in a
    /// known good state. Per-migration down DDL still runs in its own
    /// transaction so a SQL-level failure mid-plan rolls back only that
    /// step; surrounding successful down migrations stay applied (and
    /// removed from `schema_versions`). Callers that need full
    /// transactional rollback across multiple versions can wrap the call
    /// in their own SQLite `BEGIN ... COMMIT`.
    ///
    /// **Concurrency** (Codex r5 P1#3 fix): the returned `Vec` lists only
    /// the versions that **this call** rolled back. When two callers race
    /// `rollback_to` against the same database, each version is owned by
    /// exactly one caller (the one whose `DELETE FROM schema_versions`
    /// reports `changes() = 1`); the loser sees `changes() = 0` and skips
    /// the down DDL entirely, so no down DDL ever runs twice for the
    /// same version. The returned `Vec` for the loser may therefore be a
    /// strict subset of `(target, current]`.
    ///
    /// A caller whose initial current-version read is scheduled only after
    /// another caller has emptied the registry still receives
    /// [`MigrationError::RollbackOnEmptyDatabase`]. At that point it did not
    /// observe an applied version and is indistinguishable from an ordinary
    /// rollback request against an empty database.
    pub async fn rollback_to(
        &self,
        conn: &DatabaseConnection,
        target: i64,
    ) -> Result<Vec<i64>, MigrationError> {
        ensure_schema_versions_table(conn).await?;
        let current = self
            .current_version(conn)
            .await?
            .ok_or(MigrationError::RollbackOnEmptyDatabase { target })?;
        if target >= current {
            return Err(MigrationError::RollbackTargetNotBelowCurrent { target, current });
        }

        // Phase 1: build the plan and pre-validate. Collect every migration
        // in `(target, current]` and bail early if any of them lacks a
        // `down` DDL. This guarantees we never partially roll back a
        // multi-version range only to discover an irreversible step
        // halfway through.
        let mut plan: Vec<(&Migration, &'static str)> = Vec::new();
        for migration in self.migrations.iter().rev() {
            if migration.version <= target {
                break;
            }
            if migration.version > current {
                continue;
            }
            let down = migration
                .down
                .ok_or(MigrationError::IrreversibleMigration {
                    version: migration.version,
                    name: migration.name,
                })?;
            plan.push((migration, down));
        }

        // Phase 2: execute the validated plan in order. The
        // irreversible-migration class of error is no longer possible at
        // this point; only SQL-level failures from the `down` DDL itself
        // can surface here. `apply_down_migration` returns whether THIS
        // call owned the version (won the DELETE race); concurrent
        // rollback loser sees `false` and we skip pushing it to the
        // result Vec — symmetric to `run_pending`'s INSERT OR IGNORE
        // semantics.
        let mut rolled_back = Vec::new();
        for (migration, down) in plan {
            let owned = apply_down_migration(conn, migration.version, down).await?;
            if owned {
                rolled_back.push(migration.version);
            }
        }
        Ok(rolled_back)
    }
}

/// Idempotent DDL for the `schema_versions` table; safe to call on every
/// connect. The runner invokes this before any read or write of the
/// version column.
async fn ensure_schema_versions_table(conn: &DatabaseConnection) -> Result<(), MigrationError> {
    let backend = conn.get_database_backend();
    conn.execute(Statement::from_string(backend, SCHEMA_VERSIONS_DDL))
        .await?;
    Ok(())
}

async fn schema_versions_table_exists(conn: &DatabaseConnection) -> Result<bool, MigrationError> {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM sqlite_master WHERE type = ? AND name = ? LIMIT 1",
            ["table".into(), "schema_versions".into()],
        ))
        .await?;
    Ok(row.is_some())
}

async fn max_schema_version(conn: &DatabaseConnection) -> Result<Option<i64>, MigrationError> {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT MAX(version) FROM schema_versions",
        ))
        .await?;
    let Some(row) = row else { return Ok(None) };
    // SQLite returns NULL for `MAX(version)` when the table is empty; sea-orm
    // surfaces that as `Option<i64>`. We forward the None.
    // CONTRACT (Codex r5 P1#1): decode failures must propagate — a type-drifted
    // `version` column would otherwise silently report "empty registry" and
    // trigger a re-run of every migration.
    let version: Option<i64> = row.try_get_by_index(0).map_err(|err| {
        MigrationError::Database(DbErr::Custom(format!(
            "schema_versions.version decode failed: {err}"
        )))
    })?;
    Ok(version)
}

/// Apply one migration atomically. Returns `true` when this call inserted
/// the version row, `false` when another concurrent process beat us to it
/// (Codex r1 P1#2 fix: replaces the TOCTOU `migration_already_applied`
/// check + plain `INSERT` with a race-free `INSERT OR IGNORE` reading the
/// resulting `changes()` to disambiguate "we wrote it" from "someone else
/// already had").
async fn apply_one_migration(
    conn: &DatabaseConnection,
    migration: &Migration,
) -> Result<bool, MigrationError> {
    let now: DateTime<Utc> = Utc::now();
    let inserted = conn
        .transaction::<_, _, DbErr>(|txn| {
            let version = migration.version;
            let name = migration.name;
            let up = migration.up;
            let applied_at = now.to_rfc3339();
            Box::pin(async move {
                let backend = txn.get_database_backend();
                // Apply the user DDL first; if it fails the schema_versions
                // insert never happens and the transaction rolls back.
                txn.execute(Statement::from_string(backend, up)).await?;
                // `INSERT OR IGNORE` plus `changes()` lets us tell whether
                // we won the race or not without a separate read query.
                // SQLite's `changes()` reports rows changed by the LAST
                // INSERT/UPDATE/DELETE on this connection, so we read it
                // immediately after the insert, still inside the
                // transaction.
                txn.execute(Statement::from_sql_and_values(
                    backend,
                    "INSERT OR IGNORE INTO schema_versions (version, name, applied_at) VALUES (?, ?, ?)",
                    [version.into(), name.into(), applied_at.into()],
                ))
                .await?;
                let row = txn
                    .query_one(Statement::from_string(backend, "SELECT changes()"))
                    .await?
                    .ok_or_else(|| {
                        DbErr::Custom("SELECT changes() returned no row".to_string())
                    })?;
                let changed: i64 = row
                    .try_get_by_index(0)
                    .map_err(|err| DbErr::Custom(format!("changes() decode failed: {err}")))?;
                Ok::<bool, DbErr>(changed > 0)
            })
        })
        .await
        .map_err(|err| match err {
            sea_orm::TransactionError::Connection(db) => MigrationError::Database(db),
            sea_orm::TransactionError::Transaction(db) => MigrationError::Database(db),
        })?;
    Ok(inserted)
}

/// Apply one migration's down DDL atomically. Returns `true` when this
/// call owned the rollback (its `DELETE` removed the row), `false` when
/// another concurrent process beat it to the deletion (Codex r5 P1#3
/// fix: symmetric to `apply_one_migration`'s INSERT OR IGNORE / changes()
/// semantics — DELETE first, then run the down DDL only if we won the
/// race, so the down DDL never executes twice for the same version under
/// concurrent rollback).
async fn apply_down_migration(
    conn: &DatabaseConnection,
    version: i64,
    down: &'static str,
) -> Result<bool, MigrationError> {
    let owned = conn
        .transaction::<_, _, DbErr>(|txn| {
            Box::pin(async move {
                let backend = txn.get_database_backend();
                // DELETE first — the row's presence is our ownership
                // claim for this rollback. SQLite's `changes()` reports
                // the rows affected by the LAST INSERT/UPDATE/DELETE on
                // this connection, so we read it immediately after the
                // delete, still inside the transaction.
                txn.execute(Statement::from_sql_and_values(
                    backend,
                    "DELETE FROM schema_versions WHERE version = ?",
                    [version.into()],
                ))
                .await?;
                let row = txn
                    .query_one(Statement::from_string(backend, "SELECT changes()"))
                    .await?
                    .ok_or_else(|| DbErr::Custom("SELECT changes() returned no row".to_string()))?;
                let changed: i64 = row
                    .try_get_by_index(0)
                    .map_err(|err| DbErr::Custom(format!("changes() decode failed: {err}")))?;
                if changed == 0 {
                    // Another caller already rolled this version back.
                    // Skip the down DDL — running it twice is exactly the
                    // bug this fix prevents.
                    return Ok::<bool, DbErr>(false);
                }
                // We own the rollback for this version. Execute the down
                // DDL inside the same transaction so a SQL failure rolls
                // both the DELETE and the partial DDL back.
                txn.execute(Statement::from_string(backend, down)).await?;
                Ok::<bool, DbErr>(true)
            })
        })
        .await
        .map_err(|err| match err {
            sea_orm::TransactionError::Connection(db) => MigrationError::Database(db),
            sea_orm::TransactionError::Transaction(db) => MigrationError::Database(db),
        })?;
    Ok(owned)
}

/// Returns the canonical migration set the libra runtime registers on every
/// connect. CEX-15 and CEX-16 now add migrations here in version order.
/// Keeping the set centralised in this function (rather than in
/// `establish_connection`) makes it trivial to test the wiring against an
/// isolated runner.
pub fn builtin_migrations() -> Vec<Migration> {
    vec![
        sql_migration(
            2026050301,
            "automation_log",
            include_str!("../../../sql/migrations/2026050301_automation_log.sql"),
            include_str!("../../../sql/migrations/2026050301_automation_log_down.sql"),
        ),
        sql_migration(
            2026050302,
            "agent_usage_stats",
            include_str!("../../../sql/migrations/2026050302_agent_usage_stats.sql"),
            include_str!("../../../sql/migrations/2026050302_agent_usage_stats_down.sql"),
        ),
        // CEX-EntireIO Phase 1.1: external-agent capture catalog. Uses
        // `include_str!` to keep DDL out of the Rust file — the path resolves
        // from `src/internal/db/migration.rs` (three `..` segments to repo
        // root, then descend into `sql/migrations/`).
        sql_migration(
            2026050303,
            "agent_capture",
            include_str!("../../../sql/migrations/2026050303_agent_capture.sql"),
            include_str!("../../../sql/migrations/2026050303_agent_capture_down.sql"),
        ),
        // CEX-EntireIO Phase 2.1 follow-up: relax `agent_checkpoint.parent_commit`
        // to NULLable so the runtime can distinguish "user branch unborn / no
        // HEAD" from "lookup error" — see Codex review round 1 NEEDS-CHANGES.
        sql_migration(
            2026050501,
            "agent_checkpoint_parent_nullable",
            include_str!("../../../sql/migrations/2026050501_agent_checkpoint_parent_nullable.sql"),
            include_str!(
                "../../../sql/migrations/2026050501_agent_checkpoint_parent_nullable_down.sql"
            ),
        ),
        // OC-Phase 2 P2.5: persistent `Always`-reply ruleset, populated when
        // a user clicks "Always" on a permission prompt and reloaded on the
        // next session. See docs/development/commands/_general.md "Permission Ruleset
        // 与 Approval 反馈协议".
        sql_migration(
            2026050601,
            "approved_permission",
            include_str!("../../../sql/migrations/2026050601_approved_permission.sql"),
            include_str!("../../../sql/migrations/2026050601_approved_permission_down.sql"),
        ),
        // OC-Phase 5 P5.2: add the `agent_name` dimension to
        // `agent_usage_stats` so the multi-agent runtime can attribute spend
        // to a specific agent profile (planner / explorer / reviewer / …)
        // on top of the existing (provider, model) aggregation. Additive;
        // legacy rows keep `agent_name = NULL` and remain queryable through
        // the existing indexes. See docs/development/commands/_general.md OC-Phase 5
        // P5.2.
        sql_migration(
            2026050801,
            "agent_usage_stats_agent_name",
            include_str!("../../../sql/migrations/2026050801_agent_usage_stats_agent_name.sql"),
            include_str!(
                "../../../sql/migrations/2026050801_agent_usage_stats_agent_name_down.sql"
            ),
        ),
        // v0.17.800 source telemetry persistence: new
        // `source_call_log` table that mirrors the in-memory
        // `SourceCallLog::records` Vec<SourceCallRecord> shape with
        // a UUID primary key + created_at timestamp. Producer wire-up
        // (replacing the Mutex<Vec> store with a SeaORM-backed
        // recorder) lands in a follow-up; this migration is the
        // schema-side prerequisite so the producer change doesn't
        // need to register the migration itself. See agent.md
        // Storage / migration row for the gap this closes.
        sql_migration(
            2026052301,
            "source_call_log",
            include_str!("../../../sql/migrations/2026052301_source_call_log.sql"),
            include_str!("../../../sql/migrations/2026052301_source_call_log_down.sql"),
        ),
        // Phase 4 completion: the formal final `Decision` artifact table,
        // closing the ValidationReport -> RiskScoreBreakdown ->
        // DecisionProposal -> Decision chain. Mirrors `ai_decision_proposal`
        // (per-thread latest pointer). See docs/development/tracing/agent.md
        // Implementation Phase 4.
        sql_migration(
            2026053101,
            "ai_final_decision",
            include_str!("../../../sql/migrations/2026053101_ai_final_decision.sql"),
            include_str!("../../../sql/migrations/2026053101_ai_final_decision_down.sql"),
        ),
        sql_migration(
            2026060201,
            "source_call_log_agent_run_id",
            include_str!("../../../sql/migrations/2026060201_source_call_log_agent_run_id.sql"),
            include_str!(
                "../../../sql/migrations/2026060201_source_call_log_agent_run_id_down.sql"
            ),
        ),
        sql_migration(
            2026060401,
            "cherry_pick_state",
            include_str!("../../../sql/migrations/2026060401_cherry_pick_state.sql"),
            include_str!("../../../sql/migrations/2026060401_cherry_pick_state_down.sql"),
        ),
        sql_migration(
            2026060801,
            "revert_sequence",
            include_str!("../../../sql/migrations/2026060801_revert_sequence.sql"),
            include_str!("../../../sql/migrations/2026060801_revert_sequence_down.sql"),
        ),
        // Phase 1.12: persistent `notes` table for `libra notes` add/show/list/remove.
        sql_migration(
            2026061401,
            "notes",
            include_str!("../../../sql/migrations/2026061401_notes.sql"),
            include_str!("../../../sql/migrations/2026061401_notes_down.sql"),
        ),
        // 2026-06-23: rename the external-agent capture ref from the legacy
        // `agent-traces` branch to the single-word `traces` (refs/libra/traces).
        // Renames the existing `reference` row (and any reflog history) so repos
        // created before the rename keep their captured checkpoint history under
        // the new name. Conflict-safe + idempotent — see
        // `src/internal/branch.rs` (`TRACES_BRANCH` / `LEGACY_TRACES_BRANCH`)
        // and docs/development/tracing/agent.md.
        sql_migration(
            2026062301,
            "rename_agent_traces_branch",
            include_str!("../../../sql/migrations/2026062301_rename_agent_traces_branch.sql"),
            include_str!("../../../sql/migrations/2026062301_rename_agent_traces_branch_down.sql"),
        ),
        // 2026-07-02: unified scoped metadata KV table (lore.md 1.5) — the
        // single store for branch (and future scoped) metadata; protect /
        // archive / lineage.* are keys here, never separate tables. Repo-scope
        // metadata intentionally lives in config_kv under `metadata.*`.
        // Owner API: `internal::metadata::MetadataKv` (the only writer/reader).
        sql_migration(
            2026070201,
            "metadata_kv",
            include_str!("../../../sql/migrations/2026070201_metadata_kv.sql"),
            include_str!("../../../sql/migrations/2026070201_metadata_kv_down.sql"),
        ),
        // 2026-07-02: dirty-set cache (lore.md 1.1) — advisory working-tree
        // dirty snapshot + staged set, rebuilt by `status --scan`, consumed by
        // the opt-in `status --cached`/`--check-dirty`/`libra dirty` surfaces
        // only. Default `status` never touches it; freshness keys on the index
        // fingerprint + HEAD OID. Owner API: `internal::dirty::DirtyCache`.
        sql_migration(
            2026070202,
            "working_dirty",
            include_str!("../../../sql/migrations/2026070202_working_dirty.sql"),
            include_str!("../../../sql/migrations/2026070202_working_dirty_down.sql"),
        ),
        // 2026-07-03: revision ordinal index (lore.md 1.16) — rebuildable
        // OID<->ordinal mapping over per-ref first-parent chains, freshness
        // fingerprinted on tip OID + refs/replace digest. Owner API:
        // `internal::revision_ordinal::RevisionOrdinalIndex`.
        sql_migration(
            2026070301,
            "revision_ordinal",
            include_str!("../../../sql/migrations/2026070301_revision_ordinal.sql"),
            include_str!("../../../sql/migrations/2026070301_revision_ordinal_down.sql"),
        ),
        // lore.md 2.6: unified sequencer state (`sequence_state`). Folds the
        // in-progress cherry-pick forward, retires cherry-pick's lazy DDL and
        // the `revert_sequence` orphan. Owner: `internal::sequencer`.
        sql_migration(
            2026070401,
            "sequence_state",
            include_str!("../../../sql/migrations/2026070401_sequence_state.sql"),
            include_str!("../../../sql/migrations/2026070401_sequence_state_down.sql"),
        ),
        // lore.md 2.4: Lore's `layer` local-overlay primitive. Owner:
        // `internal::layer::LayerStore`. Never serialized into a commit.
        sql_migration(
            2026070501,
            "layer",
            include_str!("../../../sql/migrations/2026070501_layer.sql"),
            include_str!("../../../sql/migrations/2026070501_layer_down.sql"),
        ),
        // lore.md 2.5: index-flagged obliteration tombstone registry. Owner:
        // `internal::obliteration::ObliterationStore`.
        sql_migration(
            2026070601,
            "object_obliteration",
            include_str!("../../../sql/migrations/2026070601_object_obliteration.sql"),
            include_str!("../../../sql/migrations/2026070601_object_obliteration_down.sql"),
        ),
        // lore.md 2.2: read-only sparse view include patterns. Owner:
        // `internal::sparse::SparseViewStore`.
        sql_migration(
            2026070701,
            "sparse_view",
            include_str!("../../../sql/migrations/2026070701_sparse_view.sql"),
            include_str!("../../../sql/migrations/2026070701_sparse_view_down.sql"),
        ),
        // lore.md 2.1: per-worktree HEAD/index/HEAD-reflog isolation — adds a
        // nullable `worktree_id` scoping column to `reference` and `reflog`.
        sql_migration(
            2026070801,
            "worktree_isolation",
            include_str!("../../../sql/migrations/2026070801_worktree_isolation.sql"),
            include_str!("../../../sql/migrations/2026070801_worktree_isolation_down.sql"),
        ),
        // AG-20 (plan.md Task A5): `agent_checkpoint.traces_commit` probe
        // index (deliberately NON-unique — see the .sql header for the
        // brick-avoidance rationale) plus keyset pagination indexes for
        // `agent session list` / `agent checkpoint list`.
        sql_migration(
            2026070802,
            "agent_checkpoint_paging",
            include_str!("../../../sql/migrations/2026070802_agent_checkpoint_paging.sql"),
            include_str!("../../../sql/migrations/2026070802_agent_checkpoint_paging_down.sql"),
        ),
        // AG-24a (plan.md Task A8.5): append-only `agent_audit_log` for raw
        // checkpoint access/export. The `_down` deliberately preserves audit
        // data (freezes writes rather than dropping) — see the .sql headers.
        sql_migration(
            2026070803,
            "agent_audit_log",
            include_str!("../../../sql/migrations/2026070803_agent_audit_log.sql"),
            include_str!("../../../sql/migrations/2026070803_agent_audit_log_down.sql"),
        ),
        // plan-20260713 DR-05c-0 (M1): per-turn coverage claim/revision gate.
        // `agent_coverage_claim` is the write-front idempotence gate every
        // checkpoint writer (live now, import in M4) must pass before
        // appending to `refs/libra/traces`; `agent_coverage_revision` is the
        // append-only per-turn version history that carries supersede
        // relations (never `agent_checkpoint` — ADR-DR-16).
        sql_migration(
            2026071301,
            "agent_coverage_gate",
            include_str!("../../../sql/migrations/2026071301_agent_coverage_gate.sql"),
            include_str!("../../../sql/migrations/2026071301_agent_coverage_gate_down.sql"),
        ),
        // plan-20260713 DR-04b (M3): OpenCode export-bridge job state —
        // observed/processed generation counters + owner/fence lease + TTL
        // (ADR-DR-11). Cleanup is TTL/app-driven, never session cascade.
        sql_migration(
            2026071401,
            "agent_export_job",
            include_str!("../../../sql/migrations/2026071401_agent_export_job.sql"),
            include_str!("../../../sql/migrations/2026071401_agent_export_job_down.sql"),
        ),
    ]
}

fn sql_migration(
    version: i64,
    name: &'static str,
    up: &'static str,
    down: &'static str,
) -> Migration {
    Migration {
        version,
        name,
        up,
        down: Some(down),
    }
}

/// Convenience: build a runner pre-loaded with [`builtin_migrations`].
///
/// **Returns `Result`**: a future CEX adding a duplicate or non-monotonic
/// version to `builtin_migrations()` would otherwise produce a partial
/// registry without surfacing the registration error. Tests in
/// `tests/db_migration_test.rs` exercise this path so registration mistakes
/// fail fast in CI rather than at first-use of the missing migration.
pub fn builtin_runner() -> Result<MigrationRunner, MigrationError> {
    let mut runner = MigrationRunner::new();
    runner.extend(builtin_migrations())?;
    Ok(runner)
}

/// Highest schema version this Libra build knows how to create.
pub fn latest_builtin_schema_version() -> Result<Option<i64>, MigrationError> {
    Ok(builtin_runner()?.max_registered_version())
}

/// Read the current built-in schema version without mutating the database.
pub async fn current_builtin_schema_version_readonly(
    conn: &DatabaseConnection,
) -> Result<Option<i64>, MigrationError> {
    builtin_runner()?.current_version_readonly(conn).await
}

/// Run all built-in migrations on the given connection. This is the
/// canonical entry point used by [`crate::internal::db::establish_connection`]
/// (and by tests that want the same wiring as production). Both registry-
/// build errors and per-migration apply errors are surfaced through
/// `anyhow::Error` so the call site can attach its own context.
pub async fn run_builtin_migrations(conn: &DatabaseConnection) -> Result<Vec<i64>> {
    let runner =
        builtin_runner().with_context(|| "failed to build the built-in migration registry")?;
    runner
        .run_pending(conn)
        .await
        .with_context(|| "failed to run built-in schema migrations")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_rejects_duplicate_version() {
        let mut runner = MigrationRunner::new();
        runner
            .register(Migration {
                version: 1,
                name: "first",
                up: "",
                down: None,
            })
            .unwrap();
        let err = runner
            .register(Migration {
                version: 1,
                name: "first_again",
                up: "",
                down: None,
            })
            .unwrap_err();
        assert!(matches!(
            err,
            MigrationError::DuplicateVersion {
                version: 1,
                existing: "first",
                new: "first_again",
            }
        ));
    }

    #[test]
    fn register_rejects_non_monotonic_version() {
        let mut runner = MigrationRunner::new();
        runner
            .register(Migration {
                version: 5,
                name: "later",
                up: "",
                down: None,
            })
            .unwrap();
        let err = runner
            .register(Migration {
                version: 3,
                name: "earlier",
                up: "",
                down: None,
            })
            .unwrap_err();
        assert!(matches!(
            err,
            MigrationError::NonMonotonicRegistration { .. }
        ));
    }

    #[test]
    fn empty_runner_max_registered_version_is_none() {
        let runner = MigrationRunner::new();
        assert_eq!(runner.max_registered_version(), None);
        assert!(runner.is_empty());
        assert_eq!(runner.len(), 0);
    }

    #[test]
    fn builtin_runner_registers_current_builtin_migrations() {
        // Bump this assertion whenever a new migration is registered in
        // `builtin_migrations()` so silent registry regressions surface
        // here in addition to `tests/db_migration_test.rs`.
        let runner = builtin_runner().expect("CEX-12.5 builtin registry must build clean");
        assert_eq!(runner.len(), 25);
        assert!(!runner.is_empty());
        assert_eq!(runner.max_registered_version(), Some(2026071401));
    }

    #[test]
    fn builtin_runner_propagates_registration_errors() {
        // Codex r1 P1#1 fix regression guard: changing `builtin_runner` to
        // return `Result` (instead of silently dropping registration
        // errors) means a future CEX that introduces a duplicate version
        // is caught at registry-build time rather than at first-use of a
        // missing migration. We synthesise a duplicate inline so this test
        // remains independent from the current built-in registry contents.
        let mut runner = MigrationRunner::new();
        runner
            .register(Migration {
                version: 1,
                name: "first",
                up: "",
                down: None,
            })
            .unwrap();
        let err = runner
            .extend(vec![Migration {
                version: 1,
                name: "first_again",
                up: "",
                down: None,
            }])
            .unwrap_err();
        assert!(matches!(err, MigrationError::DuplicateVersion { .. }));
    }

    #[test]
    fn migration_error_display_pins_owned_variants() {
        assert_eq!(
            MigrationError::DuplicateVersion {
                version: 3,
                existing: "schema_versions",
                new: "schema_versions_again",
            }
            .to_string(),
            "duplicate migration version 3 \
             (existing name: schema_versions, new name: schema_versions_again)",
        );
        assert_eq!(
            MigrationError::NonMonotonicRegistration {
                prev_version: 7,
                prev_name: "add_refs",
                new_version: 5,
                new_name: "add_objects",
            }
            .to_string(),
            "migration versions must be strictly increasing; \
             got 5 (add_objects) after 7 (add_refs)",
        );
        assert_eq!(
            MigrationError::IrreversibleMigration {
                version: 4,
                name: "drop_legacy",
            }
            .to_string(),
            "migration 4 (drop_legacy) has no down DDL — cannot rollback past it",
        );
        assert_eq!(
            MigrationError::RollbackTargetNotBelowCurrent {
                target: 9,
                current: 8,
            }
            .to_string(),
            "rollback target 9 is at or above current version 8",
        );
        assert_eq!(
            MigrationError::RollbackOnEmptyDatabase { target: 2 }.to_string(),
            "rollback target 2 requested but no migrations are applied",
        );
    }
}
