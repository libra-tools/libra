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
//!   forward DDL and an optional `down` rollback DDL. The DDL **should be
//!   idempotent** at the SQL level (`CREATE TABLE IF NOT EXISTS`,
//!   `CREATE INDEX IF NOT EXISTS`) so re-running against a pre-existing
//!   table does not error; non-idempotent RENAME-based rebuilds
//!   (2026072101/2026072301) are safe only because the runner claims the
//!   version row before executing the DDL, guaranteeing single
//!   application even under concurrent upgraders.
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

use std::collections::BTreeSet;

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

    /// Forward DDL. Should be idempotent (use `IF NOT EXISTS` for tables /
    /// indexes; tolerate columns that already exist). Non-idempotent
    /// RENAME-based rebuilds are permitted because the runner claims the
    /// `schema_versions` row before running the DDL (claim-first), so the
    /// DDL never executes twice.
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
    /// Concurrency: each migration claims its `schema_versions` row FIRST
    /// (`INSERT OR IGNORE` + `changes()`); a caller that loses the claim
    /// SKIPS that migration's up-DDL entirely — required because
    /// RENAME-based rebuilds (2026072101/2026072301) are not idempotent —
    /// and does not include it in the return value.
    pub async fn run_pending(&self, conn: &DatabaseConnection) -> Result<Vec<i64>, MigrationError> {
        self.run_pending_with_post_read_gate(conn, || async {})
            .await
    }

    /// Test seam for deterministic concurrency coverage: identical to
    /// [`Self::run_pending`], except `gate` runs once immediately AFTER the
    /// current-version read and BEFORE the first claim. Racing callers can
    /// rendezvous in `gate` so both hold the same pending list, forcing
    /// every subsequent version claim to be contended (the exact window the
    /// claim-first ordering exists for). Production code must call
    /// [`Self::run_pending`].
    #[doc(hidden)]
    pub async fn run_pending_with_post_read_gate<F, Fut>(
        &self,
        conn: &DatabaseConnection,
        gate: F,
    ) -> Result<Vec<i64>, MigrationError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        ensure_schema_versions_table(conn).await?;
        let current = self.current_version(conn).await?;
        gate().await;
        let mut applied = Vec::new();

        for migration in &self.migrations {
            if let Some(current) = current
                && migration.version <= current
            {
                continue;
            }
            // A guard SQL cannot express (§C.4.3): the layer/sparse scope
            // migrations decide "main-only" from scoped rows, which a linked
            // worktree removed BEFORE the migration no longer has.
            // No pre-flight `migration_already_applied` check here: that
            // would be a TOCTOU race with concurrent processes (Codex r1
            // P1#2). `apply_one_migration` uses `INSERT OR IGNORE` and
            // reports whether this call actually wrote the row.
            let inserted = if HISTORY_GUARDED_VERSIONS.contains(&migration.version) {
                // §C.4.1.1/§C.4.3 fail-closed: the scope migrations' SQL guards
                // see only a LIVE non-main `reference` row, and removing a
                // linked worktree deletes that row — so old linked layer/sparse
                // state would be adopted into main. The registry is the durable
                // witness and it is a FILE, so its verdict is read here and the
                // row half of the decision runs inside the claim transaction.
                //
                // A blocked repository has no in-binary escape in this card
                // (Codex R23): a bulk discard would destroy the `layer_path`
                // ownership that keeps retained overlay files unstageable, so
                // the escape must be an explicit per-row adopt/unapply with
                // provenance and audit, and that is deferred.
                let history = registry_linked_history(conn).await;
                apply_one_migration_guarded(conn, migration, history).await?
            } else {
                apply_one_migration(conn, migration).await?
            };
            if inserted {
                applied.push(migration.version);
            }
        }

        Ok(applied)
    }

    /// Apply pending migrations up to and including `target`, leaving anything
    /// newer unapplied.
    ///
    /// For tests that need to stand in the schema window a migration was
    /// written for: applying everything and rolling back lands on the *down*
    /// migration's reconstruction of the old shape, which is not the same
    /// artifact a real repository of that vintage has.
    pub async fn run_pending_up_to(
        &self,
        conn: &DatabaseConnection,
        target: i64,
    ) -> Result<Vec<i64>, MigrationError> {
        ensure_schema_versions_table(conn).await?;
        let current = self.current_version(conn).await?;
        let mut applied = Vec::new();
        for migration in &self.migrations {
            if migration.version > target {
                break;
            }
            if let Some(current) = current
                && migration.version <= current
            {
                continue;
            }
            if apply_one_migration(conn, migration).await? {
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
            // A guard SQL cannot express: rolling registry v3 away while the
            // registry carries live generations re-opens the old-writer hole
            // the marker exists to close. Checked in PHASE 1, so a refusal
            // happens before any `down` DDL runs.
            if migration.version == REGISTRY_V3_CAPABILITY_VERSION {
                registry_v3_rollback_preflight(conn).await?;
            }
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
    conn.execute_raw(Statement::from_string(backend, SCHEMA_VERSIONS_DDL))
        .await?;
    Ok(())
}

async fn schema_versions_table_exists(conn: &DatabaseConnection) -> Result<bool, MigrationError> {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
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
        .query_one_raw(Statement::from_string(
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
    apply_one_migration_guarded(conn, migration, RegistryLinkedHistory::Never).await
}

/// Versions whose guard needs a witness SQL cannot see: the worktree registry.
const HISTORY_GUARDED_VERSIONS: [i64; 3] = [2026072303, 2026072304, 2026072902];

/// What the REGISTRY can say about linked-worktree history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegistryLinkedHistory {
    /// Proven never: no linked worktree has ever existed here.
    Never,
    /// One has existed, or its history cannot be established. Both take the
    /// same fail-closed branch — only a PROVEN `Never` is safe — and are
    /// distinguished only in the message.
    Existed,
    Unreadable,
}

/// Read the registry's linked-worktree history through the VALIDATED parser.
///
/// Never fails: whether an unreadable registry MATTERS depends on legacy state
/// that is only known inside the migration transaction, and erroring here would
/// refuse an upgrade for a repository with nothing to mis-adopt. Only `NotFound`
/// is `Never`; a v1 shape, a v2 without recorded history, an unreadable file and
/// a parse failure are all treated as history that cannot be ruled out, because
/// the parser is the one place that knows a pre-v3 registry's history is
/// unknowable.
async fn registry_linked_history(conn: &DatabaseConnection) -> RegistryLinkedHistory {
    let Some(storage) = sqlite_database_dir(conn).await else {
        return RegistryLinkedHistory::Never;
    };
    let raw = match std::fs::read_to_string(storage.join("worktrees.json")) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RegistryLinkedHistory::Never;
        }
        Err(_) => return RegistryLinkedHistory::Unreadable,
    };
    match crate::command::worktree::WorktreeState::parse(raw.as_bytes()) {
        Ok(state) if state.ever_had_linked_worktree() => RegistryLinkedHistory::Existed,
        Ok(_) => RegistryLinkedHistory::Never,
        Err(_) => RegistryLinkedHistory::Unreadable,
    }
}

async fn apply_one_migration_guarded(
    conn: &DatabaseConnection,
    migration: &Migration,
    registry_history: RegistryLinkedHistory,
) -> Result<bool, MigrationError> {
    let now: DateTime<Utc> = Utc::now();
    let inserted = conn
        .transaction::<_, _, DbErr>(|txn| {
            let version = migration.version;
            let name = migration.name;
            let up = migration.up;
            let applied_at = now.to_rfc3339();
            let history = registry_history;
            Box::pin(async move {
                let backend = txn.get_database_backend();
                // Claim the version row FIRST (W1-hardening review P1#1):
                // `INSERT OR IGNORE` plus `changes()` decides the race
                // winner inside the same transaction, and this first write
                // also takes SQLite's write lock, serializing concurrent
                // upgraders. The loser sees `changes() = 0` and skips the
                // DDL entirely — RENAME-based rebuilds (2026072101,
                // 2026072301) are NOT idempotent, so re-running their up
                // DDL against an already-rebuilt table must never happen.
                // If the DDL below fails, the whole transaction (including
                // this claim) rolls back, so a failed apply never leaves a
                // phantom version row.
                txn.execute_raw(Statement::from_sql_and_values(
                    backend,
                    "INSERT OR IGNORE INTO schema_versions (version, name, applied_at) VALUES (?, ?, ?)",
                    [version.into(), name.into(), applied_at.into()],
                ))
                .await?;
                let row = txn
                    .query_one_raw(Statement::from_string(backend, "SELECT changes()"))
                    .await?
                    .ok_or_else(|| {
                        DbErr::Custom("SELECT changes() returned no row".to_string())
                    })?;
                let changed: i64 = row
                    .try_get_by_index(0)
                    .map_err(|err| DbErr::Custom(format!("changes() decode failed: {err}")))?;
                if changed == 0 {
                    // Another process already applied this version; nothing
                    // to do and nothing to record.
                    return Ok::<bool, DbErr>(false);
                }
                // §C.4.3, inside the claim with the write lock held: no
                // concurrent writer can add legacy state between this read and
                // the DDL.
                // 2026072902 does not BLOCK: operations are history, and the
                // right answer for one whose worktree cannot be established is
                // the `unknown` provenance the migration already defines — `op
                // restore` refuses those. Its marking runs AFTER the up DDL,
                // which is what creates the column; see below.
                if version != 2026072902
                    && history != RegistryLinkedHistory::Never
                    && historic_state_would_be_misattributed(txn, version).await?
                {
                    let why = if history == RegistryLinkedHistory::Unreadable {
                        "its worktree registry cannot be read or parsed, so whether a linked \
                         worktree existed is unknown"
                    } else {
                        "its worktree registry shows a linked worktree has existed"
                    };
                    return Err(DbErr::Custom(format!(
                        "cannot apply migration {version}: this repository holds \
                         repository-global state that the migration would attribute to the MAIN \
                         worktree, and {why} — so that state may belong to a worktree removed \
                         earlier. Adopting it would let its overlay files be staged, or apply a \
                         foreign sparse filter. Resolving this needs an explicit per-row \
                         adopt/unapply pass, which is not yet available; until then, restore a \
                         backup taken before the upgrade or remove the legacy state with the \
                         previous binary"
                    )));
                }
                apply_migration_compatibility(txn, version, name).await?;
                if version == OPERATION_V2_MIGRATION_VERSION {
                    apply_operation_v2_migration(txn, up).await?;
                } else {
                    txn.execute_raw(Statement::from_string(backend, up)).await?;
                }
                if version == 2026072902 && history != RegistryLinkedHistory::Never {
                    // The registry witness the migration's SQL cannot see
                    // (§C.9): a linked worktree removed before this upgrade
                    // leaves no HEAD row, so its old operations would be
                    // backfilled as trusted `main`. Mark every unscoped row
                    // `unknown` instead — inside this transaction, so the
                    // version row and the marking commit together.
                    txn.execute_raw(Statement::from_string(
                        backend,
                        "UPDATE `operation` SET `scope_provenance` = 'unknown' \
                         WHERE (`worktree_id` IS NULL OR `worktree_id` = '')"
                            .to_string(),
                    ))
                    .await?;
                }
                Ok::<bool, DbErr>(true)
            })
        })
        .await
        .map_err(|err| match err {
            sea_orm::TransactionError::Connection(db) => MigrationError::Database(db),
            sea_orm::TransactionError::Transaction(db) => MigrationError::Database(db),
        })?;
    Ok(inserted)
}

/// The operation-log migration is the one exception to the static-DDL path.
///
/// The active v1 operation wrapper must keep working during the OL-02..04
/// foundation window, so old rows are copied into an explicit legacy_*
/// namespace before the v2 tables are created. The copy, validation, source
/// removal, and version claim all share the runner transaction. A failure
/// therefore rolls back both data and schema, while a fresh v2 database simply
/// receives empty legacy tables for the still-active v1 reader/writer.
async fn apply_operation_v2_migration(
    txn: &sea_orm::DatabaseTransaction,
    canonical_sql: &str,
) -> Result<(), DbErr> {
    let operation_exists = sqlite_table_exists(txn, "operation").await?;
    let operation_columns = if operation_exists {
        sqlite_table_columns(txn, "operation").await?
    } else {
        BTreeSet::new()
    };
    let operation_is_v1 = operation_columns.contains("view_id");
    if operation_exists && !operation_is_v1 && !operation_columns.contains("format_version") {
        return Err(DbErr::Custom(
            "operation table has neither the v1 view_id nor the v2 format_version shape"
                .to_string(),
        ));
    }

    if operation_is_v1 {
        if sqlite_table_exists(txn, "legacy_operation").await? {
            return Err(DbErr::Custom(
                "legacy_operation already exists while the v1 operation table is present"
                    .to_string(),
            ));
        }
        for required in [
            "op_id",
            "repo_id",
            "view_id",
            "command_name",
            "description",
            "actor",
            "start_ts",
            "status",
        ] {
            if !operation_columns.contains(required) {
                return Err(DbErr::Custom(format!(
                    "v1 operation table is missing required column {required}"
                )));
            }
        }

        txn.execute_raw(Statement::from_string(
            txn.get_database_backend(),
            legacy_operation_staging_ddl(),
        ))
        .await?;
        let expression = |column: &str, fallback: &str| {
            if operation_columns.contains(column) {
                column.to_string()
            } else {
                fallback.to_string()
            }
        };
        let insert = format!(
            "INSERT INTO legacy_operation__staging (
             op_id, repo_id, view_id, command_name, description, actor, args_digest,
             start_ts, end_ts, status, worktree_id, scope_provenance, restorable,
             control_slot, claim_owner, scope_kind) SELECT {}, {}, {}, {}, {}, {}, {}, {}, {}, {},
             COALESCE({}, ''), COALESCE({}, 'unknown'), COALESCE({}, 1), {}, {},
             COALESCE({}, 'unknown') FROM operation",
            expression("op_id", "''"),
            expression("repo_id", "''"),
            expression("view_id", "''"),
            expression("command_name", "''"),
            expression("description", "''"),
            expression("actor", "''"),
            expression("args_digest", "NULL"),
            expression("start_ts", "0"),
            expression("end_ts", "NULL"),
            expression("status", "''"),
            expression("worktree_id", "NULL"),
            expression("scope_provenance", "NULL"),
            expression("restorable", "NULL"),
            expression("control_slot", "NULL"),
            expression("claim_owner", "NULL"),
            expression("scope_kind", "NULL"),
        );
        txn.execute_raw(Statement::from_string(txn.get_database_backend(), insert))
            .await?;
        validate_copy(
            txn,
            "operation",
            "legacy_operation__staging",
            &[
                "op_id",
                "repo_id",
                "view_id",
                "command_name",
                "description",
                "actor",
            ],
        )
        .await?;
        txn.execute_raw(Statement::from_string(
            txn.get_database_backend(),
            "DROP TABLE operation; ALTER TABLE legacy_operation__staging RENAME TO legacy_operation;",
        ))
        .await?;
    }

    let parent_exists = sqlite_table_exists(txn, "operation_parent").await?;
    if parent_exists {
        let parent_columns = sqlite_table_columns(txn, "operation_parent").await?;
        if !parent_columns.contains("ordinal") {
            copy_legacy_table(
                txn,
                "operation_parent",
                "legacy_operation_parent",
                "CREATE TABLE legacy_operation_parent__staging (op_id TEXT NOT NULL, parent_op_id TEXT NOT NULL, PRIMARY KEY (op_id, parent_op_id));",
                "INSERT INTO legacy_operation_parent__staging (op_id, parent_op_id) SELECT op_id, parent_op_id FROM operation_parent;",
                &["op_id", "parent_op_id"],
            )
            .await?;
        }
    }
    copy_legacy_table_if_present(
        txn,
        "operation_view",
        "legacy_operation_view",
        "CREATE TABLE legacy_operation_view__staging (view_id TEXT PRIMARY KEY, repo_id TEXT NOT NULL, head_kind TEXT NOT NULL, head_target TEXT NOT NULL, created_at INTEGER NOT NULL);",
        "INSERT INTO legacy_operation_view__staging (view_id, repo_id, head_kind, head_target, created_at) SELECT view_id, repo_id, head_kind, head_target, created_at FROM operation_view;",
        &["view_id", "repo_id", "head_kind", "head_target"],
    )
    .await?;
    copy_legacy_table_if_present(
        txn,
        "operation_view_ref",
        "legacy_operation_view_ref",
        "CREATE TABLE legacy_operation_view_ref__staging (view_id TEXT NOT NULL, ref_kind TEXT NOT NULL, ref_name TEXT NOT NULL, ref_remote TEXT NOT NULL, target_oid TEXT NOT NULL, PRIMARY KEY (view_id, ref_kind, ref_name, ref_remote));",
        "INSERT INTO legacy_operation_view_ref__staging (view_id, ref_kind, ref_name, ref_remote, target_oid) SELECT view_id, ref_kind, ref_name, ref_remote, target_oid FROM operation_view_ref;",
        &["view_id", "ref_kind", "ref_name", "target_oid"],
    )
    .await?;
    copy_legacy_table_if_present(
        txn,
        "operation_view_workspace",
        "legacy_operation_view_workspace",
        "CREATE TABLE legacy_operation_view_workspace__staging (view_id TEXT NOT NULL, pointer_kind TEXT NOT NULL, pointer_value TEXT NOT NULL, PRIMARY KEY (view_id, pointer_kind));",
        "INSERT INTO legacy_operation_view_workspace__staging (view_id, pointer_kind, pointer_value) SELECT view_id, pointer_kind, pointer_value FROM operation_view_workspace;",
        &["view_id", "pointer_kind", "pointer_value"],
    )
    .await?;

    txn.execute_raw(Statement::from_string(
        txn.get_database_backend(),
        legacy_operation_namespace_ddl(),
    ))
    .await?;
    txn.execute_raw(Statement::from_string(
        txn.get_database_backend(),
        canonical_sql.to_string(),
    ))
    .await?;
    Ok(())
}

const OPERATION_V2_MIGRATION_VERSION: i64 = 2026090101;

fn legacy_operation_staging_ddl() -> String {
    "CREATE TABLE legacy_operation__staging (
     op_id TEXT PRIMARY KEY, repo_id TEXT NOT NULL, view_id TEXT NOT NULL,
     command_name TEXT NOT NULL, description TEXT NOT NULL, actor TEXT NOT NULL,
     args_digest TEXT, start_ts INTEGER NOT NULL, end_ts INTEGER, status TEXT NOT NULL,
     worktree_id TEXT NOT NULL DEFAULT '', scope_provenance TEXT NOT NULL DEFAULT 'unknown',
     restorable INTEGER NOT NULL DEFAULT 1, control_slot TEXT, claim_owner TEXT,
     scope_kind TEXT NOT NULL DEFAULT 'unknown');"
        .to_string()
}

fn legacy_operation_namespace_ddl() -> String {
    "CREATE TABLE IF NOT EXISTS legacy_operation (
     op_id TEXT PRIMARY KEY, repo_id TEXT NOT NULL, view_id TEXT NOT NULL,
     command_name TEXT NOT NULL, description TEXT NOT NULL, actor TEXT NOT NULL,
     args_digest TEXT, start_ts INTEGER NOT NULL, end_ts INTEGER, status TEXT NOT NULL,
     worktree_id TEXT NOT NULL DEFAULT '', scope_provenance TEXT NOT NULL DEFAULT 'unknown',
     restorable INTEGER NOT NULL DEFAULT 1, control_slot TEXT, claim_owner TEXT,
     scope_kind TEXT NOT NULL DEFAULT 'unknown');
     CREATE INDEX IF NOT EXISTS idx_legacy_operation_repo_order
       ON legacy_operation(repo_id, end_ts DESC, start_ts DESC, op_id DESC);
     CREATE INDEX IF NOT EXISTS idx_legacy_operation_dedup_scope
       ON legacy_operation(repo_id, worktree_id, command_name, args_digest, status, end_ts);
     CREATE UNIQUE INDEX IF NOT EXISTS idx_legacy_operation_control_slot
       ON legacy_operation(repo_id, worktree_id)
       WHERE status = 'running' AND control_slot IS NOT NULL;
     CREATE TRIGGER IF NOT EXISTS legacy_operation_scope_provenance_domain_insert
       BEFORE INSERT ON legacy_operation
       FOR EACH ROW WHEN NEW.scope_provenance NOT IN ('declared', 'unknown')
       BEGIN
         SELECT RAISE(ABORT, 'legacy_operation.scope_provenance must be either declared or unknown');
       END;
     CREATE TRIGGER IF NOT EXISTS legacy_operation_scope_provenance_domain_update
       BEFORE UPDATE OF scope_provenance ON legacy_operation
       FOR EACH ROW WHEN NEW.scope_provenance NOT IN ('declared', 'unknown')
       BEGIN
         SELECT RAISE(ABORT, 'legacy_operation.scope_provenance must be either declared or unknown');
       END;
     CREATE TRIGGER IF NOT EXISTS legacy_operation_scope_kind_domain_insert
       BEFORE INSERT ON legacy_operation
       FOR EACH ROW WHEN NEW.scope_kind NOT IN ('main', 'linked', 'repository', 'unknown')
       BEGIN
         SELECT RAISE(ABORT, 'legacy_operation.scope_kind must be main, linked, repository or unknown');
       END;
     CREATE TRIGGER IF NOT EXISTS legacy_operation_scope_kind_domain_update
       BEFORE UPDATE OF scope_kind ON legacy_operation
       FOR EACH ROW WHEN NEW.scope_kind NOT IN ('main', 'linked', 'repository', 'unknown')
       BEGIN
         SELECT RAISE(ABORT, 'legacy_operation.scope_kind must be main, linked, repository or unknown');
       END;
     CREATE TABLE IF NOT EXISTS legacy_operation_parent (
       op_id TEXT NOT NULL, parent_op_id TEXT NOT NULL,
       PRIMARY KEY (op_id, parent_op_id));
     CREATE INDEX IF NOT EXISTS idx_legacy_operation_parent_parent
       ON legacy_operation_parent(parent_op_id, op_id);
     CREATE TABLE IF NOT EXISTS legacy_operation_view (
       view_id TEXT PRIMARY KEY, repo_id TEXT NOT NULL, head_kind TEXT NOT NULL,
       head_target TEXT NOT NULL, created_at INTEGER NOT NULL);
     CREATE INDEX IF NOT EXISTS idx_legacy_operation_view_repo_created
       ON legacy_operation_view(repo_id, created_at DESC);
     CREATE TABLE IF NOT EXISTS legacy_operation_view_ref (
       view_id TEXT NOT NULL, ref_kind TEXT NOT NULL, ref_name TEXT NOT NULL,
       ref_remote TEXT NOT NULL, target_oid TEXT NOT NULL,
       PRIMARY KEY (view_id, ref_kind, ref_name, ref_remote));
     CREATE TABLE IF NOT EXISTS legacy_operation_view_workspace (
       view_id TEXT NOT NULL, pointer_kind TEXT NOT NULL, pointer_value TEXT NOT NULL,
       PRIMARY KEY (view_id, pointer_kind));"
        .to_string()
}

async fn copy_legacy_table_if_present(
    txn: &sea_orm::DatabaseTransaction,
    source: &str,
    target: &str,
    staging_ddl: &str,
    insert_sql: &str,
    keys: &[&str],
) -> Result<(), DbErr> {
    if sqlite_table_exists(txn, source).await? {
        copy_legacy_table(txn, source, target, staging_ddl, insert_sql, keys).await?;
    }
    Ok(())
}

async fn copy_legacy_table(
    txn: &sea_orm::DatabaseTransaction,
    source: &str,
    target: &str,
    staging_ddl: &str,
    insert_sql: &str,
    keys: &[&str],
) -> Result<(), DbErr> {
    if sqlite_table_exists(txn, target).await? {
        return Err(DbErr::Custom(format!(
            "{target} already exists while {source} still needs migration"
        )));
    }
    let staging = format!("{target}__staging");
    txn.execute_raw(Statement::from_string(
        txn.get_database_backend(),
        format!("DROP TABLE IF EXISTS {staging}; {staging_ddl}"),
    ))
    .await?;
    txn.execute_raw(Statement::from_string(
        txn.get_database_backend(),
        insert_sql.to_string(),
    ))
    .await?;
    validate_copy(txn, source, &staging, keys).await?;
    txn.execute_raw(Statement::from_string(
        txn.get_database_backend(),
        format!("DROP TABLE {source}; ALTER TABLE {staging} RENAME TO {target};"),
    ))
    .await?;
    Ok(())
}

async fn validate_copy(
    txn: &sea_orm::DatabaseTransaction,
    source: &str,
    staging: &str,
    keys: &[&str],
) -> Result<(), DbErr> {
    let source_count = count_rows(txn, source, None).await?;
    let staging_count = count_rows(txn, staging, None).await?;
    if source_count != staging_count {
        return Err(DbErr::Custom(format!(
            "copy-first migration row-count mismatch for {source}: source={source_count}, staging={staging_count}"
        )));
    }
    let invalid_predicate = keys
        .iter()
        .map(|key| format!("COALESCE(TRIM({key}), '') = ''"))
        .collect::<Vec<_>>()
        .join(" OR ");
    if count_rows(txn, staging, Some(&invalid_predicate)).await? != 0 {
        return Err(DbErr::Custom(format!(
            "copy-first migration found an empty key in {source}"
        )));
    }
    let key_predicate = keys
        .iter()
        .map(|key| format!("s.{key} = t.{key}"))
        .collect::<Vec<_>>()
        .join(" AND ");
    let source_missing = count_rows(
        txn,
        &format!(
            "{source} AS s WHERE NOT EXISTS (SELECT 1 FROM {staging} AS t WHERE {key_predicate})"
        ),
        None,
    )
    .await?;
    let staging_missing = count_rows(
        txn,
        &format!(
            "{staging} AS t WHERE NOT EXISTS (SELECT 1 FROM {source} AS s WHERE {key_predicate})"
        ),
        None,
    )
    .await?;
    if source_missing != 0 || staging_missing != 0 {
        return Err(DbErr::Custom(format!(
            "copy-first migration key-set mismatch for {source}: source_missing={source_missing}, staging_missing={staging_missing}"
        )));
    }
    Ok(())
}

async fn sqlite_table_exists<C: ConnectionTrait>(conn: &C, name: &str) -> Result<bool, DbErr> {
    Ok(conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ? LIMIT 1",
            [name.into()],
        ))
        .await?
        .is_some())
}

async fn sqlite_table_columns<C: ConnectionTrait>(
    conn: &C,
    table: &str,
) -> Result<BTreeSet<String>, DbErr> {
    let rows = conn
        .query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            format!("PRAGMA table_info('{table}')"),
        ))
        .await?;
    let mut columns = BTreeSet::new();
    for row in rows {
        let name: String = row
            .try_get_by_index(1)
            .map_err(|error| DbErr::Custom(format!("cannot decode {table} column: {error}")))?;
        columns.insert(name);
    }
    Ok(columns)
}

/// Install additive columns that SQLite cannot express idempotently in a SQL
/// migration (`ADD COLUMN IF NOT EXISTS` is unsupported).  Early M5
/// development repositories applied the 1406 base schema, while later ones
/// applied an evolved 1406 containing some or all of these columns.  Probe each
/// column inside the 1407 transaction so both shapes converge without a
/// duplicate-column failure and without publishing the version row early.
async fn apply_migration_compatibility<C: ConnectionTrait>(
    conn: &C,
    version: i64,
    name: &str,
) -> Result<(), DbErr> {
    if version != 2026071407 || name != "agent_subagent_replication" {
        return Ok(());
    }
    add_column_if_missing(
        conn,
        "agent_session",
        "sync_revision",
        "ALTER TABLE `agent_session` ADD COLUMN `sync_revision` INTEGER NOT NULL DEFAULT 1",
        None,
    )
    .await?;
    add_column_if_missing(
        conn,
        "agent_checkpoint",
        "sync_revision",
        "ALTER TABLE `agent_checkpoint` ADD COLUMN `sync_revision` INTEGER NOT NULL DEFAULT 1",
        None,
    )
    .await?;
    add_column_if_missing(
        conn,
        "agent_subagent_content_claim",
        "revision_cursor",
        "ALTER TABLE `agent_subagent_content_claim` ADD COLUMN `revision_cursor` INTEGER NOT NULL DEFAULT 0",
        None,
    )
    .await?;
    // Some development builds had already installed the column with its
    // default-zero value before 1407 existed. Backfill independently from
    // column creation so every evolved 1406 shape preserves the allocated
    // revision high-water mark.
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "UPDATE `agent_subagent_content_claim`
         SET `revision_cursor` = `current_revision`
         WHERE `revision_cursor` < `current_revision`",
    ))
    .await?;
    add_column_if_missing(
        conn,
        "agent_subagent_content_claim",
        "sync_revision",
        "ALTER TABLE `agent_subagent_content_claim` ADD COLUMN `sync_revision` INTEGER NOT NULL DEFAULT 1",
        None,
    )
    .await?;
    add_column_if_missing(
        conn,
        "agent_subagent_link",
        "sync_revision",
        "ALTER TABLE `agent_subagent_link` ADD COLUMN `sync_revision` INTEGER NOT NULL DEFAULT 1",
        None,
    )
    .await?;
    Ok(())
}

async fn add_column_if_missing<C: ConnectionTrait>(
    conn: &C,
    table: &str,
    column: &str,
    alter_sql: &'static str,
    initialize_sql: Option<&'static str>,
) -> Result<(), DbErr> {
    let backend = conn.get_database_backend();
    let probe = format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = ? LIMIT 1");
    if conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            probe,
            [column.into()],
        ))
        .await?
        .is_some()
    {
        return Ok(());
    }
    conn.execute_raw(Statement::from_string(backend, alter_sql))
        .await?;
    if let Some(initialize_sql) = initialize_sql {
        conn.execute_raw(Statement::from_string(backend, initialize_sql))
            .await?;
    }
    Ok(())
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
                txn.execute_raw(Statement::from_sql_and_values(
                    backend,
                    "DELETE FROM schema_versions WHERE version = ?",
                    [version.into()],
                ))
                .await?;
                let row = txn
                    .query_one_raw(Statement::from_string(backend, "SELECT changes()"))
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
                txn.execute_raw(Statement::from_string(backend, down))
                    .await?;
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
        // plan-20260713 DR-05c (M4): import-job state / crash-recovery identity
        // (ADR-DR-06). Import-only; the coverage claim gate remains the
        // cross-path exactly-once authority. Stable key excludes content
        // digest so a re-run resumes the same job.
        sql_migration(
            2026071402,
            "agent_import_identity",
            include_str!("../../../sql/migrations/2026071402_agent_import_identity.sql"),
            include_str!("../../../sql/migrations/2026071402_agent_import_identity_down.sql"),
        ),
        // plan-20260713 ADR-DR-06/DR-19 (M4): local anti-resurrection tombstone
        // — written in-transaction before an erase deletes `agent_session`
        // (concurrent write barrier). Carries the import-block key plus a
        // FK-free `erased_session_id` for DR-07's read-only `erased` display.
        sql_migration(
            2026071403,
            "agent_import_tombstone",
            include_str!("../../../sql/migrations/2026071403_agent_import_tombstone.sql"),
            include_str!("../../../sql/migrations/2026071403_agent_import_tombstone_down.sql"),
        ),
        // M4 compatibility hardening: 2026071403 was already released before
        // the old-writer anti-resurrection triggers were added. Keep its up
        // migration immutable and install the triggers monotonically so
        // repositories already at 1403 receive the barrier on upgrade.
        sql_migration(
            2026071404,
            "agent_tombstone_compat_barrier",
            include_str!("../../../sql/migrations/2026071404_agent_tombstone_compat_barrier.sql"),
            include_str!(
                "../../../sql/migrations/2026071404_agent_tombstone_compat_barrier_down.sql"
            ),
        ),
        // M4 conflict recovery: preserve the first complete challenger in a
        // bounded side table when coverage arbitration parks a claim.
        sql_migration(
            2026071405,
            "agent_coverage_conflict",
            include_str!("../../../sql/migrations/2026071405_agent_coverage_conflict.sql"),
            include_str!("../../../sql/migrations/2026071405_agent_coverage_conflict_down.sql"),
        ),
        // M5 / DR-06: provider-root-relative subagent content identity,
        // append-only source revisions, and boundary/content association.
        sql_migration(
            2026071406,
            "agent_subagent_content",
            include_str!("../../../sql/migrations/2026071406_agent_subagent_content.sql"),
            include_str!("../../../sql/migrations/2026071406_agent_subagent_content_down.sql"),
        ),
        // M5 compatibility hardening: 2026071406 was exercised by live
        // development repositories before replication generations, revision
        // cursors, and prune fences were added.  Keep the base migration
        // immutable and install those additions monotonically.
        sql_migration(
            2026071407,
            "agent_subagent_replication",
            include_str!("../../../sql/migrations/2026071407_agent_subagent_replication.sql"),
            include_str!("../../../sql/migrations/2026071407_agent_subagent_replication_down.sql"),
        ),
        // plan-20260714 Part C W1 (§C.4.2): re-key `sequence_state` from the
        // repository-global `CHECK(id = 1)` single row to one row per worktree
        // (`worktree_id`, main = ""), so a cherry-pick/am/revert sequence in one
        // worktree can no longer overwrite another's. `rebase_state` is
        // deliberately excluded — its shape is owned by lazy DDL in
        // `command/rebase.rs`, so a static rebuild could drop columns.
        sql_migration(
            2026071901,
            "sequencer_worktree_scope",
            include_str!("../../../sql/migrations/2026071901_sequencer_worktree_scope.sql"),
            include_str!("../../../sql/migrations/2026071901_sequencer_worktree_scope_down.sql"),
        ),
        // plan-20260714 Part C W1 (§C.4.2): re-key `rebase_state` to one row
        // per worktree too, retiring its lazy DDL. The variable historical
        // column set is normalized by `normalize_rebase_state_shape` (run
        // before the runner on every connection open), so this static rebuild
        // can assume the full pre-scope shape. Down fails closed on linked
        // rows.
        sql_migration(
            2026072101,
            "rebase_state_worktree_scope",
            include_str!("../../../sql/migrations/2026072101_rebase_state_worktree_scope.sql"),
            include_str!("../../../sql/migrations/2026072101_rebase_state_worktree_scope_down.sql"),
        ),
        // plan-20260714 Part C W1 (§C.9): record the worktree an operation ran
        // in, so the duplicate-submission window is scoped per-worktree (the
        // same command run concurrently in two worktrees is two legitimate
        // operations, not a duplicate).
        sql_migration(
            2026072201,
            "operation_worktree_scope",
            include_str!("../../../sql/migrations/2026072201_operation_worktree_scope.sql"),
            include_str!("../../../sql/migrations/2026072201_operation_worktree_scope_down.sql"),
        ),
        // plan-20260714 Part C W1 (§C.4.2): re-key `bisect_state` to one row
        // per worktree, retiring the last sequencer-family lazy DDL. The
        // variable historical column set is normalized by
        // `normalize_bisect_state_shape` (run before the runner on every
        // connection open). Linked-scope rows may already exist in the wild
        // (the lazy `worktree_id` shipped in v0.19.34), so the rebuild keeps
        // the newest row per scope. Down fails closed on linked rows.
        sql_migration(
            2026072301,
            "bisect_state_worktree_scope",
            include_str!("../../../sql/migrations/2026072301_bisect_state_worktree_scope.sql"),
            include_str!("../../../sql/migrations/2026072301_bisect_state_worktree_scope_down.sql"),
        ),
        // plan-20260714 Part C W1 (§C.4.1.1): scope the dirty-set advisory
        // cache per worktree — `working_dirty` keyed by (worktree_id, path,
        // kind), `working_dirty_meta` re-keyed to worktree_id. Legacy rows
        // are cleared (rebuildable advisory state; each scope rescans). Down
        // fails closed on linked rows.
        sql_migration(
            2026072302,
            "working_dirty_worktree_scope",
            include_str!("../../../sql/migrations/2026072302_working_dirty_worktree_scope.sql"),
            include_str!(
                "../../../sql/migrations/2026072302_working_dirty_worktree_scope_down.sql"
            ),
        ),
        // plan-20260714 Part C W1 (§C.4.1.1): scope the layer overlay
        // registry per worktree — `layer` keyed by (worktree_id, name),
        // `layer_path` by (worktree_id, path). Layer ownership is NOT
        // rebuildable (clearing it would make overlay files committable), so
        // legacy rows adopt to main ONLY when no linked worktree exists; the
        // up migration fails closed (CHECK guard) on legacy rows + linked
        // HEAD evidence, and down fails closed on linked rows.
        sql_migration(
            2026072303,
            "layer_worktree_scope",
            include_str!("../../../sql/migrations/2026072303_layer_worktree_scope.sql"),
            include_str!("../../../sql/migrations/2026072303_layer_worktree_scope_down.sql"),
        ),
        // plan-20260714 Part C W1 (§C.4.1.1): scope the read-only sparse
        // view per worktree — `sparse_view` keyed by (worktree_id, ordinal)
        // and the `sparse.enabled` toggle re-projected from the scope-less
        // `config_kv` key into the per-worktree `sparse_view_meta` table.
        // Legacy state adopts to main only when no linked worktree exists;
        // the up migration fails closed (CHECK guard) on legacy state +
        // linked HEAD evidence, and down fails closed on linked rows.
        sql_migration(
            2026072304,
            "sparse_view_worktree_scope",
            include_str!("../../../sql/migrations/2026072304_sparse_view_worktree_scope.sql"),
            include_str!("../../../sql/migrations/2026072304_sparse_view_worktree_scope_down.sql"),
        ),
        // plan-20260714 Part C W3 (§C.7): worktree registry v2 capability
        // marker — its presence makes older binaries refuse the repository
        // (future-schema fail-closed) before they can parse or recreate the
        // registry file. Down drops only the marker: the v2 JSON layout
        // itself still fails a v1 parser closed (renamed top-level key), so
        // no silent misread window opens; s1b's lifecycle tables will add
        // CHECK-guarded refusal while v2-only state exists.
        sql_migration(
            2026072401,
            "worktree_registry_v2",
            include_str!("../../../sql/migrations/2026072401_worktree_registry_v2.sql"),
            include_str!("../../../sql/migrations/2026072401_worktree_registry_v2_down.sql"),
        ),
        // plan-20260714 Part C W3-s1b (§C.7): worktree lifecycle
        // (detached_from_registry / tombstone) mirror + durable intent
        // journal for registry mutators. Down REFUSES (CHECK guard) while
        // any lifecycle or journal row exists — v2 lifecycle must never be
        // folded into a v1 active entry or silently dropped.
        sql_migration(
            2026072402,
            "worktree_lifecycle_journal",
            include_str!("../../../sql/migrations/2026072402_worktree_lifecycle_journal.sql"),
            include_str!("../../../sql/migrations/2026072402_worktree_lifecycle_journal_down.sql"),
        ),
        // plan-20260714 Part C W3-s3 (§C.6): admit the 'migrate' op (plus a
        // stage column for its state machine) into the intent journal via a
        // RENAME rebuild. Down refuses while any migrate intent exists.
        sql_migration(
            2026072403,
            "worktree_migrate_intent",
            include_str!("../../../sql/migrations/2026072403_worktree_migrate_intent.sql"),
            include_str!("../../../sql/migrations/2026072403_worktree_migrate_intent_down.sql"),
        ),
        // plan-20260714 Part C W4-s1 (§C.8): the unified workspace
        // association + lease record. Down refuses while any non-terminal
        // workspace (live lease / orphan awaiting the scavenger) exists.
        sql_migration(
            2026072501,
            "workspace_record",
            include_str!("../../../sql/migrations/2026072501_workspace_record.sql"),
            include_str!("../../../sql/migrations/2026072501_workspace_record_down.sql"),
        ),
        // plan-20260714 Part C W4-s2 hardening: keyset pagination for
        // `agent workspace list` must search by repository prefix in
        // workspace_id order instead of scanning/sorting as the registry grows.
        sql_migration(
            2026072502,
            "workspace_paging_index",
            include_str!("../../../sql/migrations/2026072502_workspace_paging_index.sql"),
            include_str!("../../../sql/migrations/2026072502_workspace_paging_index_down.sql"),
        ),
        // plan-20260714 Part C W0 (§C.11 "HEAD schema hardening"): at most one
        // local HEAD row per worktree scope, enforced by partial unique
        // indexes. Pre-existing duplicates FAIL the migration closed
        // (ADR-0714-08) — an ambiguous scope is never resolved by guessing.
        sql_migration(
            2026072901,
            "head_scope_unique",
            include_str!("../../../sql/migrations/2026072901_head_scope_unique.sql"),
            include_str!("../../../sql/migrations/2026072901_head_scope_unique_down.sql"),
        ),
        // plan-20260714 Part C W0 (§C.11 "operation scope schema/migration"):
        // distinguish a DECLARED operation scope from one inherited by the
        // 2026072201 backfill. `op restore` refuses `unknown` rows rather
        // than rewrite a worktree from an operation that may never have run
        // in it (ADR-0714-08).
        sql_migration(
            2026072902,
            "operation_scope_provenance",
            include_str!("../../../sql/migrations/2026072902_operation_scope_provenance.sql"),
            include_str!("../../../sql/migrations/2026072902_operation_scope_provenance_down.sql"),
        ),
        // plan-20260714 Part C W1 (§C.9): canonicalize `operation.args_digest`
        // so the duplicate window's SQL equality matches rows written before
        // the writer started normalizing.
        sql_migration(
            2026073001,
            "operation_args_digest_canonical",
            include_str!("../../../sql/migrations/2026073001_operation_args_digest_canonical.sql"),
            include_str!(
                "../../../sql/migrations/2026073001_operation_args_digest_canonical_down.sql"
            ),
        ),
        // plan-20260714 Part C W1 (§C.9): the composite index the scoped
        // duplicate point query needs — without it the check scans every
        // operation this repository recorded inside the window.
        sql_migration(
            2026073002,
            "operation_dedup_index",
            include_str!("../../../sql/migrations/2026073002_operation_dedup_index.sql"),
            include_str!("../../../sql/migrations/2026073002_operation_dedup_index_down.sql"),
        ),
        // plan-20260714 Part C W1 (§C.9): boundary recording — a cross-process
        // atomic `running` claim, and an explicit `restorable` property so a
        // sequencer control's HEAD/refs-only snapshot can never be replayed
        // over sequencer state it cannot restore.
        sql_migration(
            2026073003,
            "operation_boundary_claim",
            include_str!("../../../sql/migrations/2026073003_operation_boundary_claim.sql"),
            include_str!("../../../sql/migrations/2026073003_operation_boundary_claim_down.sql"),
        ),
        // plan-20260714 Part C W1 (§C.9): the typed scope KIND, so a
        // repository-scope operation is distinguishable from a main-scope one
        // and `op restore` can refuse it by kind.
        sql_migration(
            2026073004,
            "operation_scope_kind",
            include_str!("../../../sql/migrations/2026073004_operation_scope_kind.sql"),
            include_str!("../../../sql/migrations/2026073004_operation_scope_kind_down.sql"),
        ),
        // plan-20260714 Part C W1 (§C.4.1.1): registry v3 — the durable
        // registration-generation counter the service fence depends on. The
        // marker keeps a v2-era binary from rewriting the registry without it.
        sql_migration(
            2026073005,
            "worktree_registry_v3_capability",
            include_str!("../../../sql/migrations/2026073005_worktree_registry_v3_capability.sql"),
            include_str!(
                "../../../sql/migrations/2026073005_worktree_registry_v3_capability_down.sql"
            ),
        ),
        // plan-20260714 Part C W2 (§C.10): version fence for the stash
        // reflog's generation column — an older binary writes reusable
        // (ABA-prone) lines, so it must refuse the repository at connect time.
        sql_migration(
            2026073101,
            "stash_generation_fence",
            include_str!("../../../sql/migrations/2026073101_stash_generation_fence.sql"),
            include_str!("../../../sql/migrations/2026073101_stash_generation_fence_down.sql"),
        ),
        // plan-20260714 Part C W4: bind new external-agent capture, export,
        // and import state to canonical repo/worktree/workspace ownership.
        // Legacy rows stay explicitly unknown and require doctor adoption;
        // the migration must never guess that they belonged to main.
        sql_migration(
            2026080401,
            "agent_capture_workspace_scope",
            include_str!("../../../sql/migrations/2026080401_agent_capture_workspace_scope.sql"),
            include_str!(
                "../../../sql/migrations/2026080401_agent_capture_workspace_scope_down.sql"
            ),
        ),
        // plan-20260715 W2-12: shared runtime usage dimensions and replay-safe
        // event identity. Legacy rows remain queryable with NULL dimensions.
        sql_migration(
            2026080402,
            "agent_usage_runtime_attribution",
            include_str!("../../../sql/migrations/2026080402_agent_usage_runtime_attribution.sql"),
            include_str!(
                "../../../sql/migrations/2026080402_agent_usage_runtime_attribution_down.sql"
            ),
        ),
        // W2-12 follow-up: event replay keys are unique per durable session,
        // so independent headless sessions can reuse a browser command ID.
        sql_migration(
            2026080403,
            "agent_usage_event_session_scope",
            include_str!("../../../sql/migrations/2026080403_agent_usage_event_session_scope.sql"),
            include_str!(
                "../../../sql/migrations/2026080403_agent_usage_event_session_scope_down.sql"
            ),
        ),
        // plan-20260715 W4-07: Always-approval provenance + Repository ownership
        // hardening. Empty provenance backfill; project_id not rewritten.
        sql_migration(
            2026081301,
            "approved_permission_provenance",
            include_str!("../../../sql/migrations/2026081301_approved_permission_provenance.sql"),
            include_str!(
                "../../../sql/migrations/2026081301_approved_permission_provenance_down.sql"
            ),
        ),
        // plan-20260818 LB-02: DeepSeek Harness bridge durable projection
        // (agent_bridge_session/event/operation/checkpoint/link). Forward-only:
        // the down path freezes while any bridge row exists and never deletes
        // acked events/evidence. See docs/development/tracing/deepseek-harness.md
        // and the plan's ADR-LB-02/03.
        sql_migration(
            2026081801,
            "agent_bridge_capture",
            include_str!("../../../sql/migrations/2026081801_agent_bridge_capture.sql"),
            include_str!("../../../sql/migrations/2026081801_agent_bridge_capture_down.sql"),
        ),
        // plan-20260818 LB-04/LB-05: `agent_bridge_link` becomes a real
        // relation graph — edge-level uniqueness so one result can carry its
        // operation, workspace, parent-session and evidence associations, plus
        // the mutation source kinds (`commit`/`restore`/`review`). Down freezes
        // while any link row exists (ER-LB-04: never delete acked provenance).
        sql_migration(
            2026082401,
            "agent_bridge_link_relations",
            include_str!("../../../sql/migrations/2026082401_agent_bridge_link_relations.sql"),
            include_str!("../../../sql/migrations/2026082401_agent_bridge_link_relations_down.sql"),
        ),
        // OL-02..04 foundation: v2 schema, canonical manifests, and the
        // durable store are landed together. The migration is forward-only;
        // legacy rows remain under legacy_* until the later runtime cutover.
        Migration {
            version: OPERATION_V2_MIGRATION_VERSION,
            name: "operation_v2",
            up: include_str!("../../../sql/migrations/2026090101_operation_v2.sql"),
            down: None,
        },
    ]
}

/// Whether this repository holds state the migration at `version` would
/// attribute to the MAIN worktree while its real owner cannot be established.
///
/// Defined exactly as each migration defines it — including sparse's last-wins
/// `config_kv.sparse.enabled`, TRIMMED and compared case-SENSITIVELY, which is
/// what migration 2026072304's own predicate does. The probe conforms to the
/// migration rather than the reverse: migrations are immutable once applied, so
/// a probe that case-folded would refuse an upgrade over a `TRUE` the migration
/// itself projects as disabled.
///
/// Read inside the migration's transaction. Every failure propagates: a table
/// CONFIRMED absent contributes nothing, but a locked database or an undecodable
/// value is an unknown answer, and an unknown answer must not read as "nothing
/// at stake".
async fn historic_state_would_be_misattributed(
    txn: &sea_orm::DatabaseTransaction,
    version: i64,
) -> Result<bool, DbErr> {
    let tables: &[&str] = match version {
        2026072303 => &["layer", "layer_path"],
        2026072304 => &["sparse_view"],
        // 2026072902: an operation row with no recorded scope is the state that
        // would be backfilled as trusted `main`.
        _ => &[],
    };
    for table in tables {
        if count_rows(txn, table, None).await? > 0 {
            return Ok(true);
        }
    }
    if version == 2026072902
        && count_rows(
            txn,
            "operation",
            Some("`worktree_id` IS NULL OR `worktree_id` = ''"),
        )
        .await?
            > 0
    {
        return Ok(true);
    }
    if version == 2026072304 {
        match txn
            .query_one_raw(Statement::from_string(
                txn.get_database_backend(),
                "SELECT TRIM(COALESCE((SELECT `value` FROM `config_kv` \
                 WHERE `key` = 'sparse.enabled' ORDER BY `id` DESC LIMIT 1), ''))"
                    .to_string(),
            ))
            .await
        {
            Ok(Some(row)) => {
                let value: String = row.try_get_by_index(0).map_err(|error| {
                    DbErr::Custom(format!("`sparse.enabled` is undecodable: {error}"))
                })?;
                // EXACTLY the migration's predicate: `TRIM` in the SQL above,
                // then a case-SENSITIVE match against the same truthy set.
                // Migrations are immutable once applied, so the probe conforms
                // to the migration rather than the reverse — a probe that
                // case-folded would refuse an upgrade for a `TRUE` the
                // migration itself treats as disabled.
                if matches!(value.as_str(), "true" | "1" | "yes" | "on") {
                    return Ok(true);
                }
            }
            Ok(None) => {}
            Err(error) if is_missing_table_error(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

/// `SELECT COUNT(*)` with a confirmed-absent table treated as zero and every
/// other failure propagated.
async fn count_rows(
    txn: &sea_orm::DatabaseTransaction,
    table: &str,
    predicate: Option<&str>,
) -> Result<i64, DbErr> {
    let sql = match predicate {
        Some(predicate) => format!("SELECT COUNT(*) FROM `{table}` WHERE {predicate}"),
        None => format!("SELECT COUNT(*) FROM `{table}`"),
    };
    match txn
        .query_one_raw(Statement::from_string(txn.get_database_backend(), sql))
        .await
    {
        Ok(Some(row)) => row
            .try_get_by_index(0)
            .map_err(|error| DbErr::Custom(format!("`{table}` count is undecodable: {error}"))),
        Ok(None) => Ok(0),
        Err(error) if is_missing_table_error(&error) => Ok(0),
        Err(error) => Err(error),
    }
}

fn is_missing_table_error(error: &DbErr) -> bool {
    error
        .to_string()
        .to_ascii_lowercase()
        .contains("no such table")
}

/// The directory holding this connection's SQLite file, via
/// `PRAGMA database_list`. `None` for `:memory:` and anything path-less.
async fn sqlite_database_dir(conn: &DatabaseConnection) -> Option<std::path::PathBuf> {
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "PRAGMA database_list".to_string(),
        ))
        .await
        .ok()??;
    let file: String = row.try_get_by_index(2).ok()?;
    if file.is_empty() {
        return None;
    }
    std::path::Path::new(&file)
        .parent()
        .map(std::path::Path::to_path_buf)
}

/// The version whose rollback needs a check SQL cannot express.
const REGISTRY_V3_CAPABILITY_VERSION: i64 = 2026073005;

/// Refuse to roll back registry v3 while the registry carries live
/// registration generations (plan-20260714 §C.4.1.1).
///
/// The marker is what keeps a v2-era binary from rewriting `worktrees.json`
/// and dropping the generations the service fence depends on. Rolling it away
/// with generations in the file re-opens exactly that hole, and nothing can
/// detect it afterwards because the evidence is what gets dropped. The check
/// cannot be SQL — the registry is a file — so it is here, at the one place
/// that decides whether the down migration runs.
///
/// A repository with no registry, or one whose entries carry no generations
/// (every worktree registered before v3), rolls back freely.
async fn registry_v3_rollback_preflight(conn: &DatabaseConnection) -> Result<(), MigrationError> {
    // The registry belongs to the repository whose DATABASE is being rolled
    // back — asked of the connection itself, not of the process cwd. A rollback
    // run from a linked worktree, or against an explicitly-pathed database,
    // must consult THAT repository's `worktrees.json`; resolving ambiently
    // would check whichever repository the caller happens to be standing in.
    let Some(storage) = sqlite_database_dir(conn).await else {
        // An in-memory or path-less database: no repository to protect.
        return Ok(());
    };
    let path = storage.join("worktrees.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let Ok(document) = serde_json::from_str::<serde_json::Value>(&raw) else {
        // Unreadable registry: the rollback is not the place to adjudicate it,
        // and refusing here would block recovery from an unrelated problem.
        return Ok(());
    };
    let counter = document
        .get("epoch_counter")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let entry_generations = document
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry
                    .get("epoch")
                    .and_then(serde_json::Value::as_u64)
                    .is_some_and(|epoch| epoch > 0)
            })
        });
    if counter > 0 || entry_generations {
        return Err(MigrationError::Other(anyhow::anyhow!(
            "cannot roll back registry v3: '{}' carries live registration generations, and an \
             older binary would silently drop them. The epoch counter is deliberately \
             monotonic and SURVIVES worktree removal, so removing worktrees cannot clear \
             this guard — restore a registry backup taken before the v3 upgrade (or keep \
             running a v3-aware binary)",
            path.display()
        )));
    }
    Ok(())
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

/// The later of the two sequencer re-key migrations. A database at or past it
/// needs no shape normalization, because both rebuilds have already run.
const BISECT_STATE_SCOPE_MIGRATION: i64 = 2026072301;

/// Normalize `rebase_state` to the full pre-scope column set so the static
/// `2026072101_rebase_state_worktree_scope` rebuild can reference every
/// column (plan-20260714 §C.4.2).
///
/// Historically the table's shape was owned by lazy DDL in
/// `command/rebase.rs` — `autosquash`, `todo_actions`, and `empty_mode` were
/// `ALTER TABLE ADD COLUMN`ed on demand — so databases in the wild carry any
/// subset of them, and a static `INSERT .. SELECT` naming a missing column
/// fails at prepare time. This runs BEFORE the migration runner on every
/// connection open and is idempotent by construction: `CREATE TABLE IF NOT
/// EXISTS` covers a missing table, and each `ADD COLUMN` tolerates the
/// "duplicate column name" error (SQLite has no `ADD COLUMN IF NOT EXISTS`).
/// It never touches `worktree_id`, so it is a no-op on an already-migrated
/// table.
async fn normalize_rebase_state_shape(conn: &DatabaseConnection) -> Result<()> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    conn.execute_raw(Statement::from_string(
        DbBackend::Sqlite,
        r#"
            CREATE TABLE IF NOT EXISTS `rebase_state` (
                `id`           INTEGER PRIMARY KEY AUTOINCREMENT,
                `head_name`    TEXT NOT NULL,
                `onto`         TEXT NOT NULL,
                `orig_head`    TEXT NOT NULL,
                `current_head` TEXT NOT NULL,
                `todo`         TEXT NOT NULL,
                `todo_actions` TEXT NOT NULL DEFAULT '',
                `done`         TEXT NOT NULL,
                `stopped_sha`  TEXT,
                `autosquash`   INTEGER NOT NULL DEFAULT 0,
                `empty_mode`   TEXT NOT NULL DEFAULT 'keep'
            );
        "#
        .to_string(),
    ))
    .await
    .with_context(|| "failed to ensure the rebase_state table exists")?;
    for add_column in [
        "ALTER TABLE `rebase_state` ADD COLUMN `autosquash` INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE `rebase_state` ADD COLUMN `todo_actions` TEXT NOT NULL DEFAULT ''",
        "ALTER TABLE `rebase_state` ADD COLUMN `empty_mode` TEXT NOT NULL DEFAULT 'keep'",
    ] {
        if let Err(error) = conn
            .execute_raw(Statement::from_string(
                DbBackend::Sqlite,
                add_column.to_string(),
            ))
            .await
            && !error.to_string().contains("duplicate column name")
        {
            return Err(error)
                .with_context(|| format!("failed to normalize rebase_state shape: {add_column}"));
        }
    }
    Ok(())
}

/// Normalize `bisect_state` to the full lazy column set so the static
/// `2026072301_bisect_state_worktree_scope` rebuild can reference every
/// column (plan-20260714 §C.4.2).
///
/// Historically the table's shape was owned by lazy DDL in
/// `command/bisect.rs` — `completed`, `first_parent`, and `worktree_id` were
/// `ALTER TABLE ADD COLUMN`ed on demand — so databases in the wild carry any
/// subset of them. Same contract as [`normalize_rebase_state_shape`]: runs
/// BEFORE the migration runner on every connection open, idempotent by
/// construction, and a no-op once 2026072301 has re-keyed the table (the
/// rebuilt table has no `id` column, `CREATE IF NOT EXISTS` skips it, and
/// every `ADD COLUMN` hits "duplicate column name").
async fn normalize_bisect_state_shape(conn: &DatabaseConnection) -> Result<()> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    conn.execute_raw(Statement::from_string(
        DbBackend::Sqlite,
        r#"
            CREATE TABLE IF NOT EXISTS `bisect_state` (
                `id`             INTEGER PRIMARY KEY AUTOINCREMENT,
                `orig_head`      TEXT NOT NULL,
                `orig_head_name` TEXT,
                `bad`            TEXT,
                `good`           TEXT NOT NULL,
                `current`        TEXT,
                `skipped`        TEXT,
                `steps`          INTEGER,
                `completed`      INTEGER NOT NULL DEFAULT 0,
                `first_parent`   INTEGER NOT NULL DEFAULT 0,
                `worktree_id`    TEXT NOT NULL DEFAULT ''
            );
        "#
        .to_string(),
    ))
    .await
    .with_context(|| "failed to ensure the bisect_state table exists")?;
    for add_column in [
        "ALTER TABLE `bisect_state` ADD COLUMN `completed` INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE `bisect_state` ADD COLUMN `first_parent` INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE `bisect_state` ADD COLUMN `worktree_id` TEXT NOT NULL DEFAULT ''",
    ] {
        if let Err(error) = conn
            .execute_raw(Statement::from_string(
                DbBackend::Sqlite,
                add_column.to_string(),
            ))
            .await
            && !error.to_string().contains("duplicate column name")
        {
            return Err(error)
                .with_context(|| format!("failed to normalize bisect_state shape: {add_column}"));
        }
    }
    Ok(())
}

/// Run all built-in migrations on the given connection. This is the
/// canonical entry point used by [`crate::internal::db::establish_connection`]
/// (and by tests that want the same wiring as production). Both registry-
/// build errors and per-migration apply errors are surfaced through
/// `anyhow::Error` so the call site can attach its own context.
pub async fn run_builtin_migrations(conn: &DatabaseConnection) -> Result<Vec<i64>> {
    let runner =
        builtin_runner().with_context(|| "failed to build the built-in migration registry")?;
    // The rebase_state/bisect_state shape normalizations must precede the
    // runner: the 2026072101/2026072301 static rebuilds name every pre-scope
    // column, and pre-existing databases carry a lazy-DDL-era subset of them.
    //
    // They run ONLY while those rebuilds are still pending. W1 requires the
    // production path to be free of lazy DDL, and on an already-migrated
    // database these were four DDL statements per connection open forever —
    // no-ops that still took SQLite's schema lock. A database at or past the
    // later rebuild has the re-keyed shape by definition, so there is nothing
    // to normalize.
    let applied = runner
        .current_version_readonly(conn)
        .await
        .with_context(|| "failed to read the current schema version")?;
    if applied.unwrap_or(0) < BISECT_STATE_SCOPE_MIGRATION {
        normalize_rebase_state_shape(conn).await?;
        normalize_bisect_state_shape(conn).await?;
    }
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
        assert_eq!(runner.len(), 58);
        assert!(!runner.is_empty());
        assert_eq!(
            runner.max_registered_version(),
            Some(OPERATION_V2_MIGRATION_VERSION)
        );
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
