//! Data-layer tests for the window seam. No display is required.

use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use sea_orm::ConnectionTrait;
use serial_test::serial;
use tempfile::tempdir;

use super::{DataHandle, QUEUE_CAP, ReadOutcome, SourceState, repo, state};
use crate::{internal::db, utils::test};

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
    let before = fs::read(&db_path).expect("read database");
    let scope = repo::resolve_scope(repo_dir.path().to_path_buf())
        .expect("scope")
        .expect("repository");
    let context = repo::read_context(&scope).await.expect("context");
    let rows = super::history::read_history(&scope, context.head_oid)
        .await
        .expect("history");
    assert_eq!(rows.len(), 1, "the commit walk actually ran");
    let after = fs::read(&db_path).expect("read database");
    assert_eq!(before, after, "history reads must not modify the database");
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
