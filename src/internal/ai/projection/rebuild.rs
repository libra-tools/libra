//! Rebuild the task→run reverse index while preserving `base_commit_ref`.
//!
//! GC-08: `delete_many` would otherwise drop tagged refs that cannot be
//! recovered from upstream `Run.commit` (IntegrityHash ≠ repository OID).
//! Snapshot `(task_id, run_id, base_commit_ref)` before delete and backfill
//! after insert.

use std::collections::HashMap;

use sea_orm::{ActiveModelTrait, ConnectionTrait, EntityTrait, Set, TransactionTrait};

use super::index::TaskRunIndexRow;
use crate::internal::model::ai_index_task_run;

/// Snapshot of tagged refs keyed by `(task_id, run_id)`.
#[derive(Clone, Debug, Default)]
pub struct RebuildSnapshot {
    refs: HashMap<(String, String), Option<String>>,
}

impl RebuildSnapshot {
    /// Look up a previously snapshotted tagged ref.
    pub fn get(&self, task_id: &str, run_id: &str) -> Option<&Option<String>> {
        self.refs.get(&(task_id.to_string(), run_id.to_string()))
    }

    /// Number of snapshotted keys (diagnostics / tests).
    pub fn len(&self) -> usize {
        self.refs.len()
    }

    /// Whether the snapshot is empty.
    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
    }
}

/// Capture every existing `(task_id, run_id, base_commit_ref)` before a rebuild
/// deletes the projection rows.
pub async fn snapshot_task_run_base_commit_refs<C: ConnectionTrait>(
    conn: &C,
) -> Result<RebuildSnapshot, sea_orm::DbErr> {
    let rows = ai_index_task_run::Entity::find().all(conn).await?;
    let mut refs = HashMap::with_capacity(rows.len());
    for row in rows {
        refs.insert((row.task_id, row.run_id), row.base_commit_ref);
    }
    Ok(RebuildSnapshot { refs })
}

/// Delete the current projection and re-insert `rows`, backfilling
/// `base_commit_ref` from `snapshot` when the rebuilt row omits it.
pub async fn rebuild_task_run_index<C: ConnectionTrait + TransactionTrait>(
    conn: &C,
    rows: &[TaskRunIndexRow],
) -> Result<(), sea_orm::DbErr> {
    let snapshot = snapshot_task_run_base_commit_refs(conn).await?;
    conn.transaction::<_, (), sea_orm::DbErr>(|txn| {
        let rows = rows.to_vec();
        let snapshot = snapshot;
        Box::pin(async move {
            ai_index_task_run::Entity::delete_many().exec(txn).await?;
            for row in rows {
                let tagged = row.base_commit_ref.clone().or_else(|| {
                    snapshot
                        .refs
                        .get(&(row.task_id.clone(), row.run_id.clone()))
                        .cloned()
                        .flatten()
                });
                let active = ai_index_task_run::ActiveModel {
                    task_id: Set(row.task_id),
                    run_id: Set(row.run_id),
                    is_latest: Set(row.is_latest),
                    created_at: Set(row.created_at),
                    base_commit_ref: Set(tagged),
                };
                active.insert(txn).await?;
            }
            Ok(())
        })
    })
    .await
    .map_err(|error| match error {
        sea_orm::TransactionError::Connection(error) => error,
        sea_orm::TransactionError::Transaction(error) => error,
    })?;
    Ok(())
}

/// Insert a single projection row (write path used by rebuild and tests).
pub async fn insert_task_run_row<C: ConnectionTrait>(
    conn: &C,
    row: &TaskRunIndexRow,
) -> Result<(), sea_orm::DbErr> {
    let active = ai_index_task_run::ActiveModel {
        task_id: Set(row.task_id.clone()),
        run_id: Set(row.run_id.clone()),
        is_latest: Set(row.is_latest),
        created_at: Set(row.created_at),
        base_commit_ref: Set(row.base_commit_ref.clone()),
    };
    active.insert(conn).await?;
    Ok(())
}

/// Capture every existing `(task_id, run_id, base_commit_ref)` for `task_ids`
/// via [`crate::storage::GitStateStore`] before a rebuild mutates rows.
pub async fn snapshot_task_run_base_commit_refs_with_store(
    store: &dyn crate::storage::GitStateStore,
    task_ids: &[String],
) -> crate::storage::StorageResult<RebuildSnapshot> {
    let mut unique_ids = std::collections::BTreeSet::new();
    for task_id in task_ids {
        if !task_id.is_empty() {
            unique_ids.insert(task_id.as_str());
        }
    }
    let mut refs = HashMap::new();
    for task_id in unique_ids {
        let rows = store.list_ai_index_task_runs(task_id).await?;
        for row in rows {
            refs.insert((row.task_id, row.run_id), row.base_commit_ref);
        }
    }
    Ok(RebuildSnapshot { refs })
}

/// Rebuild `ai_index_task_run` rows on a backend-agnostic
/// [`crate::storage::GitStateStore`], snapshotting existing `base_commit_ref`
/// values before write and backfilling whenever a rebuilt row omits it (GC-08).
pub async fn rebuild_task_run_index_with_store(
    store: &dyn crate::storage::GitStateStore,
    rows: &[TaskRunIndexRow],
) -> crate::storage::StorageResult<()> {
    let task_ids: Vec<String> = rows.iter().map(|row| row.task_id.clone()).collect();
    let snapshot = snapshot_task_run_base_commit_refs_with_store(store, &task_ids).await?;
    for row in rows {
        let tagged = row.base_commit_ref.clone().or_else(|| {
            snapshot
                .refs
                .get(&(row.task_id.clone(), row.run_id.clone()))
                .cloned()
                .flatten()
        });
        let record = crate::storage::AiIndexTaskRunRecord {
            task_id: row.task_id.clone(),
            run_id: row.run_id.clone(),
            is_latest: row.is_latest,
            created_at: row.created_at,
            base_commit_ref: tagged,
        };
        store.upsert_ai_index_task_run(&record).await?;
    }
    Ok(())
}

/// Insert or upsert a single `ai_index_task_run` row via
/// [`crate::storage::GitStateStore`].
pub async fn insert_task_run_row_with_store(
    store: &dyn crate::storage::GitStateStore,
    row: &TaskRunIndexRow,
) -> crate::storage::StorageResult<()> {
    let record = crate::storage::AiIndexTaskRunRecord {
        task_id: row.task_id.clone(),
        run_id: row.run_id.clone(),
        is_latest: row.is_latest,
        created_at: row.created_at,
        base_commit_ref: row.base_commit_ref.clone(),
    };
    store.upsert_ai_index_task_run(&record).await
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Statement};

    use super::*;
    use crate::internal::{
        ai::projection::resolver::resolve_task_run,
        db,
        db::migration::{MigrationRunner, builtin_migrations, builtin_runner},
    };

    const TAGGED_BLAKE3: &str =
        "repo-commit:blake3:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    async fn column_exists(conn: &impl ConnectionTrait, table: &str, column: &str) -> bool {
        let sql = format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = '{column}'");
        conn.query_one_raw(Statement::from_string(conn.get_database_backend(), sql))
            .await
            .expect("pragma")
            .is_some()
    }

    async fn fresh_conn() -> (tempfile::TempDir, sea_orm::DatabaseConnection) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("libra.db");
        let conn = db::create_database(path.to_str().expect("utf-8"))
            .await
            .expect("create database");
        (dir, conn)
    }

    #[tokio::test]
    async fn ai_task_run_base_commit_ref_migration() {
        let (_dir, conn) = fresh_conn().await;
        assert!(
            column_exists(&conn, "ai_index_task_run", "base_commit_ref").await,
            "tip schema must include base_commit_ref after builtin migrations"
        );

        let runner = builtin_runner().expect("builtin registry");
        assert!(
            runner
                .max_registered_version()
                .is_some_and(|v| v >= 2026092701),
            "B3-16 migration must be registered at tip"
        );
        assert!(
            builtin_migrations()
                .iter()
                .any(|m| m.name == "ai_task_run_base_commit_ref" && m.version == 2026092701),
            "wiring must register ai_task_run_base_commit_ref"
        );

        // Forward replay is a no-op under claim-first.
        let again = runner.run_pending(&conn).await.expect("replay");
        assert!(
            again.is_empty(),
            "forward replay must not re-apply tip migrations: {again:?}"
        );

        // Old-row NULL path: insert without tagged ref.
        let row = TaskRunIndexRow::new("task-legacy", "run-legacy", true, 1, None);
        insert_task_run_row(&conn, &row).await.expect("insert null");
        let resolved = resolve_task_run(&conn, "task-legacy", "run-legacy")
            .await
            .expect("resolve")
            .expect("row");
        assert_eq!(resolved.base_commit_ref, None);

        // Protected down: non-NULL tagged value must refuse.
        insert_task_run_row(
            &conn,
            &TaskRunIndexRow::new(
                "task-tagged",
                "run-tagged",
                false,
                2,
                Some(TAGGED_BLAKE3.to_string()),
            ),
        )
        .await
        .expect("insert tagged");

        let mut down_runner = MigrationRunner::new();
        let migration = builtin_migrations()
            .into_iter()
            .find(|m| m.version == 2026092701)
            .expect("B3-16 migration present");
        down_runner.register(migration).expect("register");
        let down_err = down_runner
            .rollback_to(&conn, 2026091901)
            .await
            .expect_err("protected down must refuse non-NULL tagged refs");
        let rendered = down_err.to_string();
        assert!(
            rendered.contains("base_commit_ref")
                || rendered.contains("non-NULL")
                || rendered.contains("tagged"),
            "protected down error must mention tagged column: {rendered}"
        );
        assert!(
            column_exists(&conn, "ai_index_task_run", "base_commit_ref").await,
            "column must survive refused down"
        );
    }

    #[tokio::test]
    async fn ai_task_run_base_commit_ref_shape_guard() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("shape.db");
        let conn = sea_orm::Database::connect(format!(
            "sqlite:{}?mode=rwc",
            path.to_str().expect("utf-8")
        ))
        .await
        .expect("connect empty");

        // Drifted PK shape (missing run_id) must fail the up shape guard.
        conn.execute_unprepared(
            "CREATE TABLE ai_index_task_run (
                task_id TEXT NOT NULL,
                is_latest INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (task_id)
            )",
        )
        .await
        .expect("create drifted table");
        conn.execute_unprepared(
            "CREATE TABLE IF NOT EXISTS schema_versions (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at TEXT NOT NULL
            )",
        )
        .await
        .expect("schema_versions");

        let mut runner = MigrationRunner::new();
        let migration = builtin_migrations()
            .into_iter()
            .find(|m| m.version == 2026092701)
            .expect("B3-16 migration");
        runner.register(migration).expect("register");
        let err = runner
            .run_pending(&conn)
            .await
            .expect_err("shape guard must refuse drifted PK");
        let rendered = err.to_string();
        assert!(
            rendered.contains("shape guard") || rendered.contains("ai_index_task_run"),
            "shape-guard failure must name the table/guard: {rendered}"
        );
        assert!(
            !column_exists(&conn, "ai_index_task_run", "base_commit_ref").await,
            "column must not be added after shape-guard failure"
        );
    }

    #[tokio::test]
    async fn ai_task_run_base_commit_ref_write_rebuild_read() {
        let (_dir, conn) = fresh_conn().await;
        let tagged = TAGGED_BLAKE3.to_string();
        insert_task_run_row(
            &conn,
            &TaskRunIndexRow::new("task-a", "run-a", true, 10, Some(tagged.clone())),
        )
        .await
        .expect("write tagged");

        // Rebuild with a row that omits the tagged value — snapshot backfill
        // must restore kind fidelity.
        rebuild_task_run_index(
            &conn,
            &[TaskRunIndexRow::new("task-a", "run-a", true, 10, None)],
        )
        .await
        .expect("rebuild");

        let resolved = resolve_task_run(&conn, "task-a", "run-a")
            .await
            .expect("resolve")
            .expect("row");
        assert_eq!(
            resolved.base_commit_ref.as_deref(),
            Some(TAGGED_BLAKE3),
            "write→rebuild→read must preserve tagged kind"
        );
        assert!(
            resolved
                .base_commit_ref
                .as_deref()
                .is_some_and(|v| v.starts_with("repo-commit:blake3:")),
            "resolver context kind must stay blake3"
        );
    }

    #[tokio::test]
    async fn ai_task_run_base_commit_ref_old_binary_unsupported_future() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("libra.db");
        let conn = db::create_database(db_path.to_str().expect("utf-8"))
            .await
            .expect("create tip database");
        assert!(
            column_exists(&conn, "ai_index_task_run", "base_commit_ref").await,
            "new library tip includes base_commit_ref"
        );
        // Plant a schema version above this binary's tip — same refuse path an
        // older binary hits when opening a DB that already applied B3-16+.
        conn.execute_unprepared(
            "INSERT INTO schema_versions (version, name, applied_at) \
             VALUES (2126092701, 'post_b3_16_future', datetime('now'))",
        )
        .await
        .expect("plant future version");
        drop(conn);

        let error = db::establish_connection(db_path.to_str().expect("utf-8"))
            .await
            .expect_err("future schema must refuse before SeaORM SELECT");
        let rendered = error.to_string();
        assert!(
            rendered.contains("newer than this Libra binary supports"),
            "must use UnsupportedFuture open path: {rendered}"
        );
        assert!(
            rendered.contains("2126092701"),
            "refusal must name the unsupported version: {rendered}"
        );
    }

    #[tokio::test]
    async fn ai_task_run_base_commit_ref_survives_projection_rebuild() {
        let (_dir, conn) = fresh_conn().await;
        insert_task_run_row(
            &conn,
            &TaskRunIndexRow::new(
                "task-keep",
                "run-keep",
                true,
                42,
                Some(TAGGED_BLAKE3.to_string()),
            ),
        )
        .await
        .expect("seed tagged row");

        // Empty the projection then rebuild the same keys — snapshot taken
        // inside rebuild_task_run_index must re-attach the tagged column.
        // (Caller supplies rows without base_commit_ref to force backfill.)
        let before = snapshot_task_run_base_commit_refs(&conn)
            .await
            .expect("snapshot");
        assert_eq!(before.len(), 1);
        assert_eq!(
            before
                .get("task-keep", "run-keep")
                .cloned()
                .flatten()
                .as_deref(),
            Some(TAGGED_BLAKE3)
        );

        rebuild_task_run_index(
            &conn,
            &[TaskRunIndexRow::new(
                "task-keep",
                "run-keep",
                true,
                42,
                None,
            )],
        )
        .await
        .expect("rebuild after clear path");

        let resolved = resolve_task_run(&conn, "task-keep", "run-keep")
            .await
            .expect("resolve")
            .expect("row");
        assert_eq!(
            resolved.base_commit_ref.as_deref(),
            Some(TAGGED_BLAKE3),
            "tagged column must survive projection rebuild"
        );
    }
}
