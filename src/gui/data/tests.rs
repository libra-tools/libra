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

use super::{DataHandle, QUEUE_CAP, SourceState, repo, state};
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
            Ok((
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
            Ok((SourceState::Available, None, None))
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
            Ok((SourceState::Available, None, None))
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
            Ok((SourceState::Available, None, None))
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
    let handle =
        DataHandle::spawn_with(|_| Box::pin(async { Ok((SourceState::Available, None, None)) }));
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

    let corrupt_handle =
        DataHandle::spawn_with(|_| Box::pin(async { Ok((SourceState::Corrupt, None, None)) }));
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
            Ok((
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
            Ok((SourceState::Available, None, None))
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
            Ok((SourceState::Available, None, None))
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
