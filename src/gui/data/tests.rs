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
