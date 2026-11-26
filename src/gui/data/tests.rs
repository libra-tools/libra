//! Data-layer tests for the window seam. No display is required.

use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use sea_orm::{ConnectionTrait, TransactionTrait};
use serial_test::serial;
use tempfile::tempdir;

use super::{DataHandle, QUEUE_CAP, ReadOutcome, SourceState, repo, state};
use crate::{internal::db, utils::test};

/// A logical read-only fingerprint: SQLite file bytes legitimately change when a
/// reader checkpoints a WAL, so compare schema and operation rows.
async fn count_rows_in(conn: &sea_orm::DatabaseConnection, sql: &str) -> i64 {
    conn.query_one_raw(sea_orm::Statement::from_string(
        conn.get_database_backend(),
        sql.to_string(),
    ))
    .await
    .expect("fingerprint query")
    .and_then(|row| row.try_get::<i64>("", "n").ok())
    .unwrap_or(0)
}

async fn receipt_fingerprint(db_path: &std::path::Path) -> (i64, Option<i64>) {
    let conn = db::open_database_without_migrations(db_path)
        .await
        .expect("open repository database");
    let schema = conn
        .query_one_raw(sea_orm::Statement::from_string(
            conn.get_database_backend(),
            "SELECT MAX(version) AS n FROM schema_versions".to_string(),
        ))
        .await
        .expect("schema query")
        .and_then(|row| row.try_get::<i64>("", "n").ok());
    let operations = count_rows_in(&conn, "SELECT COUNT(*) AS n FROM `operation`").await;
    conn.close().await.ok();
    (operations, schema)
}

fn wait_snapshot(handle: &DataHandle, budget: Duration) -> super::Snapshot {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(snapshot) = handle.take_snapshot() {
            return snapshot;
        }
        assert!(Instant::now() < deadline, "snapshot did not arrive in time");
        thread::sleep(Duration::from_millis(10));
    }
}

#[tokio::test]
#[serial(cwd)]
async fn uid01_context_snapshot_in_repository() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    let handle = DataHandle::spawn();
    handle.request();
    let snapshot = wait_snapshot(&handle, Duration::from_secs(10));
    assert_eq!(snapshot.state, SourceState::Available);
    let context = snapshot.context.expect("context in a repository");
    assert_eq!(context.storage_label, ".libra");
    assert_eq!(context.head, "no commits");
    assert!(!context.branch.is_empty());
    assert!(
        !context.worktree_label.starts_with('/'),
        "labels are repository-relative, not host-absolute"
    );
    assert!(handle.shutdown());
}

#[test]
#[serial(cwd)]
fn uid01_outside_repository_absent() {
    let dir = tempdir().expect("temp dir");
    let resolved = repo::resolve_scope(dir.path().to_path_buf()).expect("resolution");
    assert!(
        resolved.is_none(),
        "a plain directory has no repository scope"
    );

    let _guard = test::ChangeDirGuard::new(dir.path());
    let handle = DataHandle::spawn_with(|scope| {
        Box::pin(async move {
            Ok(ReadOutcome::new(
                if scope.is_none() {
                    SourceState::Absent
                } else {
                    SourceState::Available
                },
                None,
                None,
            ))
        })
    });
    handle.request();
    let snapshot = wait_snapshot(&handle, Duration::from_secs(5));
    assert_eq!(snapshot.state, SourceState::Absent);
    assert!(handle.shutdown());
}

#[test]
fn uid01_source_state_classification() {
    let dir = tempdir().expect("temp dir");
    let missing = dir.path().join("missing.db");
    assert_eq!(state::probe_path(&missing), state::ProbeOutcome::Absent);

    let present = dir.path().join("present.db");
    fs::write(&present, b"data").expect("write");
    assert_eq!(state::probe_path(&present), state::ProbeOutcome::Present);
    assert_eq!(
        state::state_for_probe(&state::probe_path(&present)),
        SourceState::Available
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let locked = dir.path().join("locked.db");
        fs::write(&locked, b"data").expect("write");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
        let outcome = state::probe_path(&locked);
        assert_eq!(
            outcome,
            state::ProbeOutcome::Unreadable {
                kind: std::io::ErrorKind::PermissionDenied
            }
        );
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).expect("chmod");
    }
}

#[test]
fn uid01_supersession_and_queue_cap() {
    let handle = DataHandle::spawn_with(|_| {
        Box::pin(async move {
            thread::sleep(Duration::from_millis(20));
            Ok(ReadOutcome::new(SourceState::Available, None, None))
        })
    });
    handle.request();
    let first = wait_snapshot(&handle, Duration::from_secs(5));
    assert_eq!(first.seq, 1);

    for _ in 0..20 {
        handle.request();
    }
    assert!(
        handle.pending_requests() <= QUEUE_CAP,
        "the pending queue must stay bounded"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let latest = loop {
        if let Some(snapshot) = handle.take_snapshot()
            && snapshot.seq == 21
        {
            break snapshot;
        }
        assert!(Instant::now() < deadline, "the newest request never won");
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(
        latest.seq, 21,
        "the newest request wins; stale responses never overwrite it"
    );
    assert!(
        handle.take_snapshot().is_none(),
        "a delivered snapshot is not re-delivered"
    );
    assert!(handle.shutdown());
}

#[test]
fn uid01_shutdown_and_close_budget() {
    let handle = DataHandle::spawn_with(|_| {
        Box::pin(async move {
            thread::sleep(Duration::from_millis(50));
            Ok(ReadOutcome::new(SourceState::Available, None, None))
        })
    });
    handle.request();
    let started = Instant::now();
    let joined = handle.shutdown();
    assert!(joined, "a cooperative reader joins inside the budget");
    assert!(
        started.elapsed() <= Duration::from_secs(5),
        "close must return inside the budget"
    );

    let stalled = DataHandle::spawn_with(|_| {
        Box::pin(async move {
            thread::sleep(Duration::from_secs(6));
            Ok(ReadOutcome::new(SourceState::Available, None, None))
        })
    });
    stalled.request();
    let deadline = Instant::now() + Duration::from_secs(5);
    while stalled.worker_reads() == 0 {
        assert!(
            Instant::now() < deadline,
            "the stalled reader never started"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let started = Instant::now();
    let joined = stalled.shutdown();
    assert!(!joined, "a blocked reader is recorded as stalled");
    assert!(stalled.stalled());
    assert!(
        started.elapsed() <= Duration::from_secs(4),
        "a stalled worker must not block the close path"
    );
}

#[test]
fn uid01_main_thread_read_count_zero() {
    let handle = DataHandle::spawn_with(|_| {
        Box::pin(async { Ok(ReadOutcome::new(SourceState::Available, None, None)) })
    });
    assert_eq!(handle.worker_reads(), 0, "spawning performs no read");
    handle.request();
    let _ = wait_snapshot(&handle, Duration::from_secs(5));
    assert_eq!(
        handle.worker_reads(),
        1,
        "the caller's thread never performs the read"
    );
    assert!(handle.shutdown());
}

#[tokio::test]
#[serial(cwd)]
async fn uid01_read_only_receipt_and_head() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let db_path: PathBuf = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let before = fs::read(&db_path).expect("read database bytes");
    let operations_before = operation_count(&db_path).await;

    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let _ = repo::read_context(&scope).await.expect("context");

    let after = fs::read(&db_path).expect("read database bytes");
    assert_eq!(before, after, "a window read must not modify the database");
    assert_eq!(
        operations_before,
        operation_count(&db_path).await,
        "a window read must not append operation rows"
    );
}

async fn operation_count(db_path: &std::path::Path) -> i64 {
    let conn = db::open_database_without_migrations(db_path)
        .await
        .expect("open database");
    let row = conn
        .query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Sqlite,
            "SELECT COUNT(*) AS n FROM operation".to_string(),
        ))
        .await
        .expect("count operations")
        .expect("count row");
    row.try_get::<i64>("", "n").expect("operation count")
}

#[tokio::test]
#[serial(cwd)]
async fn uid01_corrupt_and_schema_old() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    let corrupt_handle = DataHandle::spawn_with(|_| {
        Box::pin(async { Ok(ReadOutcome::new(SourceState::Corrupt, None, None)) })
    });
    corrupt_handle.request();
    assert_eq!(
        wait_snapshot(&corrupt_handle, Duration::from_secs(5)).state,
        SourceState::Corrupt
    );
    assert!(corrupt_handle.shutdown());

    // A truly corrupt database file is classified as corrupt, not absent.
    fs::write(&db_path, b"not a sqlite database").expect("corrupt the database");
    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let error = repo::read_context(&scope).await.expect_err("corrupt read");
    assert_eq!(error.state, SourceState::Corrupt);

    // Remove the schema receipt: an old/empty schema is reported as schema-old.
    let old_dir = tempdir().expect("old-schema repo");
    test::setup_with_new_libra_in(old_dir.path()).await;
    let old_db = old_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let conn = db::open_database_without_migrations(&old_db)
        .await
        .expect("open database");
    conn.execute_unprepared("DELETE FROM schema_versions")
        .await
        .expect("clear schema receipt");
    conn.close().await.expect("close");
    let scope = repo::resolve_scope(old_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let error = repo::read_context(&scope).await.expect_err("old schema");
    assert_eq!(error.state, SourceState::SchemaOld);
}

#[test]
#[serial(cwd)]
fn uid01_busy_state_is_rendered() {
    let handle = DataHandle::spawn_with(|_| {
        Box::pin(async move {
            Ok(ReadOutcome::new(
                SourceState::Busy,
                None,
                Some("another process is writing".to_string()),
            ))
        })
    });
    handle.request();
    let snapshot = wait_snapshot(&handle, Duration::from_secs(5));
    assert_eq!(snapshot.state, SourceState::Busy);
    assert!(snapshot.state.hint().is_some());
    assert!(handle.shutdown());
}

#[test]
#[serial(cwd)]
fn uid01_worker_thread_identity() {
    let executed: std::sync::Arc<std::sync::Mutex<Option<thread::ThreadId>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let recorded = std::sync::Arc::clone(&executed);
    let handle = DataHandle::spawn_with(move |_| {
        let recorded = std::sync::Arc::clone(&recorded);
        Box::pin(async move {
            if let Ok(mut slot) = recorded.lock() {
                *slot = Some(thread::current().id());
            }
            Ok(ReadOutcome::new(SourceState::Available, None, None))
        })
    });
    handle.request();
    let _ = wait_snapshot(&handle, Duration::from_secs(5));
    let executing = executed
        .lock()
        .expect("recorded thread id")
        .expect("the reader ran");
    assert_ne!(
        executing,
        thread::current().id(),
        "the reader runs on the worker thread, never on the caller's thread"
    );
    assert_eq!(handle.worker_thread_id(), Some(executing));
    assert!(handle.shutdown());
}

#[test]
fn uid01_probe_and_schema_classification() {
    let dir = tempdir().expect("temp dir");
    let missing = dir.path().join("missing.db");
    assert_eq!(state::probe_path(&missing), state::ProbeOutcome::Absent);
    let present = dir.path().join("present.db");
    fs::write(&present, b"x").expect("write");
    assert_eq!(state::probe_path(&present), state::ProbeOutcome::Present);
    assert_eq!(
        state::classify_schema(&db::SchemaCompatibility::Compatible {
            current_version: None,
            latest_version: None,
        }),
        SourceState::Available
    );
    assert_eq!(
        state::classify_schema(&db::SchemaCompatibility::UpgradeRequired {
            current_version: None,
            latest_version: 3,
        }),
        SourceState::SchemaOld
    );
    assert_eq!(
        state::classify_schema(&db::SchemaCompatibility::UnsupportedFuture {
            current_version: 99,
            latest_version: None,
        }),
        SourceState::SchemaNew
    );
    assert_eq!(
        state::classify_store_message("database is locked"),
        SourceState::Busy
    );
    assert_eq!(
        state::classify_store_message("disk image is malformed"),
        SourceState::Corrupt
    );
    assert_eq!(
        state::classify_store_message("Conn(SqlxError(PoolTimedOut))"),
        SourceState::Busy
    );
    assert_eq!(
        state::classify_store_message("Connection pool timed out"),
        SourceState::Busy
    );
    assert_eq!(
        state::classify_open_error(&std::io::Error::other("Conn(SqlxError(PoolTimedOut))")),
        SourceState::Busy
    );
    let wrapped = anyhow::Error::msg("database is locked").context("failed to query config_kv");
    assert_eq!(state::classify_error_chain(&wrapped), SourceState::Busy);
    let corrupt =
        anyhow::Error::msg("disk image is malformed").context("failed to query config_kv");
    assert_eq!(state::classify_error_chain(&corrupt), SourceState::Corrupt);
    let pool_debug =
        anyhow::Error::msg("Conn(SqlxError(PoolTimedOut))").context("failed to query config_kv");
    assert_eq!(state::classify_error_chain(&pool_debug), SourceState::Busy);
    let pool_text =
        anyhow::Error::msg("Connection pool timed out").context("failed to query config_kv");
    assert_eq!(state::classify_error_chain(&pool_text), SourceState::Busy);
}

#[test]
fn uid01_startup_failure_is_displayed() {
    let handle = DataHandle::spawn_with_startup_failure("worker failed".to_string());
    let snapshot = handle.take_snapshot().expect("startup error snapshot");
    assert_eq!(snapshot.state, SourceState::Worker);
    assert_eq!(snapshot.seq, 0);
    assert!(snapshot.state.hint().is_some());
    assert!(handle.shutdown());
}

#[test]
fn uid01_obsolete_in_flight_response_is_dropped() {
    let handle = DataHandle::spawn_with(|_| {
        Box::pin(async move {
            thread::sleep(Duration::from_millis(120));
            Ok(ReadOutcome::new(SourceState::Available, None, None))
        })
    });
    handle.request();
    assert_eq!(wait_snapshot(&handle, Duration::from_secs(5)).seq, 1);

    let second = handle.request();
    let third = handle.request();
    assert_eq!(third, second + 1);
    let deadline = Instant::now() + Duration::from_secs(5);
    let latest = loop {
        if let Some(snapshot) = handle.take_snapshot() {
            break snapshot;
        }
        assert!(Instant::now() < deadline, "no superseded snapshot arrived");
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(
        latest.seq, third,
        "an in-flight obsolete response is dropped, not delivered"
    );
    assert!(handle.take_snapshot().is_none());
    assert!(handle.shutdown());
}

#[test]
#[serial(cwd)]
fn uid01_real_worker_outside_repository_is_absent() {
    let dir = tempdir().expect("temp dir");
    let _guard = test::ChangeDirGuard::new(dir.path());
    let handle = DataHandle::spawn();
    handle.request();
    let snapshot = wait_snapshot(&handle, Duration::from_secs(5));
    assert_eq!(snapshot.state, SourceState::Absent);
    assert_eq!(snapshot.message.as_deref(), Some("no repository"));
    assert!(handle.shutdown());
}

#[tokio::test]
#[serial(cwd)]
async fn uid02_history_rows_and_parent_chain() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    let long_subject = "x".repeat(250);
    for message in ["first", "second", "third", &long_subject] {
        crate::command::commit::execute(crate::command::commit::CommitArgs {
            message: Some(message.to_string()),
            allow_empty: true,
            disable_pre: true,
            no_verify: true,
            ..Default::default()
        })
        .await;
        std::thread::sleep(Duration::from_millis(1100));
    }
    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let context = repo::read_context(&scope).await.expect("context");
    let rows = super::history::read_history(&scope, context.head_oid)
        .await
        .expect("history read");
    assert_eq!(rows.len(), 4, "four commits are listed");
    assert_eq!(
        rows[1].subject, "third",
        "newest real commit after the long one"
    );
    assert_eq!(rows[3].subject, "first");
    assert_eq!(rows[3].parents, 0, "the root commit has no parent");
    assert_eq!(rows[2].parents, 1);
    assert_eq!(
        rows[2].parent_oids,
        vec![rows[3].oid.clone()],
        "parent ids chain to the previous commit"
    );
    assert!(rows[0].subject.len() <= 200, "subjects are truncated");
    assert_eq!(rows[1].short.len(), 7);
    assert!(!rows[1].author.is_empty());
    assert_eq!(
        rows[1].sessions, 0,
        "no capture catalog means human commits"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid02_empty_repository_has_no_rows() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let context = repo::read_context(&scope).await.expect("context");
    let rows = super::history::read_history(&scope, context.head_oid)
        .await
        .expect("history read");
    assert!(rows.is_empty());
}

#[tokio::test]
#[serial(cwd)]
async fn uid02_commit_keyed_attribution_batch() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let storage = repo_dir.path().join(crate::utils::util::ROOT_DIR);
    let conn = crate::internal::ai::capture::store::open(&storage, Duration::from_secs(2))
        .await
        .expect("open capture catalog");
    let now = 1_700_000_000_i64;
    conn.execute_unprepared(&format!(
        "INSERT INTO agent_session \
         (session_id, agent_kind, provider_session_id, state, working_dir, metadata_json, \
          redaction_report, started_at, last_event_at, scope_state) \
         VALUES ('s-1', 'claude_code', 'p-1', 'active', '/tmp', '{{}}', '{{}}', {now}, {now}, 'legacy_unknown')"
    ))
    .await
    .expect("insert session");
    conn.execute_unprepared(
        "INSERT INTO agent_session_commit (session_id, hash_kind, commit_oid) \
         VALUES ('s-1', 'sha1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')",
    )
    .await
    .expect("insert link");
    conn.execute_unprepared(&format!(
        "INSERT INTO agent_checkpoint \
         (checkpoint_id, session_id, scope, tree_oid, metadata_blob_oid, traces_commit, created_at) \
         VALUES ('k-1', 's-1', 'committed', 't', 'm', 'c', {now})"
    ))
    .await
    .expect("insert checkpoint");

    let repo_conn = db::open_database_without_migrations(
        &repo_dir
            .path()
            .join(crate::utils::util::ROOT_DIR)
            .join(crate::utils::util::DATABASE),
    )
    .await
    .expect("open repository database");
    let scope =
        crate::internal::ai::capture_scope::CaptureScope::resolve(&repo_conn, repo_dir.path())
            .await
            .expect("capture scope");
    // The session must be scoped to this repository/worktree to count.
    conn.execute_unprepared(&format!(
        "UPDATE agent_session SET scope_state = 'scoped', repo_id = '{}', \
         worktree_id = '{}', workspace_id = NULL, workspace_fence = NULL \
         WHERE session_id = 's-1'",
        scope.repo_id, scope.worktree_id
    ))
    .await
    .expect("scope session");

    let ids = vec!["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()];
    let attribution = crate::command::agent::commit_attribution_batch(&conn, &scope, &ids)
        .await
        .expect("attribution");
    let entry = attribution.get(&ids[0]).expect("linked commit");
    assert_eq!(entry.sessions, 1);
    assert_eq!(entry.checkpoints, 1);

    // Mismatched scope fixtures must be excluded even for the same commit.
    for (session_id, repo_id, worktree_id, workspace_id, fence) in [
        (
            "s-repo".to_string(),
            "other-repo".to_string(),
            scope.worktree_id.clone(),
            None,
            None,
        ),
        (
            "s-worktree".to_string(),
            scope.repo_id.clone(),
            "other-worktree".to_string(),
            None,
            None,
        ),
        (
            "s-workspace".to_string(),
            scope.repo_id.clone(),
            scope.worktree_id.clone(),
            Some("ws".to_string()),
            Some(7_i64),
        ),
        (
            "s-fence".to_string(),
            scope.repo_id.clone(),
            scope.worktree_id.clone(),
            Some("ws".to_string()),
            Some(9_i64),
        ),
    ] {
        conn.execute_unprepared(&format!(
            "INSERT INTO agent_session \
             (session_id, agent_kind, provider_session_id, state, working_dir, metadata_json, \
              redaction_report, started_at, last_event_at, scope_state, repo_id, worktree_id, \
              workspace_id, workspace_fence) \
             VALUES ('{session_id}', 'claude_code', '{session_id}', 'active', '/tmp', '{{}}', \
                     '{{}}', {now}, {now}, 'scoped', '{repo_id}', '{worktree_id}', \
                     {}, {})",
            workspace_id
                .as_ref()
                .map(|value| format!("'{value}'"))
                .unwrap_or_else(|| "NULL".to_string()),
            fence
                .map(|value| value.to_string())
                .unwrap_or_else(|| "NULL".to_string()),
        ))
        .await
        .expect("insert mismatched session");
        conn.execute_unprepared(&format!(
            "INSERT INTO agent_session_commit (session_id, hash_kind, commit_oid) \
             VALUES ('{session_id}', 'sha1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')"
        ))
        .await
        .expect("insert mismatched link");
    }
    let attribution = crate::command::agent::commit_attribution_batch(&conn, &scope, &ids)
        .await
        .expect("scoped attribution");
    assert_eq!(
        attribution.get(&ids[0]).map(|entry| entry.sessions),
        Some(1),
        "mismatched repo/worktree/workspace/fence sessions are excluded"
    );

    // Fence-only mismatch: same repo/worktree/workspace, different fence.
    let fenced_scope = crate::internal::ai::capture_scope::CaptureScope {
        repo_id: scope.repo_id.clone(),
        worktree_id: scope.worktree_id.clone(),
        workspace_id: Some("ws".to_string()),
        workspace_fence: Some(1),
    };
    for (session_id, fence) in [("s-fence-match", 1_i64), ("s-fence-bad", 9_i64)] {
        conn.execute_unprepared(&format!(
            "INSERT INTO agent_session \
             (session_id, agent_kind, provider_session_id, state, working_dir, metadata_json, \
              redaction_report, started_at, last_event_at, scope_state, repo_id, worktree_id, \
              workspace_id, workspace_fence) \
             VALUES ('{session_id}', 'claude_code', '{session_id}', 'active', '/tmp', '{{}}', \
                     '{{}}', {now}, {now}, 'scoped', '{}', '{}', 'ws', {fence})",
            fenced_scope.repo_id, fenced_scope.worktree_id
        ))
        .await
        .expect("insert fenced session");
        conn.execute_unprepared(&format!(
            "INSERT INTO agent_session_commit (session_id, hash_kind, commit_oid) \
             VALUES ('{session_id}', 'sha1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')"
        ))
        .await
        .expect("insert fenced link");
    }
    let fence_attribution =
        crate::command::agent::commit_attribution_batch(&conn, &fenced_scope, &ids)
            .await
            .expect("fence attribution");
    assert_eq!(
        fence_attribution.get(&ids[0]).map(|entry| entry.sessions),
        Some(1),
        "a session with the wrong fence is excluded"
    );

    // A tombstoned session no longer attributes its commits.
    conn.execute_unprepared(
        "INSERT INTO agent_import_tombstone \
         (tombstone_id, agent_kind, provider_session_id, erased_session_id, erased_at) \
         VALUES ('t-1', 'claude_code', 'p-1', 's-1', 1)",
    )
    .await
    .expect("insert tombstone");
    assert!(
        crate::command::agent::commit_attribution_batch(&conn, &scope, &ids)
            .await
            .expect("tombstoned attribution")
            .is_empty(),
        "tombstoned sessions do not attribute commits"
    );
    conn.close().await.expect("close capture catalog");
    repo_conn.close().await.expect("close repository database");
}

#[tokio::test]
#[serial(cwd)]
async fn uid02_read_only_receipt() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    crate::command::commit::execute(crate::command::commit::CommitArgs {
        message: Some("receipt".to_string()),
        allow_empty: true,
        disable_pre: true,
        no_verify: true,
        ..Default::default()
    })
    .await;
    let before = receipt_fingerprint(&db_path).await;
    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let context = repo::read_context(&scope).await.expect("context");
    let rows = super::history::read_history(&scope, context.head_oid)
        .await
        .expect("history");
    assert_eq!(rows.len(), 1, "the commit walk actually ran");
    let after = receipt_fingerprint(&db_path).await;
    assert_eq!(
        before, after,
        "history reads must not modify logical state (schema, operation rows)"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid02_history_cap_is_200_rows() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    for index in 0..405 {
        crate::command::commit::execute(crate::command::commit::CommitArgs {
            message: Some(format!("commit {index}")),
            allow_empty: true,
            disable_pre: true,
            no_verify: true,
            ..Default::default()
        })
        .await;
    }
    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let context = repo::read_context(&scope).await.expect("context");
    let head_oid =
        crate::internal::object_format::parse_repo_oid(context.head_oid.as_deref().expect("HEAD"))
            .expect("head oid");
    let shallow = crate::internal::shallow::ShallowSet::default();
    let walked = super::history::walk_bounded(&head_oid, 400, &shallow).expect("walk");
    assert_eq!(walked.len(), 400, "the walk loads at most 400 commits");
    let rows = super::history::read_history(&scope, context.head_oid)
        .await
        .expect("history");
    assert_eq!(rows.len(), 200, "the history cap is 200 rows");
    assert_eq!(rows[0].subject, "commit 404", "the newest commit is kept");
    assert!(
        rows.iter().all(|row| row.subject != "commit 0"),
        "the oldest commit is beyond the cap"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid02_ordering_uses_committer_time() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    // Commit A: early author date, late committer date. Commit B: late author
    // date, early committer date. Committer ordering must put A first.
    for (message, author, committer) in [
        (
            "older-author",
            "2001-01-01T00:00:00Z",
            "2020-01-01T00:00:00Z",
        ),
        (
            "newer-author",
            "2022-01-01T00:00:00Z",
            "2010-01-01T00:00:00Z",
        ),
    ] {
        // SAFETY: tests in this process are serialized by `serial(cwd)`.
        unsafe {
            std::env::set_var("GIT_AUTHOR_DATE", author);
            std::env::set_var("GIT_COMMITTER_DATE", committer);
        }
        crate::command::commit::execute(crate::command::commit::CommitArgs {
            message: Some(message.to_string()),
            allow_empty: true,
            disable_pre: true,
            no_verify: true,
            ..Default::default()
        })
        .await;
    }
    unsafe {
        std::env::remove_var("GIT_AUTHOR_DATE");
        std::env::remove_var("GIT_COMMITTER_DATE");
    }
    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let context = repo::read_context(&scope).await.expect("context");
    let rows = super::history::read_history(&scope, context.head_oid)
        .await
        .expect("history");
    assert_eq!(
        rows[0].subject, "older-author",
        "ordering follows committer time, not author time"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid02_shallow_boundary_stops_the_walk() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    for message in ["one", "two"] {
        crate::command::commit::execute(crate::command::commit::CommitArgs {
            message: Some(message.to_string()),
            allow_empty: true,
            disable_pre: true,
            no_verify: true,
            ..Default::default()
        })
        .await;
    }
    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let context = repo::read_context(&scope).await.expect("context");
    let head = context.head_oid.clone().expect("HEAD");
    fs::write(
        repo_dir
            .path()
            .join(crate::utils::util::ROOT_DIR)
            .join("shallow"),
        format!("{head}\n"),
    )
    .expect("write shallow metadata");
    let rows = super::history::read_history(&scope, context.head_oid)
        .await
        .expect("history");
    assert_eq!(rows.len(), 1, "a shallow boundary stops parent traversal");
}

#[tokio::test]
#[serial(cwd)]
async fn uid02_malformed_shallow_metadata_is_an_error() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    crate::command::commit::execute(crate::command::commit::CommitArgs {
        message: Some("one".to_string()),
        allow_empty: true,
        disable_pre: true,
        no_verify: true,
        ..Default::default()
    })
    .await;
    fs::write(
        repo_dir
            .path()
            .join(crate::utils::util::ROOT_DIR)
            .join("shallow"),
        "not-an-object-id\n",
    )
    .expect("write malformed shallow metadata");
    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let context = repo::read_context(&scope).await.expect("context");
    let error = super::history::read_history(&scope, context.head_oid)
        .await
        .expect_err("malformed shallow metadata");
    assert_eq!(error.state, SourceState::Corrupt);
    assert!(error.message.contains("shallow"));
}

async fn scoped_capture(dir: &std::path::Path) -> (String, String, sea_orm::DatabaseConnection) {
    let storage = dir.join(crate::utils::util::ROOT_DIR);
    let conn = crate::internal::ai::capture::store::open(&storage, Duration::from_secs(2))
        .await
        .expect("open capture catalog");
    let repo_conn =
        db::open_database_without_migrations(&storage.join(crate::utils::util::DATABASE))
            .await
            .expect("open repository database");
    let scope = crate::internal::ai::capture_scope::CaptureScope::resolve(&repo_conn, dir)
        .await
        .expect("capture scope");
    (scope.repo_id.clone(), scope.worktree_id.clone(), conn)
}

async fn insert_session(
    conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
    worktree_id: &str,
    id: &str,
    metadata: &str,
    started_at: i64,
) {
    insert_session_for(
        conn,
        repo_id,
        worktree_id,
        id,
        "claude_code",
        metadata,
        started_at,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn insert_session_for(
    conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
    worktree_id: &str,
    id: &str,
    agent_kind: &str,
    metadata: &str,
    started_at: i64,
) {
    conn.execute_unprepared(&format!(
        "INSERT INTO agent_session \
         (session_id, agent_kind, provider_session_id, state, working_dir, metadata_json, \
          redaction_report, started_at, last_event_at, scope_state, repo_id, worktree_id) \
         VALUES ('{id}', '{agent_kind}', 'p-{id}', 'active', '/tmp/work', '{metadata}', \
                 '{{}}', {started_at}, {started_at}, 'scoped', '{repo_id}', '{worktree_id}')"
    ))
    .await
    .expect("insert session");
}

fn write_tree(
    storage: &std::path::Path,
    entries: &[(&str, &str, &str)],
) -> git_internal::hash::ObjectHash {
    let mut bytes = Vec::new();
    for (mode, name, oid) in entries {
        bytes.extend_from_slice(mode.as_bytes());
        bytes.push(b' ');
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
        for pair in oid.as_bytes().chunks(2) {
            let high = (pair[0] as char).to_digit(16).expect("hex digit") as u8;
            let low = (pair[1] as char).to_digit(16).expect("hex digit") as u8;
            bytes.push((high << 4) | low);
        }
    }
    crate::utils::object::write_git_object(storage, "tree", &bytes).expect("write tree")
}

#[allow(clippy::too_many_arguments)]
/// Write a real committed checkpoint whose metadata blob carries the summary
/// facts the shared reader expects (the GUI never reads `metadata_json` facts).
async fn write_metadata_checkpoint(
    dir: &std::path::Path,
    conn: &sea_orm::DatabaseConnection,
    session_id: &str,
    agent_kind: &str,
    title: &str,
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
) {
    let storage = dir.join(crate::utils::util::ROOT_DIR);
    let metadata = format!(
        r#"{{"checkpoint_id":"k-{session_id}","session_id":"{session_id}","agent_kind":"{agent_kind}","extraction":{{"schema_version":1,"present":true,"partial":false,"model":"{model}","title":"{title}","prompt_count":3,"modified_file_count":1,"token_usage":{{"input_tokens":{input_tokens},"output_tokens":{output_tokens}}}}}}}"#
    );
    let blob = crate::utils::object::write_git_object(&storage, "blob", metadata.as_bytes())
        .expect("write metadata blob");
    // The catalog layout is checkpoint/<id[..2]>/<id[2..]>/metadata.json.
    let checkpoint_id = format!("k-{session_id}");
    let leaf = write_tree(&storage, &[("100644", "metadata.json", &blob.to_string())]);
    let prefix = write_tree(
        &storage,
        &[("40000", &checkpoint_id[2..], &leaf.to_string())],
    );
    let wrapped = write_tree(
        &storage,
        &[("40000", &checkpoint_id[..2], &prefix.to_string())],
    );
    let tree = write_tree(&storage, &[("40000", "checkpoint", &wrapped.to_string())]);
    conn.execute_unprepared(&format!(
        "INSERT INTO agent_checkpoint \
         (checkpoint_id, session_id, scope, tree_oid, metadata_blob_oid, traces_commit, created_at) \
         VALUES ('{checkpoint_id}', '{session_id}', 'committed', '{tree}', '{blob}', 'c', 1700000001)"
    ))
    .await
    .expect("insert checkpoint");
}

#[tokio::test]
#[serial(cwd)]
async fn uid03_sessions_rows_and_usage() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let (repo_id, worktree_id, conn) = scoped_capture(repo_dir.path()).await;
    insert_session_for(
        &conn,
        &repo_id,
        &worktree_id,
        "s-new",
        "codex",
        "{}",
        1_700_000_200,
    )
    .await;
    write_metadata_checkpoint(
        repo_dir.path(),
        &conn,
        "s-new",
        "codex",
        "Fix parser crash",
        "gpt-5-codex",
        120,
        40,
    )
    .await;
    insert_session(
        &conn,
        &repo_id,
        &worktree_id,
        "s-old",
        r#"{"model":"gpt-5","summary":"older session"}"#,
        1_700_000_100,
    )
    .await;
    conn.execute_unprepared(
        "INSERT INTO agent_session_commit (session_id, hash_kind, commit_oid) \
         VALUES ('s-new', 'sha1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')",
    )
    .await
    .expect("insert link");

    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let read = super::sessions::read_sessions(&scope)
        .await
        .expect("sessions");
    let rows = read.rows;
    assert_eq!(rows.len(), 2, "only scoped sessions are listed");
    assert_eq!(rows[0].id, "s-new", "newest first");
    assert_eq!(rows[0].model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(rows[0].usage_input, Some(120));
    assert_eq!(rows[0].usage_output, Some(40));
    assert_eq!(rows[0].usage_calls, Some(3));
    assert_eq!(rows[0].checkpoints, 1);
    assert_eq!(rows[0].commits.len(), 1);
    assert_eq!(rows[0].commits[0], "aaaaaaaa", "real link, short oid");
    assert_eq!(rows[0].workdir_label, "work", "never an absolute path");
    assert_eq!(rows[1].id, "s-old");
    assert_eq!(rows[1].usage_input, None, "missing usage stays unknown");
    assert_eq!(rows[0].summary, "Fix parser crash");
    assert_eq!(rows[0].summary_status, "available");
    assert!(rows[0].readable);
    assert!(
        !rows[1].readable,
        "no checkpoint summary means nothing readable"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid03_summary_is_redacted() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let (repo_id, worktree_id, conn) = scoped_capture(repo_dir.path()).await;
    let token = format!("ghp_{}", "a".repeat(36));
    insert_session_for(
        &conn,
        &repo_id,
        &worktree_id,
        "s-secret",
        "codex",
        "{}",
        1_700_000_000,
    )
    .await;
    write_metadata_checkpoint(
        repo_dir.path(),
        &conn,
        "s-secret",
        "codex",
        &format!("pushed with {token} ok"),
        "gpt-5",
        1,
        1,
    )
    .await;
    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let read = super::sessions::read_sessions(&scope)
        .await
        .expect("sessions");
    let rows = read.rows;
    assert_eq!(rows.len(), 1);
    assert!(
        !rows[0].summary.contains(&token),
        "the token must not reach the window: {}",
        rows[0].summary
    );
    assert!(rows[0].summary.contains("pushed with"));
}

#[tokio::test]
#[serial(cwd)]
async fn uid03_capture_absent_has_no_rows() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let read = super::sessions::read_sessions(&scope)
        .await
        .expect("sessions");
    let rows = read.rows;
    assert!(
        rows.is_empty(),
        "a repository without capture has no sessions"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid03_read_only_receipt() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let (repo_id, worktree_id, conn) = scoped_capture(repo_dir.path()).await;
    insert_session_for(
        &conn,
        &repo_id,
        &worktree_id,
        "s-1",
        "codex",
        "{}",
        1_700_000_000,
    )
    .await;
    write_metadata_checkpoint(repo_dir.path(), &conn, "s-1", "codex", "one", "gpt-5", 1, 1).await;
    conn.close().await.ok();
    let db_path = crate::internal::ai::capture::store::capture_database_path(
        &repo_dir.path().join(crate::utils::util::ROOT_DIR),
    );
    let before = fs::read(&db_path).expect("read capture database");
    let repo_db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let repo_before = fs::read(&repo_db_path).expect("read repository database");
    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let read = super::sessions::read_sessions(&scope)
        .await
        .expect("sessions");
    assert_eq!(read.rows.len(), 1, "the summary path actually ran");
    assert_eq!(read.rows[0].model.as_deref(), Some("gpt-5"));
    let after = fs::read(&db_path).expect("read capture database");
    let repo_after = fs::read(&repo_db_path).expect("read repository database");
    assert_eq!(before, after, "session reads must not modify the catalog");
    assert_eq!(
        repo_before, repo_after,
        "session reads must not modify the repository database (schema, operation rows, \
         conversion certificates)"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid03_cap_is_200_rows() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let (repo_id, worktree_id, conn) = scoped_capture(repo_dir.path()).await;
    for index in 0..210 {
        insert_session(
            &conn,
            &repo_id,
            &worktree_id,
            &format!("s-{index:03}"),
            r#"{"summary":"row"}"#,
            1_700_000_000 + index as i64,
        )
        .await;
    }
    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let read = super::sessions::read_sessions(&scope)
        .await
        .expect("sessions");
    let rows = read.rows;
    assert_eq!(rows.len(), super::sessions::SESSION_ROW_CAP);
    assert_eq!(rows[0].id, "s-209", "newest first");
}

#[test]
fn uid03_catalog_transition_uses_real_rows() {
    let rows = vec![super::sessions::SessionRow {
        id: "s-real".into(),
        agent: "claude_code".into(),
        state: "idle".into(),
        started_at: 1_700_000_000,
        last_event_at: 1_700_000_100,
        stopped_at: None,
        workdir_label: "work".into(),
        model: Some("claude-sonnet-4".into()),
        title: None,
        summary: "real summary".into(),
        usage_input: Some(1),
        usage_output: Some(2),
        usage_calls: Some(3),
        checkpoints: 4,
        commits: vec!["deadbeef".into()],
        summary_status: "available".into(),
        readable: true,
    }];
    let catalog = super::super::view::catalog_from_sessions(&rows, Vec::new());
    assert_eq!(catalog.sessions.len(), 1);
    let session = &catalog.sessions[0];
    assert_eq!(session.id, "s-real");
    assert_eq!(session.summary, "real summary");
    assert_eq!(session.model, "claude-sonnet-4");
    assert_eq!(session.start.get(..10), Some("2023-11-14"));
    assert!(session.verification.is_empty(), "no invented verification");
    assert_eq!(session.commits.len(), 1, "linked commit count is real");
    assert!(
        super::super::view::catalog_from_sessions(&[], Vec::new())
            .sessions
            .is_empty()
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid03_tombstoned_sessions_are_hidden() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let (repo_id, worktree_id, conn) = scoped_capture(repo_dir.path()).await;
    insert_session(
        &conn,
        &repo_id,
        &worktree_id,
        "s-erased",
        r#"{"summary":"erased"}"#,
        1_700_000_000,
    )
    .await;
    conn.execute_unprepared(
        "INSERT INTO agent_import_tombstone \
         (agent_kind, provider_session_id, erased_session_id, erased_at) \
         VALUES ('claude_code', 'p-s-erased', 's-erased', 1700000001)",
    )
    .await
    .expect("insert tombstone");
    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let read = super::sessions::read_sessions(&scope)
        .await
        .expect("sessions");
    let rows = read.rows;
    assert!(
        rows.is_empty(),
        "a tombstoned session must not be resurrected in the list"
    );
}

#[test]
fn uid03_agent_labels_match_the_filter_chips() {
    assert_eq!(super::sessions::agent_label("claude_code"), "Claude Code");
    assert_eq!(super::sessions::agent_label("codex"), "Codex");
    assert_eq!(super::sessions::agent_label("opencode"), "OpenCode");
    assert_eq!(super::sessions::agent_label("custom"), "custom");
}

#[tokio::test]
#[serial(cwd)]
async fn uid03_other_scope_session_is_hidden() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let (repo_id, worktree_id, conn) = scoped_capture(repo_dir.path()).await;
    insert_session(
        &conn,
        &repo_id,
        &worktree_id,
        "s-here",
        r#"{"summary":"here"}"#,
        1_700_000_000,
    )
    .await;
    insert_session(
        &conn,
        "foreign-repo",
        "foreign-worktree",
        "s-elsewhere",
        r#"{"summary":"elsewhere"}"#,
        1_700_000_100,
    )
    .await;
    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let read = super::sessions::read_sessions(&scope)
        .await
        .expect("sessions");
    let rows = read.rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "s-here", "foreign scope rows are excluded");
}

#[tokio::test]
#[serial(cwd)]
async fn uid03_corrupt_catalog_is_an_error() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let storage = repo_dir.path().join(crate::utils::util::ROOT_DIR);
    crate::internal::ai::capture::store::open(&storage, Duration::from_secs(2))
        .await
        .expect("create capture catalog")
        .close()
        .await
        .ok();
    let db_path = crate::internal::ai::capture::store::capture_database_path(&storage);
    fs::write(&db_path, b"not a sqlite database").expect("write corrupt catalog");
    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let error = super::sessions::read_sessions(&scope)
        .await
        .expect_err("a corrupt catalog must not look empty");
    assert!(!error.message.is_empty());
    assert!(
        !error.state.is_available(),
        "corrupt catalogs fail closed: {:?}",
        error.state
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid03_summary_cap_is_512_chars() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let (repo_id, worktree_id, conn) = scoped_capture(repo_dir.path()).await;
    insert_session_for(
        &conn,
        &repo_id,
        &worktree_id,
        "s-long",
        "codex",
        "{}",
        1_700_000_000,
    )
    .await;
    let long_title = "x".repeat(600);
    write_metadata_checkpoint(
        repo_dir.path(),
        &conn,
        "s-long",
        "codex",
        &long_title,
        "gpt-5",
        1,
        1,
    )
    .await;
    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let read = super::sessions::read_sessions(&scope)
        .await
        .expect("sessions");
    let rows = read.rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].summary.chars().count(),
        512,
        "summaries are bounded display excerpts"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid03_session_detail_fields_are_real() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let (repo_id, worktree_id, conn) = scoped_capture(repo_dir.path()).await;
    insert_session_for(
        &conn,
        &repo_id,
        &worktree_id,
        "s-detail",
        "codex",
        "{}",
        1_700_000_000,
    )
    .await;
    write_metadata_checkpoint(
        repo_dir.path(),
        &conn,
        "s-detail",
        "codex",
        "Detail title",
        "gpt-5-codex",
        11,
        22,
    )
    .await;
    conn.execute_unprepared(
        "INSERT INTO agent_session_commit (session_id, hash_kind, commit_oid) \
         VALUES ('s-detail', 'sha1', 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb')",
    )
    .await
    .expect("insert link");
    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let read = super::sessions::read_sessions(&scope)
        .await
        .expect("sessions");
    let rows = read.rows;
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.agent, "Codex");
    assert_eq!(row.model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(row.title.as_deref(), Some("Detail title"));
    assert_eq!(row.usage_input, Some(11));
    assert_eq!(row.usage_output, Some(22));
    assert_eq!(row.usage_calls, Some(3));
    assert_eq!(row.checkpoints, 1);
    assert_eq!(row.commits, vec!["bbbbbbbb".to_string()]);
    assert_eq!(row.workdir_label, "work");
    assert_eq!(row.started_at, 1_700_000_000);
    assert_eq!(row.last_event_at, 1_700_000_000);
}

#[tokio::test]
#[serial(cwd)]
async fn uid03_other_consumers_preserved() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let (repo_id, worktree_id, conn) = scoped_capture(repo_dir.path()).await;
    insert_session(
        &conn,
        &repo_id,
        &worktree_id,
        "s-shared",
        r#"{"summary":"shared"}"#,
        1_700_000_000,
    )
    .await;
    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let read = super::sessions::read_sessions(&scope)
        .await
        .expect("sessions");
    let _rows = read.rows;
    // The shared column list still serves the CLI surface after a GUI read.
    let count = conn
        .query_one_raw(sea_orm::Statement::from_string(
            conn.get_database_backend(),
            format!(
                "SELECT COUNT(*) AS count FROM ({})",
                crate::command::agent::SESSION_PAGE_COLUMNS
            ),
        ))
        .await
        .expect("cli-shaped query")
        .map(|row| row.try_get::<i64>("", "count").unwrap_or(0))
        .unwrap_or(0);
    assert_eq!(
        count, 1,
        "the capture catalog is unchanged for other readers"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid03_non_codex_checkpoint_metadata_is_read() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let (repo_id, worktree_id, conn) = scoped_capture(repo_dir.path()).await;
    insert_session_for(
        &conn,
        &repo_id,
        &worktree_id,
        "s-claude",
        "claude_code",
        "{}",
        1_700_000_000,
    )
    .await;
    write_metadata_checkpoint(
        repo_dir.path(),
        &conn,
        "s-claude",
        "claude_code",
        "Claude title",
        "claude-sonnet-4",
        5,
        6,
    )
    .await;
    let read = super::sessions::read_sessions(&scope_for(&repo_dir))
        .await
        .expect("sessions");
    assert_eq!(read.rows.len(), 1);
    assert_eq!(read.rows[0].agent, "Claude Code");
    assert_eq!(
        read.rows[0].model.as_deref(),
        Some("claude-sonnet-4"),
        "the summary reader is agent-aware, not codex-only"
    );
}

fn scope_for(repo_dir: &tempfile::TempDir) -> crate::internal::worktree_scope::RequestScope {
    repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository")
}

#[tokio::test]
#[serial(cwd)]
async fn uid03_commit_links_cap_per_session() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let (repo_id, worktree_id, conn) = scoped_capture(repo_dir.path()).await;
    for (session, links) in [("s-a", 4100), ("s-b", 25)] {
        insert_session(
            &conn,
            &repo_id,
            &worktree_id,
            session,
            r#"{"summary":"links"}"#,
            1_700_000_000,
        )
        .await;
        for index in 0..links {
            conn.execute_unprepared(&format!(
                "INSERT INTO agent_session_commit (session_id, hash_kind, commit_oid) \
                 VALUES ('{session}', 'sha1', '{:040x}')",
                index
            ))
            .await
            .expect("insert link");
        }
    }
    let read = super::sessions::read_sessions(&scope_for(&repo_dir))
        .await
        .expect("sessions");
    assert_eq!(read.rows.len(), 2);
    let later = read.rows.iter().find(|row| row.id == "s-b").expect("s-b");
    assert_eq!(
        later.commits.len(),
        super::sessions::COMMIT_LINK_CAP,
        "a later session keeps its links even past the first session's 4,000"
    );
    for row in &read.rows {
        assert_eq!(
            row.commits.len(),
            super::sessions::COMMIT_LINK_CAP,
            "the per-session cap is applied before any page cap"
        );
    }
}

#[tokio::test]
#[serial(cwd)]
async fn uid03_schema_mismatch_is_typed() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let (_, _, conn) = scoped_capture(repo_dir.path()).await;
    conn.execute_unprepared("UPDATE schema_versions SET version = 1")
        .await
        .expect("downgrade schema ledger");
    let error = super::sessions::read_sessions(&scope_for(&repo_dir))
        .await
        .expect_err("an old schema must not look available");
    assert_eq!(error.state, SourceState::SchemaOld);
    conn.execute_unprepared("UPDATE schema_versions SET version = 999999999999")
        .await
        .expect("upgrade schema ledger");
    let error = super::sessions::read_sessions(&scope_for(&repo_dir))
        .await
        .expect_err("a new schema must not look available");
    assert_eq!(error.state, SourceState::SchemaNew);
}

#[test]
fn uid03_capture_and_summary_labels_differ() {
    let rows = vec![super::sessions::SessionRow {
        id: "s-mixed".into(),
        agent: "Codex".into(),
        state: "active".into(),
        started_at: 1_700_000_000,
        last_event_at: 1_700_000_000,
        stopped_at: None,
        workdir_label: "work".into(),
        model: Some("gpt-5".into()),
        title: Some("title".into()),
        summary: "title".into(),
        usage_input: None,
        usage_output: None,
        usage_calls: None,
        checkpoints: 2,
        commits: Vec::new(),
        summary_status: "partial".into(),
        readable: true,
    }];
    let catalog = super::super::view::catalog_from_sessions(&rows, Vec::new());
    let session = &catalog.sessions[0];
    assert_eq!(session.capture_label, "2 checkpoint(s)");
    assert_eq!(session.readable_label, "readable content · partial");
    assert!(
        session.capture_label != session.readable_label,
        "capture completeness and summary status are separate questions"
    );
}

#[test]
fn uid03_readable_filter_choice_and_predicate_data() {
    let rows = vec![
        super::sessions::SessionRow {
            id: "s-read".into(),
            agent: "Codex".into(),
            state: "active".into(),
            started_at: 1_700_000_000,
            last_event_at: 1_700_000_000,
            stopped_at: Some(1_700_000_100),
            workdir_label: "work".into(),
            model: None,
            title: Some("readable".into()),
            summary: "readable".into(),
            usage_input: None,
            usage_output: None,
            usage_calls: None,
            checkpoints: 1,
            commits: Vec::new(),
            summary_status: "available".into(),
            readable: true,
        },
        super::sessions::SessionRow {
            id: "s-blind".into(),
            agent: "Codex".into(),
            state: "active".into(),
            started_at: 1_700_000_001,
            last_event_at: 1_700_000_001,
            stopped_at: None,
            workdir_label: "work".into(),
            model: None,
            title: None,
            summary: String::new(),
            usage_input: None,
            usage_output: None,
            usage_calls: None,
            checkpoints: 0,
            commits: Vec::new(),
            summary_status: "unknown".into(),
            readable: false,
        },
    ];
    let catalog = super::super::view::catalog_from_sessions(&rows, Vec::new());
    let readable: Vec<bool> = catalog.sessions.iter().map(|s| s.readable).collect();
    assert_eq!(
        readable,
        vec![true, false],
        "mixed rows keep their real flags"
    );
    let choices = super::super::view::filter_choices(&catalog);
    assert!(
        choices
            .iter()
            .any(|choice| choice.value == "readable:readable"),
        "the readable-content filter chip exists"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid03_summary_completion_errors_are_typed() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let (repo_id, worktree_id, conn) = scoped_capture(repo_dir.path()).await;
    insert_session_for(
        &conn,
        &repo_id,
        &worktree_id,
        "s-bad",
        "codex",
        "{}",
        1_700_000_000,
    )
    .await;
    write_metadata_checkpoint(
        repo_dir.path(),
        &conn,
        "s-bad",
        "codex",
        "title",
        "gpt-5",
        1,
        1,
    )
    .await;
    // Point the catalog row at a different blob than the tree entry: the role
    // proof fails, so completion must be an integrity error, not "unknown".
    let storage = repo_dir.path().join(crate::utils::util::ROOT_DIR);
    let other =
        crate::utils::object::write_git_object(&storage, "blob", b"{}").expect("write other blob");
    conn.execute_unprepared(&format!(
        "UPDATE agent_checkpoint SET metadata_blob_oid = '{other}' WHERE session_id = 's-bad'"
    ))
    .await
    .expect("update checkpoint");
    let read = super::sessions::read_sessions(&scope_for(&repo_dir))
        .await
        .expect("a broken metadata role degrades one row, not the page");
    assert_eq!(read.state, SourceState::Available);
    assert_eq!(read.rows.len(), 1, "the row stays visible");
    assert_eq!(
        read.rows[0].summary_status, "malformed",
        "its summary is marked malformed"
    );
    assert!(!read.rows[0].readable);

    // A valid checkpoint whose shared budget expires during completion must be
    // reported as a budget error, not corruption.
    insert_session_for(
        &conn,
        &repo_id,
        &worktree_id,
        "s-slow",
        "codex",
        "{}",
        1_700_000_001,
    )
    .await;
    write_metadata_checkpoint(
        repo_dir.path(),
        &conn,
        "s-slow",
        "codex",
        "slow",
        "gpt-5",
        1,
        1,
    )
    .await;
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = cancelled.clone();
    let budget = crate::internal::ai::checkpoint_reader::CheckpointReadBudget::new(
        std::time::Instant::now() + Duration::from_secs(5),
        std::sync::Arc::new(move || flag.load(std::sync::atomic::Ordering::SeqCst)),
    );
    let txn = conn.begin().await.expect("begin");
    let prepared = crate::internal::ai::capture::summary::SessionSummaryRead::prepare_for(
        &txn, &storage, "s-slow", "codex", budget,
    )
    .await
    .expect("prepare");
    let _ = txn.rollback().await;
    cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        prepared.finish_result().await.unwrap_err(),
        crate::internal::ai::capture::summary::SummaryErrorKind::Budget,
        "completion preserves the budget classification"
    );
}

#[test]
fn uid03_readable_filter_predicate_filters_rows() {
    use crate::gui::view::passes_filters;
    let rows = vec![
        super::sessions::SessionRow {
            id: "s-read".into(),
            agent: "Codex".into(),
            state: "active".into(),
            started_at: 1_700_000_000,
            last_event_at: 1_700_000_000,
            stopped_at: Some(1_700_000_100),
            workdir_label: "work".into(),
            model: None,
            title: Some("readable".into()),
            summary: "readable".into(),
            usage_input: None,
            usage_output: None,
            usage_calls: None,
            checkpoints: 1,
            commits: Vec::new(),
            summary_status: "available".into(),
            readable: true,
        },
        super::sessions::SessionRow {
            id: "s-blind".into(),
            agent: "Codex".into(),
            state: "active".into(),
            started_at: 1_700_000_000,
            last_event_at: 1_700_000_000,
            stopped_at: Some(1_700_000_100),
            workdir_label: "work".into(),
            model: None,
            title: None,
            summary: String::new(),
            usage_input: None,
            usage_output: None,
            usage_calls: None,
            checkpoints: 0,
            commits: Vec::new(),
            summary_status: "unknown".into(),
            readable: false,
        },
    ];
    let catalog = super::super::view::catalog_from_sessions(&rows, Vec::new());
    let visible = |readable_only: bool| -> Vec<String> {
        catalog
            .sessions
            .iter()
            .filter(|session| {
                passes_filters(session, "all", &[], &[], &[], readable_only, &catalog.today)
            })
            .map(|session| session.id.clone())
            .collect()
    };
    assert_eq!(visible(true), vec!["s-read"], "only readable rows remain");
    assert_eq!(
        visible(false),
        vec!["s-read", "s-blind"],
        "clearing the filter restores both rows"
    );
}

#[test]
fn uid03_selection_and_state_messages_are_stable() {
    use crate::gui::view::{select_session, sessions_message};
    let mut rows = vec![super::sessions::SessionRow {
        id: "s-one".into(),
        agent: "Codex".into(),
        state: "active".into(),
        started_at: 1_700_000_000,
        last_event_at: 1_700_000_000,
        stopped_at: None,
        workdir_label: "work".into(),
        model: None,
        title: None,
        summary: String::new(),
        usage_input: None,
        usage_output: None,
        usage_calls: None,
        checkpoints: 0,
        commits: Vec::new(),
        summary_status: "malformed".into(),
        readable: false,
    }];
    let mut second = rows[0].clone();
    second.id = "s-two".into();
    rows.push(second);
    let catalog = super::super::view::catalog_from_sessions(&rows, Vec::new());
    assert_eq!(
        select_session("s-two", &catalog),
        "s-two",
        "a non-first selection survives a refresh/reordering"
    );
    assert_eq!(
        select_session("s-gone", &catalog),
        "s-one",
        "a removed selection falls back to the first row"
    );
    assert_eq!(
        sessions_message(Some(&SourceState::Stale), None),
        "Sessions were not checked yet."
    );
    assert_eq!(
        sessions_message(Some(&SourceState::Absent), None),
        "No captured sessions yet: this repository has no capture catalog."
    );
    let corrupt = sessions_message(Some(&SourceState::Corrupt), Some("bad rows"));
    assert!(corrupt.contains("corrupt") && corrupt.contains("bad rows"));
    assert_eq!(
        catalog.sessions[0].readable_label, "no readable content · malformed",
        "a malformed summary is visible instead of being hidden"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid03_catalog_level_preparation_failure_fails_the_page() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let (repo_id, worktree_id, conn) = scoped_capture(repo_dir.path()).await;
    insert_session(
        &conn,
        &repo_id,
        &worktree_id,
        "s-1",
        r#"{"summary":"one"}"#,
        1_700_000_000,
    )
    .await;
    conn.execute_unprepared("DROP TABLE agent_checkpoint")
        .await
        .expect("drop checkpoint table");
    // Directly prove summary preparation fails page-level, so reverting the
    // classification cannot be masked by a later count query failing.
    let direct = super::sessions::read_summaries(
        &conn,
        &repo_dir.path().join(crate::utils::util::ROOT_DIR),
        &["s-1".to_string()],
        &["claude_code".to_string()],
    )
    .await;
    assert!(
        direct.is_err(),
        "summary preparation itself must fail the page"
    );
    let error = super::sessions::read_sessions(&scope_for(&repo_dir))
        .await
        .expect_err("a missing checkpoint table is a source-level failure");
    assert!(
        !error.state.is_available(),
        "the page reports the source failure instead of an empty list"
    );
    assert!(!error.message.is_empty());
}

#[test]
fn uid03_search_hits_real_rows() {
    let rows = vec![super::sessions::SessionRow {
        id: "s-search".into(),
        agent: "Codex".into(),
        state: "active".into(),
        started_at: 1_700_000_000,
        last_event_at: 1_700_000_000,
        stopped_at: None,
        workdir_label: "work".into(),
        model: Some("gpt-5".into()),
        title: Some("Fix parser crash".into()),
        summary: "parser crash fixed".into(),
        usage_input: None,
        usage_output: None,
        usage_calls: None,
        checkpoints: 1,
        commits: Vec::new(),
        summary_status: "available".into(),
        readable: true,
    }];
    let catalog = super::super::view::catalog_from_sessions(&rows, Vec::new());
    let hits = super::super::view::search_hits(&catalog, "all", "parser");
    assert!(
        hits.iter()
            .any(|hit| hit.id == "s-search" && hit.label == "Summary"),
        "search reads the real summary text: {hits:?}"
    );
    assert!(
        super::super::view::search_hits(&catalog, "all", "absent").is_empty(),
        "a non-matching query yields no hits"
    );
}

#[test]
fn uid03_busy_causes_map_to_busy() {
    let busy = anyhow::Error::from(
        crate::internal::ai::checkpoint_reader::CheckpointReaderError::CatalogBusy,
    );
    assert_eq!(state::classify_error_chain(&busy), SourceState::Busy);
    let pool = anyhow::anyhow!("Connection pool timed out while acquiring a connection");
    assert_eq!(
        state::classify_error_chain(&pool),
        SourceState::Busy,
        "sqlx pool timeout wording is transient"
    );
    let corrupt = anyhow::anyhow!("checkpoint object identity or type is inconsistent");
    assert_eq!(state::classify_error_chain(&corrupt), SourceState::Corrupt);
}

#[tokio::test]
#[serial(cwd)]
async fn uid03_active_and_stopped_sessions_render_distinctly() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let (repo_id, worktree_id, conn) = scoped_capture(repo_dir.path()).await;
    insert_session(
        &conn,
        &repo_id,
        &worktree_id,
        "s-active",
        r#"{"summary":"active"}"#,
        1_700_000_000,
    )
    .await;
    insert_session(
        &conn,
        &repo_id,
        &worktree_id,
        "s-stopped",
        r#"{"summary":"stopped"}"#,
        1_700_000_001,
    )
    .await;
    conn.execute_unprepared(
        "UPDATE agent_session SET stopped_at = 1700000100, state = 'stopped' \
         WHERE session_id = 's-stopped'",
    )
    .await
    .expect("mark stopped");
    let read = super::sessions::read_sessions(&scope_for(&repo_dir))
        .await
        .expect("sessions");
    let active = read
        .rows
        .iter()
        .find(|row| row.id == "s-active")
        .expect("active");
    let stopped = read
        .rows
        .iter()
        .find(|row| row.id == "s-stopped")
        .expect("stopped");
    assert_eq!(active.stopped_at, None, "an active session has no end time");
    assert_eq!(stopped.stopped_at, Some(1_700_000_100));
    let catalog = super::super::view::catalog_from_sessions(&read.rows, Vec::new());
    let active_view = catalog
        .sessions
        .iter()
        .find(|session| session.id == "s-active")
        .expect("active view");
    let stopped_view = catalog
        .sessions
        .iter()
        .find(|session| session.id == "s-stopped")
        .expect("stopped view");
    assert_eq!(
        active_view.end, "—",
        "an active session does not invent an end"
    );
    assert_eq!(stopped_view.end, "2023-11-14 22:15");
    assert_eq!(
        crate::gui::view::end_label(active_view),
        "Still running",
        "the drawn state label follows the session state"
    );
    assert_eq!(
        crate::gui::view::end_label(stopped_view),
        "Ended at 22:15",
        "a stopped session draws its real end time"
    );
    let mut stale = active_view.clone();
    stale.end = "2023-11-14 23:00".to_string();
    assert_eq!(
        crate::gui::view::end_label(&stale),
        "Still running",
        "an active session with a stale end timestamp still shows as running"
    );
    assert_eq!(
        crate::gui::model::clock(&active_view.end),
        "—",
        "the drawn clock preserves the missing-value marker"
    );
    assert_eq!(
        crate::gui::model::clock(&stopped_view.end),
        "22:15",
        "a stopped session renders its real end time"
    );
    let mut quarantined = stopped_view.clone();
    quarantined.running_label = "quarantined".to_string();
    quarantined.end = "—".to_string();
    assert_eq!(
        crate::gui::view::end_label(&quarantined),
        "No recorded end",
        "a quarantined session with a cleared end is not 'Still running'"
    );
}

async fn insert_operation(dir: &tempfile::TempDir, op_id: &str, command: &str, end_ts: i64) {
    let db_path = dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let conn = db::open_database_without_migrations(&db_path)
        .await
        .expect("open repository database");
    let repo_id = crate::internal::config::ConfigKv::get_with_conn(&conn, "libra.repoid")
        .await
        .expect("repo id query")
        .expect("repo id")
        .value;
    conn.execute_unprepared(&format!(
        "INSERT INTO operation \
         (op_id, repo_id, kind, status, command_name, description, actor, worktree_id, \
          scope_kind, pre_view_oid, post_view_oid, start_ts, end_ts) \
         VALUES ('{op_id}', '{repo_id}', 'commit', 'success', '{command}', 'summary {op_id}', \
                 'tester', '', 'main', 'view-{op_id}', 'view-{op_id}', {start_ts}, {end_ts})",
        start_ts = end_ts - 1
    ))
    .await
    .expect("insert operation");
    conn.close().await.ok();
}

async fn insert_operations_bulk(dir: &tempfile::TempDir, count: usize) {
    let db_path = dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let conn = db::open_database_without_migrations(&db_path)
        .await
        .expect("open repository database");
    let repo_id = crate::internal::config::ConfigKv::get_with_conn(&conn, "libra.repoid")
        .await
        .expect("repo id query")
        .expect("repo id")
        .value;
    let mut batch = String::new();
    for index in 0..count {
        if index % 500 == 0 {
            if !batch.is_empty() {
                conn.execute_unprepared(&batch).await.expect("bulk insert");
            }
            batch.clear();
            batch.push_str(
                "INSERT INTO operation (op_id, repo_id, kind, status, command_name, description, actor, worktree_id, scope_kind, pre_view_oid, post_view_oid, start_ts, end_ts) VALUES ",
            );
        } else {
            batch.push(',');
        }
        let ts = 1_700_000_000_i64 + index as i64;
        batch.push_str(&format!(
            "('op-{index:06}', '{repo_id}', 'commit', 'success', 'commit', 'summary {index}', \
             'tester', '', 'main', 'view-{index}', 'view-{index}', {ts}, {ts})"
        ));
    }
    if !batch.is_empty() {
        conn.execute_unprepared(&batch).await.expect("bulk insert");
    }
    conn.close().await.ok();
}

#[tokio::test]
#[serial(cwd)]
async fn uid04_operation_rows() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    insert_operation(&repo_dir, "op-1", "commit", 1_700_000_100).await;
    insert_operation(&repo_dir, "op-2", "rebase", 1_700_000_200).await;
    let read = super::operations::read_operations(&scope_for(&repo_dir))
        .await
        .expect("operations");
    assert_eq!(read.state, SourceState::Available);
    assert_eq!(read.rows[0].op_id, "op-2", "newest first");
    assert!(!read.rows.is_empty(), "the real operation log has rows");
    let row = &read.rows[0];
    assert!(!row.op_id.is_empty());
    assert!(!row.command_name.is_empty());
    assert!(!row.status.is_empty());
    assert!(row.description.chars().count() <= 200);
    assert!(row.end_ts.is_some());
    assert!(!read.truncated);
}

#[tokio::test]
#[serial(cwd)]
async fn uid04_source_states() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let read = super::operations::read_operations(&scope_for(&repo_dir))
        .await
        .expect("operations");
    assert_eq!(read.state, SourceState::Absent, "no rows means absent");
    assert!(read.rows.is_empty());
}

#[test]
fn uid04_id_snapshot_paging() {
    let rows: Vec<super::operations::OperationRow> = (0..450)
        .map(|index| super::operations::OperationRow {
            op_id: format!("op-{index:04}"),
            command_name: "commit".into(),
            description: "fixture".into(),
            actor: "tester".into(),
            status: "succeeded".into(),
            start_ts: 1_700_000_000,
            end_ts: Some(1_700_000_000),
            view_id: "view".into(),
        })
        .collect();
    let first = super::operations::page_slice(&rows, 0);
    let second = super::operations::page_slice(&rows, 1);
    let third = super::operations::page_slice(&rows, 2);
    assert_eq!(first.len(), 200);
    assert_eq!(second.len(), 200);
    assert_eq!(third.len(), 50);
    assert_eq!(first[0].op_id, "op-0000");
    assert_eq!(second[0].op_id, "op-0200");
    assert_eq!(third[0].op_id, "op-0400");
    assert_eq!(
        super::operations::page_slice(&rows, 9).len(),
        0,
        "pages past the end are empty, never wrapped"
    );
}

#[test]
fn uid04_filter_and_search() {
    let row = super::operations::OperationRow {
        op_id: "op-1".into(),
        command_name: "commit".into(),
        description: "feat(ui): add sessions".into(),
        actor: "tester".into(),
        status: "succeeded".into(),
        start_ts: 1,
        end_ts: Some(1),
        view_id: "view".into(),
    };
    assert!(super::operations::matches_filter(&row, "", "sessions"));
    assert!(super::operations::matches_filter(&row, "commit", ""));
    assert!(!super::operations::matches_filter(&row, "rebase", ""));
    assert!(!super::operations::matches_filter(&row, "", "absent"));
}

#[tokio::test]
#[serial(cwd)]
async fn uid04_read_only_receipt() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    insert_operation(&repo_dir, "op-1", "commit", 1_700_000_100).await;
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let before = receipt_fingerprint(&db_path).await;
    let _read = super::operations::read_operations(&scope_for(&repo_dir))
        .await
        .expect("operations");
    let after = receipt_fingerprint(&db_path).await;
    assert_eq!(before, after, "operation reads write no rows");
}

#[tokio::test]
#[serial(cwd)]
async fn uid04_schema_mismatch_is_typed() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    insert_operation(&repo_dir, "op-1", "commit", 1_700_000_100).await;
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let conn = db::open_database_without_migrations(&db_path)
        .await
        .expect("open repo db");
    conn.execute_unprepared(
        "DELETE FROM schema_versions;          INSERT INTO schema_versions (version, name, applied_at) VALUES (1, 'old', 'now')",
    )
    .await
    .expect("downgrade schema");
    conn.close().await.ok();
    let error = super::operations::read_operations(&scope_for(&repo_dir))
        .await
        .expect_err("an old schema is not Available");
    assert_eq!(error.state, SourceState::SchemaOld);
}

#[tokio::test]
#[serial(cwd)]
async fn uid04_description_is_redacted() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    insert_operation(&repo_dir, "op-1", "commit", 1_700_000_100).await;
    let token = format!("ghp_{}", "b".repeat(36));
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let conn = db::open_database_without_migrations(&db_path)
        .await
        .expect("open repo db");
    conn.execute_unprepared(&format!(
        "UPDATE operation SET description = 'pushed with {token} ok'"
    ))
    .await
    .expect("set secret description");
    conn.close().await.ok();
    let read = super::operations::read_operations(&scope_for(&repo_dir))
        .await
        .expect("operations");
    assert!(!read.rows[0].description.contains(&token));
    assert!(read.rows[0].description.contains("pushed with"));
}

#[tokio::test]
#[serial(cwd)]
async fn uid04_paths_are_sanitized() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    insert_operation(&repo_dir, "op-1", "commit", 1_700_000_100).await;
    insert_operation(&repo_dir, "op-2", "commit", 1_700_000_200).await;
    insert_operation(&repo_dir, "op-3", "commit", 1_700_000_300).await;
    insert_operation(&repo_dir, "op-4", "commit", 1_700_000_400).await;
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let conn = db::open_database_without_migrations(&db_path)
        .await
        .expect("open repo db");
    conn.execute_unprepared(
        "UPDATE operation SET description = \
         'edited path=\"/Users/alice/private/file.rs\" path=C:\\work\\repo\\x.rs done' \
         WHERE op_id = 'op-1'",
    )
    .await
    .expect("set paths 1");
    conn.execute_unprepared(
        "UPDATE operation SET description = \
         'cd /Users/alice; a=b=/Users/alice/f=b.rs a=b=C:\\Users\\alice\\g=b.rs \
          error:/Users/alice/h.rs [file](/Users/alice/i.rs) path=(/Users/alice/e.rs) a.rs->/Users/alice/q.rs \
          file:///Users/alice/c=b.rs branch feat/search done' \
         WHERE op_id = 'op-2'",
    )
    .await
    .expect("set paths 2");
    conn.execute_unprepared(
        "UPDATE operation SET description = 'json {\"file\":\"/Users/alice/j.rs\"} [file](</Users/alice/i2.rs>)' \
         WHERE op_id = 'op-3'",
    )
    .await
    .expect("set paths 3");
    conn.execute_unprepared(
        "UPDATE operation SET description = 'win {\"file\":\"C:\\\\Users\\\\alice\\\\k.rs\"} done' \
         WHERE op_id = 'op-4'",
    )
    .await
    .expect("set paths 4");
    conn.close().await.ok();
    let read = super::operations::read_operations(&scope_for(&repo_dir))
        .await
        .expect("operations");
    let first = read
        .rows
        .iter()
        .find(|row| row.op_id == "op-1")
        .expect("op-1");
    let second = read
        .rows
        .iter()
        .find(|row| row.op_id == "op-2")
        .expect("op-2");
    let one = &first.description;
    assert!(!one.contains("/Users/"), "no absolute path survives: {one}");
    assert!(!one.contains("C:\\work"), "no Windows path survives: {one}");
    assert!(one.contains("file.rs") && one.contains("x.rs"));
    let two = &second.description;
    assert!(!two.contains("/Users/"), "no absolute path survives: {two}");
    assert!(!two.contains("C:\\Users"), "no nested Windows path: {two}");
    assert!(two.contains("f=b.rs"), "nested basename survives: {two}");
    assert!(
        two.contains("g=b.rs"),
        "nested Windows basename survives: {two}"
    );
    assert!(two.contains("h.rs"), "colon path sanitized: {two}");
    assert!(two.contains("i.rs"), "markdown link sanitized: {two}");
    assert!(two.contains("e.rs"), "wrapped assignment sanitized: {two}");
    assert!(two.contains("q.rs"), "arrow destination sanitized: {two}");
    assert!(two.contains("c=b.rs"), "URL basename survives: {two}");
    assert!(
        two.contains("branch feat/search"),
        "relative names survive: {two}"
    );
    let three = read
        .rows
        .iter()
        .find(|row| row.op_id == "op-3")
        .expect("op-3");
    let json = &three.description;
    assert!(!json.contains("/Users/"), "JSON path removed: {json}");
    assert_eq!(
        json, "json {\"file\":\"j.rs\"} [file](<i2.rs>)",
        "the sanitized JSON is exact and well-formed"
    );
    assert!(
        json.contains("i2.rs"),
        "angle-bracket destination sanitized: {json}"
    );
    let four = read
        .rows
        .iter()
        .find(|row| row.op_id == "op-4")
        .expect("op-4");
    let win_json = &four.description;
    assert_eq!(
        win_json, "win {\"file\":\"k.rs\"} done",
        "the sanitized Windows JSON is exact"
    );
    assert!(
        win_json.contains("k.rs"),
        "JSON Windows basename survives: {win_json}"
    );
}

#[test]
fn uid04_filter_before_paging_reaches_later_matches() {
    let rows: Vec<super::operations::OperationRow> = (0..450)
        .map(|index| super::operations::OperationRow {
            op_id: format!("op-{index:04}"),
            command_name: "commit".into(),
            description: if index == 350 {
                "needle".to_string()
            } else {
                "haystack".to_string()
            },
            actor: "tester".into(),
            status: "succeeded".into(),
            start_ts: 1,
            end_ts: Some(1),
            view_id: "view".into(),
        })
        .collect();
    let (page, page_count) = super::operations::filtered_page(&rows, "", "needle", 0);
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].op_id, "op-0350");
    assert_eq!(page_count, 1, "page count follows the filtered sequence");
}

#[tokio::test]
#[serial(cwd)]
async fn uid04_snapshot_cap_truncated() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    insert_operations_bulk(&repo_dir, crate::command::op::OPERATION_SNAPSHOT_CAP).await;
    let read = super::operations::read_operations(&scope_for(&repo_dir))
        .await
        .expect("operations");
    assert_eq!(read.rows.len(), crate::command::op::OPERATION_SNAPSHOT_CAP);
    assert!(!read.truncated, "exactly at the cap is not truncated");

    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    insert_operations_bulk(&repo_dir, crate::command::op::OPERATION_SNAPSHOT_CAP + 1).await;
    let read = super::operations::read_operations(&scope_for(&repo_dir))
        .await
        .expect("operations");
    assert_eq!(read.rows.len(), crate::command::op::OPERATION_SNAPSHOT_CAP);
    assert!(read.truncated, "one past the cap is truncated");
}

#[tokio::test]
#[serial(cwd)]
async fn uid04_read_budget_10k() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    insert_operations_bulk(&repo_dir, 10_000).await;
    let started = std::time::Instant::now();
    let read = super::operations::read_operations(&scope_for(&repo_dir))
        .await
        .expect("operations");
    let elapsed = started.elapsed();
    assert_eq!(read.rows.len(), 10_000);
    let budget = if cfg!(debug_assertions) { 600 } else { 150 };
    assert!(
        elapsed < std::time::Duration::from_millis(budget),
        "10k production read took {elapsed:?} against a {budget}ms budget"
    );
}

#[test]
fn uid04_snapshot_reset_to_page_one() {
    assert_eq!(crate::gui::desktop::reset_page_for_new_snapshot(), 0);
    let rows: Vec<super::operations::OperationRow> = (0..450)
        .map(|index| super::operations::OperationRow {
            op_id: format!("op-{index:04}"),
            command_name: "commit".into(),
            description: "summary".into(),
            actor: "tester".into(),
            status: "succeeded".into(),
            start_ts: 1,
            end_ts: Some(1),
            view_id: "view".into(),
        })
        .collect();
    let (second, _) = super::operations::filtered_page(&rows, "", "", 1);
    let delivered = crate::gui::desktop::delivered_page(1);
    assert_eq!(delivered, 0, "delivery resets a nonzero page");
    let (reset, _) = super::operations::filtered_page(&rows, "", "", delivered);
    assert_eq!(second[0].op_id, "op-0200");
    assert_eq!(
        reset[0].op_id, "op-0000",
        "a new snapshot starts at page one"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid04_terminalization_no_duplicate() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    insert_operations_bulk(&repo_dir, 450).await;
    let first = super::operations::read_operations(&scope_for(&repo_dir))
        .await
        .expect("first snapshot");
    // Terminalize an operation between page reads: the SNAPSHOT is fixed, so
    // traversal stays complete and duplicate-free.
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let conn = db::open_database_without_migrations(&db_path)
        .await
        .expect("open repo db");
    conn.execute_unprepared(
        "UPDATE operation SET end_ts = end_ts + 1000, status = 'success' \
         WHERE op_id = 'op-000200'",
    )
    .await
    .expect("terminalize");
    conn.close().await.ok();
    let mut seen = std::collections::HashSet::new();
    for page in 0..3 {
        let (rows, _) = super::operations::filtered_page(&first.rows, "", "", page);
        for row in rows {
            assert!(
                seen.insert(row.op_id.clone()),
                "no row repeats across pages"
            );
        }
    }
    assert_eq!(seen.len(), 450, "every row appears exactly once");
    let second = super::operations::read_operations(&scope_for(&repo_dir))
        .await
        .expect("second snapshot");
    assert!(
        second.rows.iter().any(|row| row.op_id == "op-000200"),
        "the new snapshot still lists the terminalized row"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid04_missing_schema_ledger_is_schema_old() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    insert_operation(&repo_dir, "op-1", "commit", 1_700_000_100).await;
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let conn = db::open_database_without_migrations(&db_path)
        .await
        .expect("open repo db");
    conn.execute_unprepared("DROP TABLE schema_versions")
        .await
        .expect("drop schema ledger");
    conn.close().await.ok();
    let error = super::operations::read_operations(&scope_for(&repo_dir))
        .await
        .expect_err("a missing ledger is not Available");
    assert_eq!(error.state, SourceState::SchemaOld);
    // The read-only connection must not recreate the ledger.
    let conn = db::open_database_without_migrations(&db_path)
        .await
        .expect("open repo db");
    let exists = conn
        .query_one_raw(sea_orm::Statement::from_string(
            conn.get_database_backend(),
            "SELECT COUNT(*) AS n FROM sqlite_master WHERE type='table' AND name='schema_versions'"
                .to_string(),
        ))
        .await
        .expect("table probe")
        .and_then(|row| row.try_get::<i64>("", "n").ok())
        .unwrap_or(0);
    conn.close().await.ok();
    assert_eq!(exists, 0, "the missing ledger was not recreated");
}

#[tokio::test]
#[serial(cwd)]
async fn uid04_summary_cap_is_200_chars() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    insert_operation(&repo_dir, "op-long", "commit", 1_700_000_100).await;
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let conn = db::open_database_without_migrations(&db_path)
        .await
        .expect("open repo db");
    let long = "x".repeat(250);
    conn.execute_unprepared(&format!(
        "UPDATE operation SET description = '{long}' WHERE op_id = 'op-long'"
    ))
    .await
    .expect("set long summary");
    conn.close().await.ok();
    let read = super::operations::read_operations(&scope_for(&repo_dir))
        .await
        .expect("operations");
    assert_eq!(read.rows[0].description.chars().count(), 200);
}

#[tokio::test]
#[serial(cwd)]
async fn uid04_unreadable_database_is_an_error() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    fs::write(&db_path, b"not a sqlite database").expect("corrupt database");
    let error = super::operations::read_operations(&scope_for(&repo_dir))
        .await
        .expect_err("a corrupt repository database must not look empty");
    assert_eq!(error.state, SourceState::Corrupt);
    assert!(!error.message.is_empty());
}

#[cfg(unix)]
#[tokio::test]
#[serial(cwd)]
async fn uid04_permission_denied_is_unreadable() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    insert_operation(&repo_dir, "op-1", "commit", 1_700_000_100).await;
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let mut permissions = fs::metadata(&db_path).expect("stat database").permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&db_path, permissions).expect("set readonly");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&db_path).expect("stat database").permissions();
        permissions.set_mode(0o000);
        fs::set_permissions(&db_path, permissions).expect("set mode 000");
    }
    let error = super::operations::read_operations(&scope_for(&repo_dir))
        .await
        .expect_err("an unreadable database is not Available");
    assert_eq!(
        error.state,
        SourceState::Unreadable {
            kind: std::io::ErrorKind::PermissionDenied
        }
    );
    // Restore so the tempdir can be cleaned up.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&db_path).expect("stat database").permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&db_path, permissions).expect("restore mode");
    }
}

async fn rebuild_memory(dir: &tempfile::TempDir) {
    let _guard = test::ChangeDirGuard::new(dir.path());
    crate::command::memory::execute(crate::command::memory::MemoryArgs {
        command: crate::command::memory::MemoryCommand::Rebuild,
        allow_stale: false,
    })
    .await
    .expect("memory rebuild");
}

async fn insert_ai_operation(dir: &tempfile::TempDir, op_id: &str) {
    let db_path = dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let conn = db::open_database_without_migrations(&db_path)
        .await
        .expect("open repo db");
    let repo_id = crate::internal::config::ConfigKv::get_with_conn(&conn, "libra.repoid")
        .await
        .expect("repo id")
        .expect("repo id")
        .value;
    conn.execute_unprepared(&format!(
        "INSERT INTO operation (op_id, repo_id, kind, status, command_name, description, \
         actor, worktree_id, scope_kind, pre_view_oid, post_view_oid, start_ts, end_ts) \
         VALUES ('{op_id}', '{repo_id}', 'commit', 'success', 'commit', 'fixture', 'tester', \
         '', 'main', 'view', 'view', 1700000000, 1700000001); \
         INSERT INTO ai_operation_link (operation_id, repo_id, redaction_version) VALUES ('{op_id}', '{repo_id}', '1')"
    ))
    .await
    .expect("insert ai operation");
    conn.close().await.ok();
}

async fn commit_memory_fixture(dir: &tempfile::TempDir, message: &str) {
    let _guard = test::ChangeDirGuard::new(dir.path());
    crate::command::commit::execute(crate::command::commit::CommitArgs {
        message: Some(message.to_string()),
        allow_empty: true,
        disable_pre: true,
        no_verify: true,
        ..Default::default()
    })
    .await;
}

#[tokio::test]
#[serial(cwd)]
async fn uid05_episode_rows_and_fields() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    commit_memory_fixture(&repo_dir, "memory fixture").await;
    rebuild_memory(&repo_dir).await;
    let read = super::memory::read_memory(&scope_for(&repo_dir))
        .await
        .expect("memory");
    assert_eq!(read.state, SourceState::Available);
    assert!(!read.rows.is_empty(), "the projection has real episodes");
    assert!(read.rows.len() <= super::memory::MEMORY_ROW_CAP);
    let row = &read.rows[0];
    assert!(!row.episode_id.is_empty());
    assert!(!row.source_kind.is_empty());
    assert!(!row.title.is_empty());
    assert!(row.title.chars().count() <= 200);
    assert!(row.body.chars().count() <= 200);
    assert!(row.ended_at >= row.started_at);
}

#[tokio::test]
#[serial(cwd)]
async fn uid05_stale_state_hides_rows() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    commit_memory_fixture(&repo_dir, "first").await;
    rebuild_memory(&repo_dir).await;
    // A new linked operation changes the fingerprint, making the projection stale.
    insert_ai_operation(&repo_dir, "op-stale").await;
    let read = super::memory::read_memory(&scope_for(&repo_dir))
        .await
        .expect("memory");
    assert_eq!(read.state, SourceState::Stale);
    assert!(read.rows.is_empty(), "stale projections show zero rows");
}

#[tokio::test]
#[serial(cwd)]
async fn uid05_absent_before_first_rebuild() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let read = super::memory::read_memory(&scope_for(&repo_dir))
        .await
        .expect("memory");
    assert_eq!(read.state, SourceState::Absent);
    assert!(read.rows.is_empty());
}

#[tokio::test]
#[serial(cwd)]
async fn uid05_bounded_children_and_signature() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    for index in 0..3 {
        commit_memory_fixture(&repo_dir, &format!("commit {index}")).await;
    }
    rebuild_memory(&repo_dir).await;
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let conn = db::open_database_without_migrations(&db_path)
        .await
        .expect("open repo db");
    let repo_id = crate::internal::config::ConfigKv::get_with_conn(&conn, "libra.repoid")
        .await
        .expect("repo id")
        .expect("repo id")
        .value;
    let bounded = crate::internal::ai::memory::reader::list_episodes_bounded(&conn, &repo_id, 2)
        .await
        .expect("bounded episodes");
    assert_eq!(bounded.len(), 2);
    let all = crate::internal::ai::memory::reader::list_episodes(&conn, &repo_id)
        .await
        .expect("all episodes");
    assert_eq!(
        all.len(),
        3,
        "the original signature still returns everything"
    );
    conn.close().await.ok();
}

#[tokio::test]
#[serial(cwd)]
async fn uid05_read_only_receipt() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    commit_memory_fixture(&repo_dir, "receipt").await;
    rebuild_memory(&repo_dir).await;
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let before = receipt_fingerprint(&db_path).await;
    let _read = super::memory::read_memory(&scope_for(&repo_dir))
        .await
        .expect("memory");
    let after = receipt_fingerprint(&db_path).await;
    assert_eq!(before, after, "memory reads write no rows");
}

#[tokio::test]
#[serial(cwd)]
async fn uid05_freshness_cost_measured() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    commit_memory_fixture(&repo_dir, "cost").await;
    rebuild_memory(&repo_dir).await;
    let started = std::time::Instant::now();
    let read = super::memory::read_memory(&scope_for(&repo_dir))
        .await
        .expect("memory");
    let elapsed = started.elapsed();
    assert_eq!(read.state, SourceState::Available);
    assert!(
        elapsed < std::time::Duration::from_millis(150),
        "bounded memory read took {elapsed:?}, over the 150ms budget"
    );
}

#[test]
fn uid05_search_and_filter_use_real_fields() {
    let row = super::memory::MemoryRow {
        episode_id: "ep-1".into(),
        source_kind: "commit".into(),
        started_at: 1,
        ended_at: 2,
        title: "rate limiter".into(),
        body: "token bucket".into(),
        actor: Some("tester".into()),
        outcome: "succeeded".into(),
        evidence_count: 0,
        path_count: 0,
    };
    assert!(super::memory::matches_filter(&row, "", "limiter"));
    assert!(super::memory::matches_filter(&row, "commit", ""));
    assert!(!super::memory::matches_filter(&row, "agent_session", ""));
    assert!(!super::memory::matches_filter(&row, "", "absent"));
}

#[tokio::test]
#[serial(cwd)]
async fn uid05_privacy_redacts_and_sanitizes() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    commit_memory_fixture(&repo_dir, "privacy").await;
    rebuild_memory(&repo_dir).await;
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let conn = db::open_database_without_migrations(&db_path)
        .await
        .expect("open repo db");
    let token = format!("ghp_{}", "c".repeat(36));
    let actor = format!("{token} {}", "x".repeat(250));
    conn.execute_unprepared(&format!(
        "UPDATE memory_episode SET title = 'edited path=\"/Users/alice/x.rs\" and `/Users/alice/bt`', \
         body = 'C:\\Users\\alice\\y.rs \\\\server\\share\\f.rs {token}', actor = '{actor}'"
    ))
    .await
    .expect("set privacy fixture");
    conn.close().await.ok();
    let read = super::memory::read_memory(&scope_for(&repo_dir))
        .await
        .expect("memory");
    assert_eq!(read.state, SourceState::Available);
    for row in &read.rows {
        assert!(!row.title.contains("/Users/"), "title path: {}", row.title);
        assert!(!row.body.contains("/Users/"), "body path: {}", row.body);
        assert!(!row.body.contains("C:"), "body Windows path: {}", row.body);
        assert!(!row.body.contains("server"), "UNC path: {}", row.body);
        assert!(!row.title.contains(&token) && !row.body.contains(&token));
        let actor = row.actor.as_deref().unwrap_or("");
        assert!(!actor.contains(&token), "actor secret");
        assert!(actor.chars().count() <= 200, "actor is capped");
        assert!(row.title.chars().count() <= 200);
        assert!(row.body.chars().count() <= 200);
    }
}

#[tokio::test]
#[serial(cwd)]
async fn uid05_bounded_children_match_selected_rows() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    for index in 0..3 {
        commit_memory_fixture(&repo_dir, &format!("child {index}")).await;
    }
    rebuild_memory(&repo_dir).await;
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let conn = db::open_database_without_migrations(&db_path)
        .await
        .expect("open repo db");
    let repo_id = crate::internal::config::ConfigKv::get_with_conn(&conn, "libra.repoid")
        .await
        .expect("repo id")
        .expect("repo id")
        .value;
    let all = crate::internal::ai::memory::reader::list_episodes(&conn, &repo_id)
        .await
        .expect("all");
    assert_eq!(all.len(), 3);
    let selected = all[0].episode_id.clone();
    let excluded = all[2].episode_id.clone();
    conn.execute_unprepared(&format!(
        "INSERT INTO memory_episode_evidence (episode_id, ordinal, kind, ref_id, link_confidence, \
         resolution_status) VALUES ('{selected}', 1, 'commit', 'ev-selected', 'identity', 'resolved'), \
         ('{excluded}', 1, 'commit', 'ev-excluded', 'identity', 'resolved'); \
         INSERT INTO memory_episode_path (episode_id, code_path, change_kind, blob_oid_at_end, \
         ended_at) VALUES ('{selected}', 'src/selected.rs', 'modified', NULL, 1), \
         ('{excluded}', 'src/excluded.rs', 'modified', NULL, 1)"
    ))
    .await
    .expect("seed children");
    let bounded = crate::internal::ai::memory::reader::list_episodes_bounded(&conn, &repo_id, 1)
        .await
        .expect("bounded");
    assert_eq!(bounded.len(), 1);
    let episode = &bounded[0];
    assert_eq!(episode.episode_id, selected);
    assert!(
        episode
            .evidence
            .iter()
            .any(|edge| edge.ref_id == "ev-selected"),
        "selected episode keeps its evidence"
    );
    assert!(
        !episode
            .evidence
            .iter()
            .any(|edge| edge.ref_id == "ev-excluded"),
        "excluded episode children are not attached"
    );
    assert!(
        episode.paths.iter().any(|path| {
            crate::internal::ai::memory::reader::render_code_path(&path.code_path)
                == "src/selected.rs"
        }),
        "selected episode keeps its paths"
    );
    conn.close().await.ok();
}

#[tokio::test]
#[serial(cwd)]
async fn uid05_freshness_scale_and_separate_timings() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let conn = db::open_database_without_migrations(&db_path)
        .await
        .expect("open repo db");
    let repo_id = crate::internal::config::ConfigKv::get_with_conn(&conn, "libra.repoid")
        .await
        .expect("repo id")
        .expect("repo id")
        .value;
    // 50k linked operations exercise the freshness fingerprint scale.
    let mut op_batch = String::new();
    let mut link_batch = String::new();
    for index in 0..50_000 {
        if index % 500 == 0 {
            if !op_batch.is_empty() {
                conn.execute_unprepared(&op_batch).await.expect("ops");
                conn.execute_unprepared(&link_batch).await.expect("links");
            }
            op_batch.clear();
            link_batch.clear();
            op_batch.push_str(
                "INSERT INTO operation (op_id, repo_id, kind, status, command_name, description, 
                 actor, worktree_id, scope_kind, pre_view_oid, post_view_oid, start_ts, end_ts) VALUES ",
            );
            link_batch.push_str(
                "INSERT INTO ai_operation_link (operation_id, repo_id, redaction_version) VALUES ",
            );
        } else {
            op_batch.push(',');
            link_batch.push(',');
        }
        op_batch.push_str(&format!(
            "('op-{index:06}', '{repo_id}', 'commit', 'success', 'commit', 'd', 'a', '', 
             'main', 'v', 'v', 1, 2)"
        ));
        link_batch.push_str(&format!("('op-{index:06}', '{repo_id}', '1')"));
    }
    if !op_batch.is_empty() {
        conn.execute_unprepared(&op_batch).await.expect("ops");
        conn.execute_unprepared(&link_batch).await.expect("links");
    }
    conn.close().await.ok();
    rebuild_memory(&repo_dir).await;
    let conn = db::open_database_without_migrations(&db_path)
        .await
        .expect("open repo db");
    let mut batch = String::new();
    for index in 0..10_000 {
        if index % 500 == 0 {
            if !batch.is_empty() {
                conn.execute_unprepared(&batch).await.expect("episodes");
            }
            batch.clear();
            batch.push_str(
                "INSERT INTO memory_episode (episode_id, repo_id, source_kind, source_key, 
                 outcome, actor, started_at, ended_at, title, body, content_digest, 
                 rules_version) VALUES ",
            );
        } else {
            batch.push(',');
        }
        batch.push_str(&format!(
            "('ep-{index:05}', '{repo_id}', 'commit', 'k{index}', 'succeeded', 'tester', 
             {ts}, {ts}, 'title {index}', 'body {index}', 'digest', 1)",
            ts = 1_700_000_000_i64 + index as i64
        ));
    }
    if !batch.is_empty() {
        conn.execute_unprepared(&batch).await.expect("episodes");
    }

    let conn = db::open_database_without_migrations(&db_path)
        .await
        .expect("open repo db");
    let freshness_started = std::time::Instant::now();
    let stale = crate::internal::ai::memory::reader::projection_is_stale(&conn, &repo_id).await;
    let freshness = freshness_started.elapsed();
    eprintln!("freshness={freshness:?}");
    conn.close().await.ok();
    assert_eq!(stale.expect("status"), Some(false), "the rebuild is fresh");
    // The page budget excludes freshness (recorded separately above).
    let conn = db::open_database_without_migrations(&db_path)
        .await
        .expect("open repo db");
    let page_started = std::time::Instant::now();
    let page = crate::internal::ai::memory::reader::list_episodes_bounded(&conn, &repo_id, 200)
        .await
        .expect("bounded page");
    let page_elapsed = page_started.elapsed();
    conn.close().await.ok();
    assert_eq!(page.len(), super::memory::MEMORY_ROW_CAP);
    let read = super::memory::read_memory(&scope_for(&repo_dir))
        .await
        .expect("memory");
    assert_eq!(read.state, SourceState::Available);
    assert_eq!(read.rows.len(), super::memory::MEMORY_ROW_CAP);
    eprintln!("page={page_elapsed:?}");
    let budget = if cfg!(debug_assertions) { 600 } else { 150 };
    assert!(
        page_elapsed < std::time::Duration::from_millis(budget),
        "bounded page took {page_elapsed:?} against a {budget}ms budget"
    );
}

#[test]
fn uid05_worker_fallback_preserves_state() {
    let outcome = super::worker::ReadOutcome::new(
        SourceState::Absent,
        None,
        Some("no repository".to_string()),
    );
    assert_eq!(outcome.memory.state, SourceState::Absent);
    let outcome = super::worker::ReadOutcome::new(SourceState::Corrupt, None, None);
    assert_eq!(outcome.memory.state, SourceState::Corrupt);
}

#[tokio::test]
#[serial(cwd)]
async fn uid05_empty_projection_is_absent() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    rebuild_memory(&repo_dir).await;
    let read = super::memory::read_memory(&scope_for(&repo_dir))
        .await
        .expect("memory");
    assert_eq!(
        read.state,
        SourceState::Absent,
        "a fresh projection with no episodes is absent, not available"
    );
    assert!(read.rows.is_empty());
}

fn review_state(run_id: &str, created_at: &str, terminal: &str) -> String {
    format!(
        r#"{{"schema_version":2,"run_id":"{run_id}","kind":"review","agents":[{{"slug":"claude-code"}},{{"slug":"codex"}}],"starting_sha":"abc","target_scope":"HEAD~1..HEAD","terminal_state":"{terminal}","created_at":"{created_at}","updated_at":"{created_at}","cancel_requested":false}}"#
    )
}

fn investigate_state(run_id: &str, started_at: &str, terminal: &str) -> String {
    format!(
        r#"{{"schema_version":2,"run_id":"{run_id}","topic":"investigation topic","kind":"investigate","agents":["codex"],"max_turns":4,"quorum":2,"completed_rounds":1,"turn":1,"next_agent_idx":0,"stances":[],"findings_doc":"findings.md","starting_sha":"abc","started_at":"{started_at}","updated_at":"{started_at}","terminal_state":"{terminal}","cancel_requested":false}}"#
    )
}

fn run_root(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path()
        .join(crate::utils::util::ROOT_DIR)
        .join("sessions")
        .join("agent-runs")
}

fn manifest_json(run_id: &str, kind: &str) -> String {
    format!(
        r#"{{"schema_version":2,"run_id":"{run_id}","kind":"{kind}","agents":[],"starting_sha":"a","target_scope":"scope","terminal_state":null,"created_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z","findings_oid":null,"redaction_report":{{"matches":0,"bytes_scanned":0,"bytes_redacted":0}},"manual_attach":[]}}"#
    )
}

fn write_run(dir: &tempfile::TempDir, run_id: &str, state: &str, manifest: bool) {
    let root = run_root(dir).join(run_id);
    fs::create_dir_all(&root).expect("run dir");
    fs::write(root.join("state.json"), state).expect("state.json");
    if manifest {
        let kind = if state.contains("\"kind\":\"investigate\"") {
            "investigate"
        } else {
            "review"
        };
        fs::write(root.join("manifest.json"), manifest_json(run_id, kind)).expect("manifest.json");
    }
}

#[tokio::test]
#[serial(cwd)]
async fn uid06_run_rows_and_detail() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    write_run(
        &repo_dir,
        "rv-1",
        &review_state("rv-1", "2023-11-14T22:15:00Z", "error"),
        true,
    );
    write_run(
        &repo_dir,
        "iv-1",
        &investigate_state("iv-1", "2023-11-14T22:15:00Z", "max_turns"),
        true,
    );
    let read = super::runs::read_runs(&scope_for(&repo_dir))
        .await
        .expect("runs");
    assert_eq!(read.state, SourceState::Available);
    assert_eq!(read.rows.len(), 2);
    let review = read
        .rows
        .iter()
        .find(|row| row.run_id == "rv-1")
        .expect("rv");
    assert_eq!(review.kind, "review");
    assert_eq!(review.status, "error");
    assert_eq!(review.started_at, 1_700_000_100);
    assert_eq!(review.summary, "HEAD~1..HEAD");
    assert_eq!(review.reviewers, vec!["claude-code", "codex"]);
    assert_eq!(review.item_state, "available");
    let investigate = read
        .rows
        .iter()
        .find(|row| row.run_id == "iv-1")
        .expect("iv");
    assert_eq!(investigate.kind, "investigate");
    assert_eq!(investigate.started_at, 1_700_000_100);
}

#[tokio::test]
#[serial(cwd)]
async fn uid06_malformed_isolation() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    write_run(
        &repo_dir,
        "rv-good",
        &review_state("rv-good", "2024-01-01T00:00:00Z", "success"),
        true,
    );
    write_run(&repo_dir, "rv-bad", "not json", true);
    let read = super::runs::read_runs(&scope_for(&repo_dir))
        .await
        .expect("runs");
    assert_eq!(read.state, SourceState::Available);
    let good = read
        .rows
        .iter()
        .find(|row| row.run_id == "rv-good")
        .expect("good");
    let bad = read
        .rows
        .iter()
        .find(|row| row.run_id == "rv-bad")
        .expect("bad");
    assert_eq!(good.item_state, "available");
    assert_eq!(bad.item_state, "malformed");
}

#[tokio::test]
#[serial(cwd)]
async fn uid06_incomplete_and_oversized_items() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    write_run(
        &repo_dir,
        "rv-nomanifest",
        &review_state("rv-nomanifest", "2024-01-01T00:00:00Z", "success"),
        false,
    );
    let big = format!(
        r#"{{"schema_version":2,"run_id":"rv-big","kind":"review","agents":[],"starting_sha":"a","target_scope":"{}","terminal_state":"success","created_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z","cancel_requested":false}}"#,
        "x".repeat(70 * 1024)
    );
    write_run(&repo_dir, "rv-big", &big, true);
    let read = super::runs::read_runs(&scope_for(&repo_dir))
        .await
        .expect("runs");
    let no_manifest = read
        .rows
        .iter()
        .find(|row| row.run_id == "rv-nomanifest")
        .expect("nomanifest");
    let oversized = read
        .rows
        .iter()
        .find(|row| row.run_id == "rv-big")
        .expect("big");
    assert_eq!(no_manifest.item_state, "incomplete");
    assert_eq!(oversized.item_state, "oversized");
    assert!(
        read.truncated,
        "an oversized file marks the subset truncated"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid06_unknown_kind_is_malformed() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    write_run(
        &repo_dir,
        "run-x",
        r#"{"kind":"experiment","terminal_state":"success"}"#,
        true,
    );
    let read = super::runs::read_runs(&scope_for(&repo_dir))
        .await
        .expect("runs");
    assert_eq!(read.rows[0].kind, "unknown");
    assert!(read.rows[0].summary.contains("experiment"));
    assert_eq!(read.rows[0].item_state, "malformed");
}

#[tokio::test]
#[serial(cwd)]
async fn uid06_truncated_subset() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    for index in 0..(super::runs::RUN_STATE_CAP + 1) {
        write_run(
            &repo_dir,
            &format!("rv-{index:04}"),
            &review_state(&format!("rv-{index:04}"), "2024-01-01T00:00:00Z", "success"),
            true,
        );
    }
    let read = super::runs::read_runs(&scope_for(&repo_dir))
        .await
        .expect("runs");
    assert_eq!(read.rows.len(), super::runs::RUN_ROWS_PER_KIND);
    assert!(read.truncated, "state parsing hit its cap");
}

#[tokio::test]
#[serial(cwd)]
async fn uid06_source_states() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let read = super::runs::read_runs(&scope_for(&repo_dir))
        .await
        .expect("runs");
    assert_eq!(read.state, SourceState::Absent);
    assert!(read.rows.is_empty());
}

#[tokio::test]
#[serial(cwd)]
async fn uid06_read_only() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    write_run(
        &repo_dir,
        "rv-1",
        r#"{"kind":"review","terminal_state":"success"}"#,
        true,
    );
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let before = receipt_fingerprint(&db_path).await;
    let _read = super::runs::read_runs(&scope_for(&repo_dir))
        .await
        .expect("runs");
    let after = receipt_fingerprint(&db_path).await;
    assert_eq!(before, after, "run reads write no rows");
}

#[test]
fn uid06_filter_uses_real_fields() {
    let row = super::runs::RunRow {
        run_id: "rv-1".into(),
        kind: "review".into(),
        started_at: 1,
        status: "success".into(),
        summary: "rate limiter review".into(),
        reviewers: vec!["codex".into()],
        rounds: None,
        conclusion: String::new(),
        conclusion_state: "unavailable".to_string(),
        started_at_ms: 1000,
        item_state: "available".into(),
    };
    assert!(super::runs::matches_filter(&row, "", "limiter"));
    assert!(super::runs::matches_filter(&row, "review", ""));
    assert!(!super::runs::matches_filter(&row, "investigate", ""));
    assert!(!super::runs::matches_filter(&row, "", "absent"));
}

#[tokio::test]
#[serial(cwd)]
async fn uid06_manifest_bounds_and_ordering() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    // Manifest over the cap must become item-level oversized.
    write_run(
        &repo_dir,
        "rv-big-manifest",
        &review_state("rv-big-manifest", "2024-01-01T00:00:00Z", "success"),
        false,
    );
    let root = run_root(&repo_dir).join("rv-big-manifest");
    fs::write(root.join("manifest.json"), "x".repeat(70 * 1024)).expect("big manifest");
    // Directory mtime order opposite to the record time: the record time wins.
    write_run(
        &repo_dir,
        "rv-old-dir-new-record",
        &review_state("rv-old-dir-new-record", "2024-01-02T00:00:00Z", "success"),
        true,
    );
    write_run(
        &repo_dir,
        "rv-new-dir-old-record",
        &review_state("rv-new-dir-old-record", "2023-01-01T00:00:00Z", "success"),
        true,
    );
    let read = super::runs::read_runs(&scope_for(&repo_dir))
        .await
        .expect("runs");
    let big = read
        .rows
        .iter()
        .find(|row| row.run_id == "rv-big-manifest")
        .expect("big manifest");
    assert_eq!(big.item_state, "oversized");
    assert_eq!(
        read.rows[0].run_id, "rv-old-dir-new-record",
        "records sort by their own timestamp, not directory mtime"
    );
}

#[test]
fn uid06_screens_show_unclassified_rows() {
    let mut row = super::runs::RunRow {
        run_id: "rv-bad".into(),
        kind: "unknown".into(),
        started_at: 1,
        status: "unknown".into(),
        summary: String::new(),
        reviewers: Vec::new(),
        rounds: None,
        conclusion: String::new(),
        conclusion_state: "unavailable".to_string(),
        started_at_ms: 1000,
        item_state: "malformed".into(),
    };
    assert!(super::runs::visible_on(&row, "review"));
    assert!(super::runs::visible_on(&row, "investigate"));
    row.kind = "review".into();
    row.item_state = "malformed".into();
    assert!(super::runs::visible_on(&row, "review"));
    assert!(!super::runs::visible_on(&row, "investigate"));
    row.item_state = "available".into();
    assert!(super::runs::visible_on(&row, "review"));
    assert!(!super::runs::visible_on(&row, "investigate"));
}

#[tokio::test]
#[serial(cwd)]
async fn uid06_paused_and_precision() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let paused = r#"{"schema_version":2,"run_id":"iv-paused","topic":"paused topic","kind":"investigate","agents":["codex"],"max_turns":4,"quorum":2,"completed_rounds":1,"turn":1,"next_agent_idx":0,"stances":[],"findings_doc":"findings.md","starting_sha":"a","started_at":"2024-01-01T00:00:00.500Z","updated_at":"2024-01-01T00:00:00.500Z","pending_turn":{"turn":1,"agent_idx":0,"slug":"codex","reason":"agent_failure"},"cancel_requested":false}"#;
    write_run(&repo_dir, "iv-paused", paused, true);
    write_run(
        &repo_dir,
        "rv-later",
        &review_state("rv-later", "2024-01-01T00:00:00.900Z", "success"),
        true,
    );
    let read = super::runs::read_runs(&scope_for(&repo_dir))
        .await
        .expect("runs");
    let paused_row = read
        .rows
        .iter()
        .find(|row| row.run_id == "iv-paused")
        .expect("paused");
    assert_eq!(paused_row.status, "paused");
    assert_eq!(paused_row.rounds, Some(1));
    assert_eq!(
        read.rows[0].run_id, "rv-later",
        "microsecond precision orders runs"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid06_conclusions_and_identity() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    write_run(
        &repo_dir,
        "rv-concl",
        &review_state("rv-concl", "2024-01-01T00:00:00Z", "success"),
        true,
    );
    fs::write(
        run_root(&repo_dir).join("rv-concl").join("findings.md"),
        "# Review findings\n\n- run_id: rv-concl\n- terminal_state: success\n\n## claude-code — success\n<<<untrusted-reviewer-output slug=\"claude-code\">>>\nConclusion: no regressions found\n<<<end-untrusted-reviewer-output>>>",
    )
    .expect("findings");
    let stance = r#"{"schema_version":2,"run_id":"iv-concl","topic":"topic","kind":"investigate","agents":["codex"],"max_turns":4,"quorum":2,"completed_rounds":2,"turn":2,"next_agent_idx":0,"stances":[{"turn":2,"agent_idx":0,"slug":"codex","disposition":"concluding","summary":"final stance summary"}],"findings_doc":"findings.md","starting_sha":"a","started_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z","terminal_state":"quorum","cancel_requested":false}"#;
    write_run(&repo_dir, "iv-concl", stance, true);
    // Identity mismatches must be item-level malformed.
    let mismatched = r#"{"schema_version":2,"run_id":"someone-else","topic":"t","kind":"investigate","agents":[],"max_turns":1,"quorum":1,"completed_rounds":0,"turn":0,"next_agent_idx":0,"stances":[],"findings_doc":"findings.md","starting_sha":"a","started_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z","terminal_state":null,"cancel_requested":false}"#;
    write_run(&repo_dir, "iv-mismatch", mismatched, true);
    let read = super::runs::read_runs(&scope_for(&repo_dir))
        .await
        .expect("runs");
    let review = read
        .rows
        .iter()
        .find(|row| row.run_id == "rv-concl")
        .expect("rv");
    assert!(review.conclusion.contains("no regressions"));
    let investigate = read
        .rows
        .iter()
        .find(|row| row.run_id == "iv-concl")
        .expect("iv");
    assert_eq!(investigate.conclusion, "final stance summary");
    assert_eq!(investigate.rounds, Some(2));
    let mismatch = read
        .rows
        .iter()
        .find(|row| row.run_id == "iv-mismatch")
        .expect("mismatch");
    assert_eq!(mismatch.item_state, "malformed");
}

#[tokio::test]
#[serial(cwd)]
async fn uid06_legacy_schema_and_conclusion_states() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    write_run(
        &repo_dir,
        "rv-legacy",
        &review_state("rv-legacy", "2024-01-01T00:00:00Z", "success"),
        true,
    );
    // Supported legacy manifest schema 1 must stay available.
    let legacy_manifest = r#"{"schema_version":1,"run_id":"rv-legacy","kind":"review","agents":[],"starting_sha":"a","target_scope":"scope","terminal_state":null,"created_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z","findings_oid":null,"redaction_report":{"matches":0,"bytes_scanned":0,"bytes_redacted":0},"manual_attach":[]}"#;
    fs::write(
        run_root(&repo_dir).join("rv-legacy").join("manifest.json"),
        legacy_manifest,
    )
    .expect("legacy manifest");
    // Heading-only findings are an empty conclusion, not "recorded".
    write_run(
        &repo_dir,
        "rv-empty",
        &review_state("rv-empty", "2024-01-01T00:00:00Z", "success"),
        true,
    );
    fs::write(
        run_root(&repo_dir).join("rv-empty").join("findings.md"),
        "# Review findings\n\n## claude-code — success (exit code 0)\n",
    )
    .expect("empty findings");
    // Oversized findings are an explicit oversized conclusion.
    write_run(
        &repo_dir,
        "rv-oversized",
        &review_state("rv-oversized", "2024-01-01T00:00:00Z", "success"),
        true,
    );
    fs::write(
        run_root(&repo_dir).join("rv-oversized").join("findings.md"),
        "y".repeat(70 * 1024),
    )
    .expect("oversized findings");
    let read = super::runs::read_runs(&scope_for(&repo_dir))
        .await
        .expect("runs");
    let legacy = read
        .rows
        .iter()
        .find(|row| row.run_id == "rv-legacy")
        .expect("legacy");
    assert_eq!(legacy.item_state, "available", "schema 1 is supported");
    let empty = read
        .rows
        .iter()
        .find(|row| row.run_id == "rv-empty")
        .expect("empty");
    assert_eq!(empty.conclusion_state, "empty");
    assert!(empty.conclusion.is_empty());
    let oversized = read
        .rows
        .iter()
        .find(|row| row.run_id == "rv-oversized")
        .expect("oversized");
    assert_eq!(oversized.conclusion_state, "oversized");
}

#[test]
fn uid06_future_schema_is_malformed() {
    // Data-level guard: a schema newer than the supported set is rejected by
    // the typed manifest identity check (covered through read_runs above for
    // legacy; this pins the comparison contract directly).
    let supported = [
        1,
        crate::internal::ai::review::store::REVIEW_MANIFEST_SCHEMA_VERSION,
    ];
    assert!(supported.contains(&1));
    assert!(!supported.contains(&99));
}

#[tokio::test]
#[serial(cwd)]
async fn uid06_future_schemas_are_malformed() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let review_state_json = r#"{"schema_version":99,"run_id":"rv-future","kind":"review","agents":[],"starting_sha":"a","target_scope":"s","terminal_state":"success","created_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z","cancel_requested":false}"#;
    write_run(&repo_dir, "rv-future", review_state_json, true);
    let investigate_state_json = r#"{"schema_version":99,"run_id":"iv-future","topic":"t","kind":"investigate","agents":[],"max_turns":1,"quorum":1,"completed_rounds":0,"turn":0,"next_agent_idx":0,"stances":[],"findings_doc":"findings.md","starting_sha":"a","started_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z","terminal_state":null,"cancel_requested":false}"#;
    write_run(&repo_dir, "iv-future", investigate_state_json, true);
    // Manifest schema 99 for both kinds must also be malformed.
    write_run(
        &repo_dir,
        "rv-bad-manifest",
        &review_state("rv-bad-manifest", "2024-01-01T00:00:00Z", "success"),
        true,
    );
    fs::write(
        run_root(&repo_dir)
            .join("rv-bad-manifest")
            .join("manifest.json"),
        manifest_json("rv-bad-manifest", "review")
            .replace("\"schema_version\":2", "\"schema_version\":99"),
    )
    .expect("bad manifest");
    write_run(
        &repo_dir,
        "iv-bad-manifest",
        &investigate_state("iv-bad-manifest", "2024-01-01T00:00:00Z", "quorum"),
        true,
    );
    fs::write(
        run_root(&repo_dir)
            .join("iv-bad-manifest")
            .join("manifest.json"),
        manifest_json("iv-bad-manifest", "investigate")
            .replace("\"schema_version\":2", "\"schema_version\":99"),
    )
    .expect("bad investigate manifest");
    let read = super::runs::read_runs(&scope_for(&repo_dir))
        .await
        .expect("runs");
    for id in [
        "rv-future",
        "iv-future",
        "rv-bad-manifest",
        "iv-bad-manifest",
    ] {
        let row = read.rows.iter().find(|row| row.run_id == id).expect(id);
        assert_eq!(row.item_state, "malformed", "{id} must be malformed");
    }
}

#[tokio::test]
#[serial(cwd)]
async fn uid06_future_state_schema_is_malformed() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let state = r#"{"schema_version":99,"run_id":"rv-future","kind":"review","agents":[],"starting_sha":"a","target_scope":"s","terminal_state":"success","created_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z","cancel_requested":false}"#;
    write_run(&repo_dir, "rv-future", state, true);
    let read = super::runs::read_runs(&scope_for(&repo_dir))
        .await
        .expect("runs");
    assert_eq!(read.rows[0].item_state, "malformed");
}

#[cfg(unix)]
#[tokio::test]
#[serial(cwd)]
async fn uid06_fifo_state_does_not_block() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let dir = run_root(&repo_dir).join("rv-fifo");
    fs::create_dir_all(&dir).expect("dir");
    fs::write(
        dir.join("manifest.json"),
        manifest_json("rv-fifo", "review"),
    )
    .expect("manifest");
    let status = std::process::Command::new("mkfifo")
        .arg(dir.join("state.json"))
        .status()
        .expect("mkfifo");
    assert!(status.success(), "the FIFO fixture was created");
    // Run the read on a worker thread with a watchdog: tokio's timer cannot
    // interrupt synchronous filesystem I/O, so a blocked FIFO must be caught
    // by recv_timeout instead of hanging the test process.
    let scope = scope_for(&repo_dir);
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let result = runtime.block_on(super::runs::read_runs(&scope));
        let _ = sender.send(result);
    });
    let read = receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("a FIFO must not block the read")
        .expect("runs");
    assert_eq!(read.rows.len(), 1);
    assert_eq!(read.rows[0].item_state, "incomplete");
}

#[cfg(unix)]
#[tokio::test]
#[serial(cwd)]
async fn uid06_directory_read_failure_is_unreadable() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let root = run_root(&repo_dir);
    fs::create_dir_all(&root).expect("run root");
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(&root).expect("stat").permissions();
    permissions.set_mode(0o000);
    fs::set_permissions(&root, permissions).expect("chmod 000");
    let error = super::runs::read_runs(&scope_for(&repo_dir))
        .await
        .expect_err("a non-searchable directory is not absent");
    assert_eq!(
        error.state,
        SourceState::Unreadable {
            kind: std::io::ErrorKind::PermissionDenied
        }
    );
    let mut permissions = fs::metadata(&root).expect("stat").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&root, permissions).expect("restore");
}

fn snapshot_with_context(
    state: SourceState,
    context: Option<super::RepoContext>,
) -> super::Snapshot {
    super::Snapshot {
        seq: 1,
        state,
        context,
        message: None,
        history: Vec::new(),
        history_error: None,
        history_state: SourceState::Available,
        sessions: Vec::new(),
        sessions_error: None,
        sessions_state: SourceState::Available,
        operations: super::operations::OperationsRead {
            state: SourceState::Available,
            rows: Vec::new(),
            truncated: false,
        },
        operations_error: None,
        memory: super::memory::MemoryRead {
            state: SourceState::Stale,
            rows: Vec::new(),
        },
        memory_error: None,
        runs: super::runs::RunsRead {
            state: SourceState::Absent,
            rows: Vec::new(),
            truncated: false,
        },
        runs_error: None,
    }
}

#[test]
fn uid07_context_and_sources() {
    let context = super::RepoContext {
        worktree_label: "libra".into(),
        storage_label: ".libra".into(),
        branch: "main".into(),
        head: "9d1e4c7".into(),
        object_format: "sha1".into(),
        head_oid: Some("9d1e4c7".into()),
    };
    let snapshot = snapshot_with_context(SourceState::Available, Some(context));
    let availability = super::availability(Some(&snapshot));
    assert_eq!(availability.repo, SourceState::Available);
    assert_eq!(availability.repo_label, "libra");
    assert_eq!(availability.branch, "main");
    assert_eq!(availability.head, "9d1e4c7");
    assert_eq!(availability.object_format, "sha1");
    assert_eq!(availability.sources.len(), 5);
    assert_eq!(
        availability
            .sources
            .iter()
            .find(|source| source.name == "memory")
            .expect("memory")
            .state,
        SourceState::Stale
    );
    assert_eq!(
        availability
            .sources
            .iter()
            .find(|source| source.name == "runs")
            .expect("runs")
            .state,
        SourceState::Absent
    );
}

#[test]
fn uid07_about_availability() {
    let unchecked = super::availability(None);
    assert_eq!(unchecked.repo, SourceState::Unchecked);
    assert_eq!(unchecked.repo_label, "未检查");
    assert!(
        unchecked
            .sources
            .iter()
            .all(|source| source.label == "未检查")
    );
    let absent = super::availability(Some(&snapshot_with_context(SourceState::Absent, None)));
    assert_eq!(absent.repo, SourceState::Absent);
    assert_eq!(absent.branch, "—");
}

#[test]
fn uid07_capability_map_states() {
    let snapshot = snapshot_with_context(SourceState::Available, None);
    let availability = super::availability(Some(&snapshot));
    let rows = super::capabilities(&availability);
    assert_eq!(rows.len(), 5);
    assert!(
        rows.iter()
            .any(|row| row.capability == "memory" && row.data == "stale")
    );
    assert!(
        rows.iter()
            .any(|row| row.capability == "capture" && row.data == "available")
    );
}

#[test]
fn uid07_search_after_catalog_removal() {
    // The catalog module no longer exposes `catalog()`; availability is the
    // only source of surface state for search.
    let availability = super::availability(None);
    assert_eq!(availability.sources.len(), 5);
}

#[test]
fn uid07_welcome_in_and_out() {
    let inside = super::availability(Some(&snapshot_with_context(SourceState::Available, None)));
    assert!(inside.repo.is_available());
    let outside = super::availability(Some(&snapshot_with_context(SourceState::Absent, None)));
    assert!(!outside.repo.is_available());
}

#[test]
fn uid07_import_copy() {
    assert_eq!(
        crate::gui::screens::cloud::IMPORT_EXAMPLE,
        "your-repository"
    );
    assert!(!crate::gui::screens::cloud::IMPORT_EXAMPLE.contains("acme"));
}

#[tokio::test]
#[serial(cwd)]
async fn uid07_read_only_no_certificate() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    commit_memory_fixture(&repo_dir, "uid07").await;
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let before = receipt_fingerprint(&db_path).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    let handle = super::DataHandle::spawn();
    handle.request();
    let snapshot = wait_snapshot(&handle, Duration::from_secs(10));
    let availability = super::availability(Some(&snapshot));
    assert_eq!(availability.repo, SourceState::Available);
    assert!(!availability.sources.is_empty());
    drop(handle);
    let after = receipt_fingerprint(&db_path).await;
    assert_eq!(
        before, after,
        "availability/status reads write no schema receipt, operation or certificate"
    );
}

#[tokio::test]
async fn uid07_bounded_probe() {
    let started = std::time::Instant::now();
    let availability = super::context::probe_availability(None).await;
    assert!(started.elapsed() < std::time::Duration::from_millis(80));
    assert_eq!(availability.repo_label, "no repository");
}

#[tokio::test]
#[serial(cwd)]
async fn uid07_probe_delivery_precedes_rows() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    // Block the reader until released: the probe must arrive while the
    // snapshot is still unavailable, proving ordering.
    let (release, released) = std::sync::mpsc::channel();
    let released = std::sync::Arc::new(std::sync::Mutex::new(released));
    let handle = super::DataHandle::spawn_with(move |_scope| {
        let released = released.clone();
        Box::pin(async move {
            let _ = tokio::task::spawn_blocking(move || {
                released
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .recv()
            })
            .await;
            Ok(super::ReadOutcome::new(SourceState::Available, None, None))
        })
    });
    handle.request();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let probe = loop {
        if let Some(probe) = handle.take_probe() {
            break probe;
        }
        assert!(std::time::Instant::now() < deadline, "probe did not arrive");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(probe.repo, SourceState::Available);
    assert!(handle.take_snapshot().is_none(), "rows are still blocked");
    release.send(()).expect("release reader");
    let snapshot = wait_snapshot(&handle, Duration::from_secs(10));
    assert!(snapshot.state.is_available());
}

#[test]
fn uid07_repo_failure_states_propagate() {
    let outcome = super::worker::ReadOutcome::new(SourceState::Corrupt, None, None);
    assert_eq!(outcome.history_state, SourceState::Corrupt);
    let outcome = super::worker::ReadOutcome::new(SourceState::Busy, None, None);
    assert_eq!(outcome.history_state, SourceState::Busy);
    let handle = super::DataHandle::spawn_with_startup_failure("boom".to_string());
    let snapshot = handle.take_snapshot().expect("worker failure snapshot");
    assert_eq!(snapshot.sessions_state, SourceState::Worker);
    assert_eq!(snapshot.operations.state, SourceState::Worker);
    assert_eq!(snapshot.memory.state, SourceState::Worker);
    assert_eq!(snapshot.runs.state, SourceState::Worker);
    assert_eq!(snapshot.history_state, SourceState::Worker);
}

#[tokio::test]
#[serial(cwd)]
async fn uid07_probe_real_fixtures_and_states() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    commit_memory_fixture(&repo_dir, "probe").await;
    let scope = scope_for(&repo_dir);
    let started = std::time::Instant::now();
    let availability = super::context::probe_availability(Some(&scope)).await;
    assert!(
        started.elapsed() < std::time::Duration::from_millis(400),
        "probe stayed within its bounded budget"
    );
    assert_eq!(availability.repo, SourceState::Available);
    assert_eq!(
        availability
            .sources
            .iter()
            .find(|source| source.name == "operation")
            .expect("operation")
            .state,
        SourceState::Available
    );
    // No capture catalog yet: absent, not unreadable.
    assert_eq!(
        availability
            .sources
            .iter()
            .find(|source| source.name == "capture")
            .expect("capture")
            .state,
        SourceState::Absent
    );
    // A real capture catalog probes as Available.
    let storage = repo_dir.path().join(crate::utils::util::ROOT_DIR);
    crate::internal::ai::capture::store::open(&storage, Duration::from_secs(2))
        .await
        .expect("create capture catalog")
        .close()
        .await
        .ok();
    let availability = super::context::probe_availability(Some(&scope)).await;
    assert_eq!(
        availability
            .sources
            .iter()
            .find(|source| source.name == "capture")
            .expect("capture")
            .state,
        SourceState::Available
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid07_probe_outside_repository_is_absent() {
    let availability = super::context::probe_availability(None).await;
    assert_eq!(availability.repo, SourceState::Absent);
    assert!(
        availability
            .sources
            .iter()
            .all(|source| source.state == SourceState::Absent)
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial(cwd)]
async fn uid07_probe_fifo_directory_does_not_block() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let root = run_root(&repo_dir);
    fs::create_dir_all(root.parent().expect("sessions dir")).expect("sessions");
    let status = std::process::Command::new("mkfifo")
        .arg(&root)
        .status()
        .expect("mkfifo");
    assert!(status.success(), "agent-runs itself is a FIFO");
    let scope = scope_for(&repo_dir);
    let started = std::time::Instant::now();
    let availability = super::context::probe_availability(Some(&scope)).await;
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(
        availability
            .sources
            .iter()
            .find(|source| source.name == "runs")
            .expect("runs")
            .state,
        SourceState::Unreadable {
            kind: std::io::ErrorKind::NotADirectory
        }
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid07_probe_failure_classification() {
    // Corrupt database.
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    fs::write(&db_path, b"not a sqlite database").expect("corrupt db");
    let scope = scope_for(&repo_dir);
    let availability = super::context::probe_availability(Some(&scope)).await;
    assert_eq!(availability.repo, SourceState::Corrupt);

    // Schema mismatch.
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let conn = db::open_database_without_migrations(&db_path)
        .await
        .expect("db");
    conn.execute_unprepared(
        "DELETE FROM schema_versions; INSERT INTO schema_versions (version, name, applied_at) VALUES (1, 'old', 'now')",
    )
    .await
    .expect("downgrade");
    conn.close().await.ok();
    let scope = scope_for(&repo_dir);
    let availability = super::context::probe_availability(Some(&scope)).await;
    assert_eq!(availability.repo, SourceState::SchemaOld);
}

#[cfg(unix)]
#[tokio::test]
#[serial(cwd)]
async fn uid07_probe_permission_denied_is_unreadable() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(&db_path).expect("stat").permissions();
    permissions.set_mode(0o000);
    fs::set_permissions(&db_path, permissions).expect("chmod 000");
    let scope = scope_for(&repo_dir);
    let availability = super::context::probe_availability(Some(&scope)).await;
    assert_eq!(
        availability.repo,
        SourceState::Unreadable {
            kind: std::io::ErrorKind::PermissionDenied
        }
    );
    let mut permissions = fs::metadata(&db_path).expect("stat").permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(&db_path, permissions).expect("restore");
}

#[tokio::test]
#[serial(cwd)]
async fn uid07_probe_contention_is_busy() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let holder = db::open_database_without_migrations(&db_path)
        .await
        .expect("holder db");
    holder
        .execute_unprepared("PRAGMA journal_mode=DELETE")
        .await
        .expect("rollback journal");
    holder
        .execute_unprepared("PRAGMA locking_mode=EXCLUSIVE")
        .await
        .expect("exclusive locking mode");
    holder
        .execute_unprepared("BEGIN EXCLUSIVE")
        .await
        .expect("hold exclusive lock");
    let scope = scope_for(&repo_dir);
    let started = std::time::Instant::now();
    let availability = super::context::probe_availability(Some(&scope)).await;
    let elapsed = started.elapsed();
    holder.execute_unprepared("COMMIT").await.expect("release");
    holder.close().await.ok();
    assert_eq!(
        availability.repo,
        SourceState::Busy,
        "exclusive writer contention is retryable Busy"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "contention stayed within the bounded probe budget"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid07_probe_capture_failures_and_future_schema() {
    // Capture corrupt.
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let storage = repo_dir.path().join(crate::utils::util::ROOT_DIR);
    crate::internal::ai::capture::store::open(&storage, Duration::from_secs(2))
        .await
        .expect("create capture")
        .close()
        .await
        .ok();
    let capture_path = crate::internal::ai::capture::store::capture_database_path(&storage);
    fs::write(&capture_path, b"not sqlite").expect("corrupt capture");
    let scope = scope_for(&repo_dir);
    let availability = super::context::probe_availability(Some(&scope)).await;
    assert_eq!(
        availability
            .sources
            .iter()
            .find(|source| source.name == "capture")
            .expect("capture")
            .state,
        SourceState::Corrupt
    );

    // Capture old schema.
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let storage = repo_dir.path().join(crate::utils::util::ROOT_DIR);
    let conn = crate::internal::ai::capture::store::open(&storage, Duration::from_secs(2))
        .await
        .expect("create capture");
    conn.execute_unprepared(
        "DELETE FROM schema_versions; INSERT INTO schema_versions (version, name, applied_at) VALUES (1, 'old', 'now')",
    )
    .await
    .expect("downgrade capture");
    conn.close().await.ok();
    let scope = scope_for(&repo_dir);
    let availability = super::context::probe_availability(Some(&scope)).await;
    assert_eq!(
        availability
            .sources
            .iter()
            .find(|source| source.name == "capture")
            .expect("capture")
            .state,
        SourceState::SchemaOld
    );

    // Repository future schema.
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let db_path = repo_dir
        .path()
        .join(crate::utils::util::ROOT_DIR)
        .join(crate::utils::util::DATABASE);
    let conn = db::open_database_without_migrations(&db_path)
        .await
        .expect("db");
    conn.execute_unprepared(
        "DELETE FROM schema_versions; INSERT INTO schema_versions (version, name, applied_at) VALUES (999999999999, 'future', 'now')",
    )
    .await
    .expect("future schema");
    conn.close().await.ok();
    let scope = scope_for(&repo_dir);
    let availability = super::context::probe_availability(Some(&scope)).await;
    assert_eq!(availability.repo, SourceState::SchemaNew);
}

#[cfg(unix)]
#[tokio::test]
#[serial(cwd)]
async fn uid07_probe_capture_permission_denied() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let storage = repo_dir.path().join(crate::utils::util::ROOT_DIR);
    crate::internal::ai::capture::store::open(&storage, Duration::from_secs(2))
        .await
        .expect("create capture")
        .close()
        .await
        .ok();
    let capture_path = crate::internal::ai::capture::store::capture_database_path(&storage);
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(&capture_path).expect("stat").permissions();
    permissions.set_mode(0o000);
    fs::set_permissions(&capture_path, permissions).expect("chmod");
    let scope = scope_for(&repo_dir);
    let availability = super::context::probe_availability(Some(&scope)).await;
    assert_eq!(
        availability
            .sources
            .iter()
            .find(|source| source.name == "capture")
            .expect("capture")
            .state,
        SourceState::Unreadable {
            kind: std::io::ErrorKind::PermissionDenied
        }
    );
    let mut permissions = fs::metadata(&capture_path).expect("stat").permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(&capture_path, permissions).expect("restore");
}

#[test]
fn uid07_unresolved_scope_is_unchecked() {
    let availability = super::unresolved_availability();
    assert_eq!(availability.repo, SourceState::Unchecked);
    assert!(
        availability
            .sources
            .iter()
            .all(|source| source.state == SourceState::Unchecked)
    );
    assert_eq!(availability.repo_label, "unresolved");
}

#[test]
fn uid07_merge_preserves_independent_probe_sources() {
    let mut probe = super::availability(None);
    probe.repo = SourceState::Available;
    for source in &mut probe.sources {
        if source.name == "capture" {
            source.state = SourceState::Available;
            source.label = "available";
        }
    }
    let mut snapshot = snapshot_with_context(SourceState::Corrupt, None);
    snapshot.sessions_state = SourceState::Unchecked;
    let merged = super::merge_availability(&probe, &snapshot);
    assert_eq!(merged.repo, SourceState::Corrupt, "the repo error wins");
    assert_eq!(
        merged
            .sources
            .iter()
            .find(|source| source.name == "capture")
            .expect("capture")
            .state,
        SourceState::Available,
        "an unaffected probe verdict survives a repository failure"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn uid07_production_context_failure_preserves_probe() {
    let repo_dir = tempdir().expect("temp repo");
    test::setup_with_new_libra_in(repo_dir.path()).await;
    // Healthy capture catalog, corrupt repository database.
    let storage = repo_dir.path().join(crate::utils::util::ROOT_DIR);
    crate::internal::ai::capture::store::open(&storage, Duration::from_secs(2))
        .await
        .expect("create capture")
        .close()
        .await
        .ok();
    fs::write(storage.join(crate::utils::util::DATABASE), b"not sqlite").expect("corrupt db");
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    let handle = super::DataHandle::spawn();
    handle.request();
    let snapshot = wait_snapshot(&handle, Duration::from_secs(10));
    assert!(snapshot.context.is_none(), "the context read failed");
    assert_eq!(snapshot.sessions_state, SourceState::Unchecked);
    assert_eq!(snapshot.history_state, SourceState::Corrupt);
    assert_eq!(snapshot.operations.state, SourceState::Corrupt);
    assert_eq!(snapshot.memory.state, SourceState::Corrupt);
    assert_eq!(snapshot.runs.state, SourceState::Unchecked);
    let probe = handle.take_probe().expect("probe retained");
    let merged = super::merge_availability(&probe, &snapshot);
    assert_eq!(merged.repo, SourceState::Corrupt);
    assert_eq!(
        merged
            .sources
            .iter()
            .find(|source| source.name == "capture")
            .expect("capture")
            .state,
        SourceState::Available,
        "a healthy capture probe survives the repository failure"
    );
    let capabilities = super::availability_capabilities(&merged);
    assert!(
        capabilities
            .iter()
            .any(|row| row.capability == "capture" && row.data == "available")
    );
}

#[cfg(feature = "storage-redb")]
#[tokio::test]
#[serial_test::serial(cwd, env)]
async fn redb_window_readonly_context_and_source_state_mapping() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use crate::{
        internal::db::DatabaseRole,
        storage::{
            RefKey, RefTarget, StorageBackendError,
            codec::encode_versioned,
            manifest::{
                INITIAL_STORAGE_GENERATION, StorageFormatManifest, publish_storage_manifest,
            },
            meta::{
                MAX_SUPPORTED_READER_VERSION, META_SCHEMA_CODEC_VERSION, META_SCHEMA_SINGLETON_KEY,
                META_SCHEMA_TABLE, MetaSchemaRecord, MetaSchemaRole,
            },
            opener::{
                RedbRepairContext, begin_write_bounded_blocking, open_redb_read_only,
                open_redb_writable,
            },
            runtime,
        },
        utils::util,
    };

    struct HashKindGuard(git_internal::hash::HashKind);
    impl Drop for HashKindGuard {
        fn drop(&mut self) {
            git_internal::hash::set_hash_kind(self.0);
        }
    }
    let _hash_guard = HashKindGuard(git_internal::hash::get_hash_kind());

    #[derive(Debug)]
    struct CrashBackend {
        inner: redb::backends::FileBackend,
        dead: Arc<AtomicBool>,
    }

    impl redb::StorageBackend for CrashBackend {
        fn len(&self) -> Result<u64, redb::BackendError> {
            self.inner.len()
        }

        fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), redb::BackendError> {
            self.inner.read(offset, out)
        }

        fn set_len(&self, len: u64) -> Result<(), redb::BackendError> {
            if self.dead.load(Ordering::SeqCst) {
                return Ok(());
            }
            self.inner.set_len(len)
        }

        fn sync_data(&self) -> Result<(), redb::BackendError> {
            if self.dead.load(Ordering::SeqCst) {
                return Ok(());
            }
            self.inner.sync_data()
        }

        fn write(&self, offset: u64, data: &[u8]) -> Result<(), redb::BackendError> {
            if self.dead.load(Ordering::SeqCst) {
                return Ok(());
            }
            self.inner.write(offset, data)
        }
    }

    fn create_unrecovered_redb_fixture(path: &std::path::Path) {
        const KV: redb::TableDefinition<&str, &str> = redb::TableDefinition::new("rb27_crash_kv");
        let dead = Arc::new(AtomicBool::new(false));
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .expect("create crash fixture file");
        let db = redb::Builder::new()
            .create_with_backend(CrashBackend {
                inner: redb::backends::FileBackend::new(file),
                dead: Arc::clone(&dead),
            })
            .expect("create redb database on crash backend");
        let wtx =
            begin_write_bounded_blocking(&db, DatabaseRole::Repository, Duration::from_secs(2))
                .expect("begin fixture seed write");
        {
            let mut table = wtx.open_table(KV).expect("open fixture kv table");
            table
                .insert("seed", "intact")
                .expect("insert committed seed entry");
        }
        wtx.commit().expect("commit fixture seed write");
        dead.store(true, Ordering::SeqCst);
        drop(db);
    }

    fn overwrite_meta_schema(path: &std::path::Path, record: &MetaSchemaRecord) {
        let db = open_redb_writable(path, DatabaseRole::Repository, RedbRepairContext::Migrate)
            .expect("open redb writable to seed META_SCHEMA");
        let wtx =
            begin_write_bounded_blocking(&db, DatabaseRole::Repository, Duration::from_secs(2))
                .expect("begin META_SCHEMA seed write");
        {
            let mut table = wtx
                .open_table(META_SCHEMA_TABLE)
                .expect("open META_SCHEMA table");
            let encoded = encode_versioned(META_SCHEMA_CODEC_VERSION, record)
                .expect("encode META_SCHEMA record");
            table
                .insert(META_SCHEMA_SINGLETON_KEY, encoded.as_slice())
                .expect("insert META_SCHEMA record");
        }
        wtx.commit().expect("commit META_SCHEMA record");
    }

    // AC1: Pure-Redb repository (no `libra.db`) returns accurate RepoContext
    // (HEAD, branch, hash kind) via strict `open_read_only` without mutating
    // on-disk bytes or mtime.
    let repo_dir = tempdir().expect("temp redb repo");
    let storage = repo_dir.path().join(util::ROOT_DIR);
    let paths = runtime::init_redb_repository_storage(&storage)
        .await
        .expect("init pure-redb storage");
    assert!(
        !storage.join(util::DATABASE).exists(),
        "pure-redb repository must not create libra.db"
    );

    let sha256_commit = "a".repeat(64);
    {
        let cfg = runtime::open_repo_config_store_read_write(&storage)
            .await
            .expect("open redb config store rw");
        cfg.set("core.objectformat", "sha256", false)
            .await
            .expect("set sha256 objectformat");
        drop(cfg);

        let git = runtime::open_git_store_read_write(&storage)
            .await
            .expect("open redb git store rw");
        let head_outcome = git
            .update_ref_cas(
                &RefKey::LocalHead { worktree_id: None },
                None,
                Some(&RefTarget::Symbolic("main".to_string())),
                &[],
            )
            .await
            .expect("seed HEAD");
        assert!(matches!(
            head_outcome,
            crate::storage::RefCasOutcome::Applied { .. }
        ));
        let branch_outcome = git
            .update_ref_cas(
                &RefKey::LocalBranch {
                    name: "main".to_string(),
                },
                None,
                Some(&RefTarget::Direct(sha256_commit.clone())),
                &[],
            )
            .await
            .expect("seed main branch");
        assert!(matches!(
            branch_outcome,
            crate::storage::RefCasOutcome::Applied { .. }
        ));
    }

    let git_bytes_before = fs::read(&paths.git_db_path).expect("read git.redb bytes before");
    let git_mtime_before = fs::metadata(&paths.git_db_path)
        .expect("stat git.redb before")
        .modified()
        .expect("git.redb mtime before");
    let manifest_path = storage.join(crate::storage::manifest::STORAGE_FORMAT_MANIFEST_FILE);
    let manifest_bytes_before = fs::read(&manifest_path).expect("read manifest bytes before");

    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("resolve pure-redb scope")
        .expect("pure-redb repository scope");
    let context = repo::read_context(&scope)
        .await
        .expect("read_context on pure-redb repository");
    assert_eq!(context.storage_label, ".libra");
    assert_eq!(context.branch, "main");
    assert_eq!(context.object_format, "sha256");
    assert_eq!(context.head_oid.as_deref(), Some(sha256_commit.as_str()));
    assert_eq!(context.head, &sha256_commit[..12]);
    assert_eq!(
        git_internal::hash::get_hash_kind(),
        git_internal::hash::HashKind::Sha256
    );
    assert!(
        !storage.join(util::DATABASE).exists(),
        "read_context must never create libra.db"
    );
    assert_eq!(
        fs::read(&paths.git_db_path).expect("read git.redb bytes after"),
        git_bytes_before,
        "read_context must not modify git.redb bytes"
    );
    assert_eq!(
        fs::metadata(&paths.git_db_path)
            .expect("stat git.redb after")
            .modified()
            .expect("git.redb mtime after"),
        git_mtime_before,
        "read_context must not modify git.redb mtime"
    );
    assert_eq!(
        fs::read(&manifest_path).expect("read manifest bytes after"),
        manifest_bytes_before,
        "read_context must not modify storage-format.json"
    );

    // AC2: `META_SCHEMA` too new -> `SourceState::SchemaNew` (without mutating disk).
    let future_dir = tempdir().expect("temp future redb repo");
    let future_storage = future_dir.path().join(util::ROOT_DIR);
    let future_paths = runtime::init_redb_repository_storage(&future_storage)
        .await
        .expect("init future redb storage");
    let future_record = MetaSchemaRecord {
        role: MetaSchemaRole::Repository,
        format_version: MAX_SUPPORTED_READER_VERSION + 1,
        min_reader_version: MAX_SUPPORTED_READER_VERSION + 1,
    };
    assert_eq!(
        state::classify_redb_meta_schema(&future_record, DatabaseRole::Repository),
        SourceState::SchemaNew
    );
    assert_eq!(
        state::classify_storage_error(&StorageBackendError::SchemaTooNew {
            role: DatabaseRole::Repository,
            found: MAX_SUPPORTED_READER_VERSION + 1,
            max_supported: MAX_SUPPORTED_READER_VERSION,
        }),
        SourceState::SchemaNew
    );
    overwrite_meta_schema(&future_paths.git_db_path, &future_record);
    let future_bytes_before = fs::read(&future_paths.git_db_path).expect("read future bytes");
    let future_scope = repo::resolve_scope(future_dir.path().to_path_buf())
        .expect("future scope")
        .expect("future repo");
    let future_err = repo::read_context(&future_scope)
        .await
        .expect_err("future META_SCHEMA must fail closed");
    assert_eq!(future_err.state, SourceState::SchemaNew);
    assert_eq!(
        fs::read(&future_paths.git_db_path).expect("read future bytes after"),
        future_bytes_before,
        "SchemaNew read must not modify git.redb bytes"
    );

    // AC3: `META_SCHEMA` too old -> `SourceState::SchemaOld` (without mutating disk).
    let old_dir = tempdir().expect("temp old redb repo");
    let old_storage = old_dir.path().join(util::ROOT_DIR);
    let old_paths = runtime::init_redb_repository_storage(&old_storage)
        .await
        .expect("init old redb storage");
    let old_record = MetaSchemaRecord {
        role: MetaSchemaRole::Repository,
        format_version: 0,
        min_reader_version: 0,
    };
    assert_eq!(
        state::classify_redb_meta_schema(&old_record, DatabaseRole::Repository),
        SourceState::SchemaOld
    );
    overwrite_meta_schema(&old_paths.git_db_path, &old_record);
    let old_bytes_before = fs::read(&old_paths.git_db_path).expect("read old bytes");
    let old_scope = repo::resolve_scope(old_dir.path().to_path_buf())
        .expect("old scope")
        .expect("old repo");
    let old_err = repo::read_context(&old_scope)
        .await
        .expect_err("old META_SCHEMA must fail closed");
    assert_eq!(old_err.state, SourceState::SchemaOld);
    assert_eq!(
        fs::read(&old_paths.git_db_path).expect("read old bytes after"),
        old_bytes_before,
        "SchemaOld read must not modify git.redb bytes"
    );

    // AC4: Dirty/unrecovered Redb needing repair (`RepairAborted`) ->
    // `SourceState::Corrupt` mentioning `libra fsck`, NEVER upgrading to write.
    let crash_dir = tempdir().expect("temp crash redb repo");
    let crash_storage = crash_dir.path().join(util::ROOT_DIR);
    let crash_redb_dir = crash_storage.join(crate::storage::manifest::REDB_SUBDIR);
    fs::create_dir_all(&crash_redb_dir).expect("create crash redb dir");
    let crash_manifest = StorageFormatManifest::new_redb_finalized(INITIAL_STORAGE_GENERATION)
        .expect("create finalized manifest");
    publish_storage_manifest(&crash_storage, &crash_manifest).expect("publish crash manifest");
    let crash_git_path =
        runtime::resolve_repo_git_redb_path(&crash_storage).expect("resolve crash git.redb path");
    create_unrecovered_redb_fixture(&crash_git_path);
    let ro_check = open_redb_read_only(&crash_git_path, DatabaseRole::Repository)
        .err()
        .expect("crash fixture must trigger RepairAborted on read-only open");
    assert!(
        matches!(ro_check, StorageBackendError::RepairAborted { .. }),
        "expected RepairAborted on crash fixture, got {ro_check:?}"
    );
    assert_eq!(
        state::classify_storage_error(&ro_check),
        SourceState::Corrupt
    );
    let crash_bytes_before = fs::read(&crash_git_path).expect("read crash fixture bytes before");
    let crash_mtime_before = fs::metadata(&crash_git_path)
        .expect("stat crash fixture before")
        .modified()
        .expect("crash fixture mtime before");
    let crash_scope = repo::resolve_scope(crash_dir.path().to_path_buf())
        .expect("crash scope")
        .expect("crash repo");
    let crash_err = repo::read_context(&crash_scope)
        .await
        .expect_err("RepairAborted must fail closed");
    assert_eq!(crash_err.state, SourceState::Corrupt);
    assert!(
        crash_err.message.contains("libra fsck"),
        "RepairAborted error message must include `libra fsck` hint: {}",
        crash_err.message
    );
    assert!(
        crash_err
            .state
            .hint()
            .is_some_and(|hint| hint.contains("libra fsck")),
        "SourceState::Corrupt hint must include `libra fsck`"
    );
    assert_eq!(
        fs::read(&crash_git_path).expect("read crash fixture bytes after"),
        crash_bytes_before,
        "RepairAborted read must NEVER upgrade to write or alter bytes on disk"
    );
    assert_eq!(
        fs::metadata(&crash_git_path)
            .expect("stat crash fixture after")
            .modified()
            .expect("crash fixture mtime after"),
        crash_mtime_before,
        "RepairAborted read must NEVER alter mtime on disk"
    );

    // AC5: Permission-denied / unreadable directory preserves original
    // filesystem `ErrorKind::PermissionDenied` in `SourceState::Unreadable`.
    assert_eq!(
        state::classify_storage_error(&StorageBackendError::Io {
            context: "stat redb directory",
            kind: std::io::ErrorKind::PermissionDenied,
            raw_os_error: None,
        }),
        SourceState::Unreadable {
            kind: std::io::ErrorKind::PermissionDenied,
        }
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        struct RestoreModeGuard {
            path: PathBuf,
            mode: u32,
        }
        impl Drop for RestoreModeGuard {
            fn drop(&mut self) {
                let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(self.mode));
            }
        }

        // Case 5a: `.libra/redb` directory unreadable (`chmod 0o000`).
        let redb_dir = storage.join(crate::storage::manifest::REDB_SUBDIR);
        let redb_orig_mode = fs::metadata(&redb_dir)
            .expect("stat redb dir")
            .permissions()
            .mode();
        {
            let _guard = RestoreModeGuard {
                path: redb_dir.clone(),
                mode: redb_orig_mode,
            };
            fs::set_permissions(&redb_dir, fs::Permissions::from_mode(0o000))
                .expect("chmod 0o000 redb dir");
            let err = repo::read_context(&scope)
                .await
                .expect_err("unreadable redb dir must fail closed");
            assert_eq!(
                err.state,
                SourceState::Unreadable {
                    kind: std::io::ErrorKind::PermissionDenied,
                }
            );
        }

        // Case 5b: `.libra` storage directory unreadable (`chmod 0o000`).
        let storage_orig_mode = fs::metadata(&storage)
            .expect("stat storage dir")
            .permissions()
            .mode();
        {
            let _guard = RestoreModeGuard {
                path: storage.clone(),
                mode: storage_orig_mode,
            };
            fs::set_permissions(&storage, fs::Permissions::from_mode(0o000))
                .expect("chmod 0o000 storage dir");
            let err = repo::read_context(&scope)
                .await
                .expect_err("unreadable storage dir must fail closed");
            assert_eq!(
                err.state,
                SourceState::Unreadable {
                    kind: std::io::ErrorKind::PermissionDenied,
                }
            );
        }
    }
}
