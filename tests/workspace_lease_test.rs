//! Agent workspace association + lease store (plan-20260714 Part C §C.8, W4).
//!
//! These are the §C.12 `lease` matrix rows for the store layer:
//! acquire/renew/release/takeover/fence, owner crash, orphan cleanup, and the
//! human "no lease at all" path. Every case drives
//! [`libra::internal::workspace::WorkspaceStore`] against a real SQLite file
//! (bootstrapped through the production connection helper, so the schema is the
//! one migration `2026072501` actually installs) and passes `now_ms` explicitly
//! — expiry is exercised by arithmetic, never by sleeping (§C.12).

use std::{path::Path, sync::Arc};

#[cfg(debug_assertions)]
use libra::internal::workspace::test_hooks;
use libra::internal::{
    db::establish_connection_with_busy_timeout,
    workspace::{
        AcquireRequest, DEFAULT_LIST_LIMIT, MAX_LIST_LIMIT, RepoIdentity, WorkspaceError,
        WorkspaceKind, WorkspaceLease, WorkspaceListQuery, WorkspaceOwnerKind, WorkspaceState,
        WorkspaceStore,
    },
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serial_test::serial;
use tempfile::TempDir;

const REPO_ID: &str = "repo-0001";
const TTL: i64 = 60_000;
const NOW: i64 = 1_800_000_000_000;

/// A bootstrapped repository database in a temporary directory.
struct TestDb {
    _dir: TempDir,
    path: std::path::PathBuf,
    conn: DatabaseConnection,
}

async fn open_db() -> TestDb {
    open_db_with_repo_id(REPO_ID).await
}

/// The store reads `libra.repoid` from the connection for every operation
/// (§C.4.1.1) — a bootstrapped database with no identity is not a repository,
/// so seed it exactly as `libra init` does.
async fn open_db_with_repo_id(repo_id: &str) -> TestDb {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("libra.db");
    std::fs::File::create(&path).expect("touch sqlite file");
    let conn = connect(&path).await;
    conn.execute(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO config_kv (key, value, encrypted) VALUES ('libra.repoid', ?, 0)",
        [repo_id.into()],
    ))
    .await
    .expect("seed repository identity");
    TestDb {
        _dir: dir,
        path,
        conn,
    }
}

/// A second, independent connection to the same file — used by the concurrency
/// cases so two writers really contend for the SQLite write lock.
async fn connect(path: &Path) -> DatabaseConnection {
    connect_with_busy_timeout(path, std::time::Duration::from_secs(30)).await
}

/// A connection whose lock patience is under the test's control: a tiny timeout
/// turns "blocked on another writer" into an observable outcome.
async fn connect_with_busy_timeout(path: &Path, busy: std::time::Duration) -> DatabaseConnection {
    establish_connection_with_busy_timeout(path.to_str().expect("utf-8 db path"), busy)
        .await
        .expect("open repository database")
}

fn workspace_dir(root: &Path, name: &str) -> std::path::PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).expect("create workspace dir");
    path
}

/// The store's failpoints are process-global, so every test that installs one
/// is `#[serial]` and holds this guard: a panic mid-test must not leave a hook
/// armed for the rest of the binary. Like the seam itself, it exists only in
/// debug builds.
#[cfg(debug_assertions)]
struct FailpointGuard;

#[cfg(debug_assertions)]
impl Drop for FailpointGuard {
    fn drop(&mut self) {
        test_hooks::set_before_write(None);
        test_hooks::set_after_write(None);
    }
}

async fn live_row_count(conn: &DatabaseConnection) -> i64 {
    let row = conn
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) FROM workspace_record WHERE state IN ('provisioning', 'active', \
             'releasing')"
                .to_string(),
        ))
        .await
        .expect("count live workspace rows")
        .expect("count row");
    row.try_get_by_index(0).expect("count value")
}

// ---------------------------------------------------------------------------
// §C.8: one linked worktree, one live lease — decided by the database
// ---------------------------------------------------------------------------

/// Two agents racing for the SAME linked worktree: exactly one acquire may
/// win, and the loser must see the stable lease-held refusal. The winner is
/// elected by the partial unique index, so this also proves the loser left no
/// row behind (a read-then-write election would publish two).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspace_lease_same_linked_worktree_single_winner() {
    let db = open_db().await;
    let path = workspace_dir(db.path.parent().expect("db parent"), "linked-a");
    let second = connect(&db.path).await;

    // A barrier, not a sleep: both tasks are inside acquire before either can
    // commit, so the race window is real and deterministic (§C.12).
    let gate = Arc::new(tokio::sync::Barrier::new(2));
    let request_a = AcquireRequest::linked("wt-1", path.clone(), "agent-a", TTL)
        .with_association(Some("task-a".to_string()), Some("session-a".to_string()));
    let request_b = AcquireRequest::linked("wt-1", path.clone(), "agent-b", TTL);

    let first_conn = db.conn.clone();
    let gate_a = gate.clone();
    let a = tokio::spawn(async move {
        gate_a.wait().await;
        WorkspaceStore::acquire(&first_conn, &request_a, NOW).await
    });
    let gate_b = gate.clone();
    let b = tokio::spawn(async move {
        gate_b.wait().await;
        WorkspaceStore::acquire(&second, &request_b, NOW).await
    });

    let (a, b) = (a.await.expect("task a"), b.await.expect("task b"));
    let winners = [&a, &b].iter().filter(|result| result.is_ok()).count();
    assert_eq!(
        winners, 1,
        "exactly one acquire may win the linked identity; got a={a:?} b={b:?}"
    );

    let loser = if a.is_err() { a } else { b };
    match loser.expect_err("one acquire must lose") {
        WorkspaceError::LeaseHeld(detail) => {
            assert!(
                !detail.path_conflict,
                "a linked-identity collision must not be reported as a path alias: {detail:?}"
            );
            assert!(
                detail.identity.contains("wt-1"),
                "the refusal names the contended worktree: {detail:?}"
            );
        }
        other => panic!("expected a lease-held refusal, got {other:?}"),
    }

    assert_eq!(
        live_row_count(&db.conn).await,
        1,
        "the losing acquire must not leave a second live record behind"
    );
}

/// The same election with the contention PROVEN, not hoped for.
///
/// A barrier or a failpoint can only show that the loser *entered* `acquire`;
/// it cannot show that its write attempt overlapped the winner's lock, because
/// the scheduler is free to commit the winner first. So the contention is
/// established by consequence instead: the contender runs against a connection
/// whose busy timeout is deliberately tiny, and while the winner's insert sits
/// uncommitted it MUST come back with a lock error — an outcome only reachable
/// by actually attempting the write while the lock is held.
///
/// A third connection then shows the precondition that would defeat an
/// APPLICATION-level election: the winner's row is invisible, so a
/// "SELECT then INSERT if absent" store would happily elect the contender. Once
/// the winner commits, the retry meets the unique index and is refused — that
/// index, not any application check, is the arbiter.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_acquire_overlap_is_decided_by_the_unique_index() {
    use sea_orm::TransactionTrait;

    let db = open_db().await;
    let path = workspace_dir(db.path.parent().expect("db parent"), "linked-a");
    let impatient = connect_with_busy_timeout(&db.path, std::time::Duration::from_millis(50)).await;
    let observer = connect(&db.path).await;

    // A: transaction open, row inserted, NOT yet committed. The identity is
    // resolved BEFORE the transaction, exactly as `acquire` does it — a read
    // inside a write transaction is the shape that deadlocks.
    let identity = RepoIdentity::resolve(&db.conn)
        .await
        .expect("repository identity");
    let txn = db.conn.begin().await.expect("begin A");
    let winner = WorkspaceStore::acquire_with_conn(
        &txn,
        &identity,
        &AcquireRequest::linked("wt-1", path.clone(), "agent-a", TTL),
        NOW,
    )
    .await
    .expect("A inserts first and owns the election");

    assert!(
        WorkspaceStore::find_live_linked_with_conn(&observer, "wt-1")
            .await
            .expect("lookup")
            .is_none(),
        "A's uncommitted insert must be invisible to other connections — an \
         application-level election would elect B here"
    );

    // B reaches its write while A still holds the lock: with a 50ms busy
    // timeout the only way to get here is to have attempted it.
    let request_b = AcquireRequest::linked("wt-1", path, "agent-b", TTL);
    let blocked = WorkspaceStore::acquire(&impatient, &request_b, NOW)
        .await
        .expect_err("B's write must contend with A's uncommitted lock");
    match &blocked {
        WorkspaceError::WriteFailed(message) => assert!(
            message.contains("locked") || message.contains("busy"),
            "expected a lock-contention failure, got {message}"
        ),
        other => panic!("expected B to block on A's write lock, got {other:?}"),
    }

    txn.commit().await.expect("publish A");

    // Same contender, same request, now that A is published: the index refuses.
    let error = WorkspaceStore::acquire(&impatient, &request_b, NOW)
        .await
        .expect_err("B must lose the election once A's row is visible");
    assert!(
        matches!(error, WorkspaceError::LeaseHeld(_)),
        "an uncontended retry must surface the stable lease-held refusal: {error:?}"
    );

    assert_eq!(
        live_row_count(&db.conn).await,
        1,
        "one directory, one live record"
    );
    assert_eq!(
        WorkspaceStore::find_live_linked_with_conn(&db.conn, "wt-1")
            .await
            .expect("lookup")
            .expect("record")
            .workspace_id,
        winner.workspace_id
    );
}

/// The same guarantee, sequentially, with the full refusal surface asserted:
/// the holder is named, the stable code is the Agent-domain lease code (§C.13
/// — never a branch-conflict code), and the hint points at doctor recovery.
#[tokio::test]
async fn linked_identity_conflict_reports_holder_and_stable_code() {
    let db = open_db().await;
    let path = workspace_dir(db.path.parent().expect("db parent"), "linked-a");

    let held = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-1", path.clone(), "agent-a", TTL),
        NOW,
    )
    .await
    .expect("first acquire");

    let error = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-1", path, "agent-b", TTL),
        NOW,
    )
    .await
    .expect_err("second acquire must be refused");

    assert_eq!(
        error.stable_code().as_str(),
        "LBR-AGENT-022",
        "lease conflicts ride the Agent domain, not the branch-conflict codes"
    );
    match &error {
        WorkspaceError::LeaseHeld(detail) => {
            assert_eq!(detail.holder.as_deref(), Some("agent-a"));
            assert_eq!(
                detail.workspace_id.as_deref(),
                Some(held.workspace_id.as_str())
            );
            assert_eq!(detail.state, Some(WorkspaceState::Active));
        }
        other => panic!("expected a lease-held refusal, got {other:?}"),
    }
    let rendered = libra::utils::error::CliError::from(error).to_string();
    assert!(
        rendered.contains("agent-a"),
        "the user-facing message names the holder: {rendered}"
    );
}

/// A different worktree in the same repository, and the same worktree id in a
/// different repository, are independent identities — the index must not
/// over-serialize unrelated agents.
#[tokio::test]
async fn distinct_identities_do_not_contend() {
    let db = open_db().await;
    let root = db.path.parent().expect("db parent");

    WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-1", workspace_dir(root, "a"), "agent-a", TTL),
        NOW,
    )
    .await
    .expect("first worktree");
    WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-2", workspace_dir(root, "b"), "agent-b", TTL),
        NOW,
    )
    .await
    .expect("second worktree in the same repo");
    assert_eq!(live_row_count(&db.conn).await, 2);

    // A different repository is a different database with its own identity —
    // the same worktree id there contends with nothing here.
    let other = open_db_with_repo_id("repo-0002").await;
    WorkspaceStore::acquire(
        &other.conn,
        &AcquireRequest::linked(
            "wt-1",
            workspace_dir(other.path.parent().expect("db parent"), "c"),
            "agent-c",
            TTL,
        ),
        NOW,
    )
    .await
    .expect("same worktree id in another repository");
    assert_eq!(live_row_count(&other.conn).await, 1);

    // The identity is never taken from the caller: a repository whose
    // `libra.repoid` is missing cannot register workspaces at all, instead of
    // silently minting rows under an empty identity that both unique indexes
    // would then fail to separate.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("libra.db");
    std::fs::File::create(&path).expect("touch sqlite file");
    let anonymous = connect(&path).await;
    let error = WorkspaceStore::acquire(
        &anonymous,
        &AcquireRequest::linked("wt-1", workspace_dir(dir.path(), "d"), "agent-d", TTL),
        NOW,
    )
    .await
    .expect_err("a database without a repository identity cannot host workspaces");
    assert!(
        matches!(error, WorkspaceError::Corrupt(_)),
        "expected a corrupt-identity refusal, got {error:?}"
    );
}

/// The identity the WRITE statements resolve in SQL and the identity the READ
/// path resolves in Rust must be the same value in every configuration —
/// including a repository whose `libra.repoid` was rewritten (config rows are
/// append-only, newest id wins) and one whose newest value is blank. A
/// divergence would let writes land under a superseded identity that no read
/// or unique index would ever match up with.
#[tokio::test]
async fn write_and_read_paths_resolve_the_same_repository_identity() {
    let db = open_db().await;
    let root = db.path.parent().expect("db parent");

    // A rewritten identity: the newest row wins for reads AND writes.
    db.conn
        .execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO config_kv (key, value, encrypted) VALUES ('libra.repoid', \
             'repo-rewritten', 0)"
                .to_string(),
        ))
        .await
        .expect("rewrite identity");
    let lease = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::task(
            WorkspaceKind::TaskCopy,
            workspace_dir(root, "task"),
            "agent-a",
            TTL,
        ),
        NOW,
    )
    .await
    .expect("acquire under the rewritten identity");
    let record = WorkspaceStore::get_with_conn(&db.conn, &lease.workspace_id)
        .await
        .expect("read")
        .expect("the repo-scoped read finds the row the write just made");
    assert_eq!(record.repo_id, "repo-rewritten");

    // A blank newest value means "no identity" on BOTH paths — the write must
    // not silently fall back to the older, superseded row.
    db.conn
        .execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO config_kv (key, value, encrypted) VALUES ('libra.repoid', '   ', 0)"
                .to_string(),
        ))
        .await
        .expect("blank identity");
    let error = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::task(
            WorkspaceKind::TaskCopy,
            workspace_dir(root, "task-2"),
            "agent-b",
            TTL,
        ),
        NOW,
    )
    .await
    .expect_err("a blank identity must refuse the write, not use a stale one");
    assert!(
        matches!(error, WorkspaceError::Corrupt(_)),
        "expected a corrupt-identity refusal, got {error:?}"
    );
    assert!(
        WorkspaceStore::get_with_conn(&db.conn, &lease.workspace_id)
            .await
            .is_err(),
        "reads report the same missing identity rather than a different one"
    );
}

/// A padded or whitespace-only `libra.repoid` is refused on BOTH paths rather
/// than normalized. SQLite's `TRIM` strips only spaces while Rust's
/// `str::trim` strips every Unicode whitespace character, so any normalization
/// split between SQL and Rust would eventually let a write land under an
/// identity no read could match — a live lease invisible to list/doctor while
/// still blocking the down migration.
#[tokio::test]
async fn whitespace_padded_repository_identity_is_refused_not_normalized() {
    for identity in ["\t", "   ", " repo-0001", "repo-0001\n"] {
        let db = open_db_with_repo_id(identity).await;
        let root = db.path.parent().expect("db parent");
        let error = WorkspaceStore::acquire(
            &db.conn,
            &AcquireRequest::task(
                WorkspaceKind::TaskCopy,
                workspace_dir(root, "task"),
                "agent-a",
                TTL,
            ),
            NOW,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, WorkspaceError::Corrupt(_)),
            "identity {identity:?} must be refused as corrupt metadata, got {error:?}"
        );
        assert_eq!(live_row_count(&db.conn).await, 0);
        assert!(
            RepoIdentity::resolve(&db.conn).await.is_err(),
            "the read path must refuse identity {identity:?} too"
        );
    }
}

/// Rewriting `libra.repoid` while workspaces are still live would reopen the
/// uniqueness namespace — both partial indexes are prefixed by `repo_id`, so
/// the same worktree and directory could be claimed a second time while the old
/// rows became invisible to every repo-scoped listing and unreclaimable. New
/// registrations therefore fail closed until those rows are adopted or
/// released.
#[tokio::test]
async fn identity_rewrite_with_live_workspaces_fails_closed() {
    let db = open_db().await;
    let root = db.path.parent().expect("db parent");
    let path = workspace_dir(root, "linked-a");

    let lease = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-1", path.clone(), "agent-a", TTL),
        NOW,
    )
    .await
    .expect("acquire under the original identity");

    db.conn
        .execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO config_kv (key, value, encrypted) VALUES ('libra.repoid', 'repo-new', 0)"
                .to_string(),
        ))
        .await
        .expect("rewrite identity");

    for request in [
        // The very same worktree and directory...
        AcquireRequest::linked("wt-1", path.clone(), "agent-b", TTL),
        // ...and an unrelated one: nothing may be registered while rows of a
        // previous identity are unsettled.
        AcquireRequest::task(
            WorkspaceKind::TaskCopy,
            workspace_dir(root, "task"),
            "agent-c",
            TTL,
        ),
    ] {
        let error = WorkspaceStore::acquire(&db.conn, &request, NOW + 1)
            .await
            .expect_err("a rewritten identity must not reopen the namespace");
        assert!(
            matches!(error, WorkspaceError::Corrupt(_)),
            "expected a fail-closed identity refusal, got {error:?}"
        );
    }
    assert_eq!(
        live_row_count(&db.conn).await,
        1,
        "no second live record may appear for the same worktree"
    );

    // The stranded row is reachable through the recovery entry point (nothing
    // else can see it — every other read is scoped to the current identity),
    // and adopting it re-homes it onto the current identity WITHOUT changing
    // who holds it.
    let stranded = WorkspaceStore::foreign_identity_records_with_conn(&db.conn, None, None)
        .await
        .expect("list workspaces of the previous identity");
    assert_eq!(stranded.items.len(), 1);
    assert_eq!(stranded.next_cursor, None);
    assert_eq!(stranded.items[0].workspace_id, lease.workspace_id);
    assert_eq!(stranded.items[0].repo_id, REPO_ID);
    assert!(
        WorkspaceStore::get_with_conn(&db.conn, &lease.workspace_id)
            .await
            .expect("read")
            .is_none(),
        "a foreign-identity row is invisible to the repo-scoped read until adopted"
    );

    let identity = RepoIdentity::resolve(&db.conn)
        .await
        .expect("current identity");
    WorkspaceStore::adopt_foreign_identity_with_conn(
        &db.conn,
        &identity,
        &lease.workspace_id,
        NOW + 3,
    )
    .await
    .expect("doctor adopts the stranded record");

    let adopted = WorkspaceStore::get_with_conn(&db.conn, &lease.workspace_id)
        .await
        .expect("read")
        .expect("the adopted record is visible under the current identity");
    assert_eq!(adopted.repo_id, "repo-new");
    assert_eq!(adopted.lease_owner.as_deref(), Some("agent-a"));
    assert_eq!(adopted.lease_fence, lease.fence);
    assert!(
        WorkspaceStore::foreign_identity_records_with_conn(&db.conn, None, None)
            .await
            .expect("list")
            .items
            .is_empty()
    );

    // Acquire is still refused — but now by the ordinary lease guard, because
    // the adopted record holds the identity. Releasing it frees the namespace.
    let error = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-1", path.clone(), "agent-b", TTL),
        NOW + 4,
    )
    .await
    .expect_err("the adopted record still holds its lease");
    assert!(matches!(error, WorkspaceError::LeaseHeld(_)), "{error:?}");

    WorkspaceStore::release_with_conn(&db.conn, &lease, NOW + 5)
        .await
        .expect("the original owner's token still works after adoption");
    WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-1", path, "agent-b", TTL),
        NOW + 6,
    )
    .await
    .expect("the namespace is usable once the previous identity's rows are settled");
}

/// A takeover must re-check the identity too, not just match the one it was
/// handed: if `libra.repoid` is rewritten between resolve and update, reviving
/// the row under the old identity would make it live again yet invisible to
/// every repo-scoped read and unreachable by any later recovery.
#[tokio::test]
async fn reclaim_refuses_after_the_identity_changes_underneath_it() {
    let db = open_db().await;
    let path = workspace_dir(db.path.parent().expect("db parent"), "linked-a");

    let stale = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-1", path, "agent-a", TTL),
        NOW,
    )
    .await
    .expect("acquire");
    let identity = RepoIdentity::resolve(&db.conn)
        .await
        .expect("identity resolved by the doctor before the rewrite");

    db.conn
        .execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO config_kv (key, value, encrypted) VALUES ('libra.repoid', 'repo-new', 0)"
                .to_string(),
        ))
        .await
        .expect("rewrite identity");

    let error = WorkspaceStore::reclaim_expired_with_conn(
        &db.conn,
        &identity,
        &stale.workspace_id,
        "doctor",
        TTL,
        NOW + TTL + 1,
    )
    .await
    .expect_err("a takeover under a superseded identity must be refused");
    assert!(
        matches!(error, WorkspaceError::Corrupt(_)),
        "expected an identity-drift refusal, got {error:?}"
    );

    // The row is untouched: still fence 1, still owned by the crashed agent,
    // and still reachable through the recovery entry point.
    let stranded = WorkspaceStore::foreign_identity_records_with_conn(&db.conn, None, None)
        .await
        .expect("list");
    assert_eq!(stranded.items.len(), 1);
    assert_eq!(stranded.items[0].lease_fence, 1);
    assert_eq!(stranded.items[0].lease_owner.as_deref(), Some("agent-a"));
}

/// An `active` record claims the workspace is usable, so `acquire` refuses to
/// publish one for a directory that does not exist — otherwise a linked
/// acquire could occupy a worktree's lease identity with a phantom path and
/// lock the real workspace out. A missing leaf is only legal for
/// `provisioning`, which exists precisely to cover materialization.
#[tokio::test]
async fn active_publication_requires_an_existing_directory() {
    let db = open_db().await;
    let root = db.path.parent().expect("db parent");
    let missing = root.join("not-created-yet");

    let error = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-1", missing.clone(), "agent-a", TTL),
        NOW,
    )
    .await
    .expect_err("an active record must not point at a missing directory");
    assert!(
        matches!(error, WorkspaceError::Invalid(_)),
        "expected a validation refusal, got {error:?}"
    );
    assert_eq!(live_row_count(&db.conn).await, 0);

    // A file is not a workspace either.
    std::fs::write(root.join("a-file"), b"x").expect("write file");
    assert!(matches!(
        WorkspaceStore::acquire(
            &db.conn,
            &AcquireRequest::linked("wt-1", root.join("a-file"), "agent-a", TTL),
            NOW,
        )
        .await
        .expect_err("a regular file is not a workspace directory"),
        WorkspaceError::Invalid(_)
    ));

    // Provisioning may name a directory that does not exist yet; activation
    // happens after materialization.
    let lease = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::task(WorkspaceKind::TaskCopy, missing.clone(), "agent-a", TTL),
        NOW,
    )
    .await
    .expect("provisioning admits a not-yet-created directory");
    std::fs::create_dir(&missing).expect("materialize");
    WorkspaceStore::activate_with_conn(&db.conn, &lease, NOW + 1)
        .await
        .expect("activate once the directory exists");
}

// ---------------------------------------------------------------------------
// §C.8: the canonical path is unique across live workspaces
// ---------------------------------------------------------------------------

/// A path ALIAS — `.`/`..` detours, a trailing separator, or (on Unix) a
/// symlinked parent — must not be able to claim a second live record for one
/// directory. Without this, two workspaces would legally double-write the same
/// files.
#[tokio::test]
async fn workspace_lease_canonical_path_alias_rejected() {
    let db = open_db().await;
    let root = db.path.parent().expect("db parent");
    let path = workspace_dir(root, "task-copy");

    WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::task(WorkspaceKind::TaskCopy, path.clone(), "agent-a", TTL),
        NOW,
    )
    .await
    .expect("first acquire");

    let mut aliases = vec![
        root.join("task-copy")
            .join(".")
            .join("..")
            .join("task-copy"),
        std::path::PathBuf::from(format!("{}{}", path.display(), std::path::MAIN_SEPARATOR)),
    ];
    #[cfg(unix)]
    {
        let link = root.join("task-link");
        std::os::unix::fs::symlink(&path, &link).expect("symlink workspace");
        aliases.push(link);
    }

    for alias in aliases {
        let outcome = WorkspaceStore::acquire(
            &db.conn,
            &AcquireRequest::task(WorkspaceKind::TaskCopy, alias.clone(), "agent-b", TTL),
            NOW,
        )
        .await;
        match outcome {
            Ok(lease) => panic!(
                "alias {} was accepted as workspace {}",
                alias.display(),
                lease.workspace_id
            ),
            Err(WorkspaceError::LeaseHeld(detail)) => assert!(
                detail.path_conflict,
                "alias {} must be refused as a path conflict: {detail:?}",
                alias.display()
            ),
            Err(other) => panic!(
                "alias {} must be refused as a held lease, got {other:?}",
                alias.display()
            ),
        }
    }

    assert_eq!(
        live_row_count(&db.conn).await,
        1,
        "no alias may add a second live record for one directory"
    );
}

/// A workspace may be registered before its directory exists, but only when the
/// PARENT resolves — otherwise `/root/link/ws` and `/root/target/ws` (with
/// `link` a symlink to a not-yet-created `target`) would store two different
/// strings and both claim the single directory that materialization creates.
#[cfg(unix)]
#[tokio::test]
async fn unmaterialized_paths_behind_a_dangling_symlink_are_refused() {
    let db = open_db().await;
    let root = db.path.parent().expect("db parent");
    let target = root.join("not-yet");
    let link = root.join("link");
    std::os::unix::fs::symlink(&target, &link).expect("dangling symlink");

    // The dangling link itself cannot be registered...
    let error = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::task(WorkspaceKind::TaskCopy, link.clone(), "agent-a", TTL),
        NOW,
    )
    .await
    .expect_err("a dangling symlink has no provable identity");
    assert!(matches!(error, WorkspaceError::Invalid(_)), "{error:?}");

    // ...nor a workspace *inside* it, whose parent cannot be resolved.
    let error = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::task(WorkspaceKind::TaskCopy, link.join("ws"), "agent-a", TTL),
        NOW,
    )
    .await
    .expect_err("an unresolvable parent must fail closed");
    assert!(matches!(error, WorkspaceError::Invalid(_)), "{error:?}");
    assert_eq!(live_row_count(&db.conn).await, 0);

    // Once the target exists, both spellings collapse onto one canonical path,
    // so the second claim is refused as an alias rather than double-claiming.
    std::fs::create_dir(&target).expect("materialize target");
    WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::task(WorkspaceKind::TaskCopy, link.join("ws"), "agent-a", TTL),
        NOW,
    )
    .await
    .expect("a resolvable parent admits a not-yet-created leaf");
    let error = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::task(WorkspaceKind::TaskCopy, target.join("ws"), "agent-b", TTL),
        NOW,
    )
    .await
    .expect_err("the same directory through its real name is an alias");
    match error {
        WorkspaceError::LeaseHeld(detail) => assert!(detail.path_conflict, "{detail:?}"),
        other => panic!("expected a path conflict, got {other:?}"),
    }
    assert_eq!(live_row_count(&db.conn).await, 1);
}

/// `..` must be resolved by the KERNEL, not lexically: with `/root/a/link`
/// pointing at `/root/b/sub`, `/root/a/link/../ws` really is `/root/b/ws`,
/// while a lexical pass would pop `link` and store `/root/a/ws` — letting the
/// same directory be claimed twice.
#[cfg(unix)]
#[tokio::test]
async fn symlink_parent_traversal_aliases_are_rejected() {
    let db = open_db().await;
    let root = db.path.parent().expect("db parent");
    std::fs::create_dir_all(root.join("a")).expect("mkdir a");
    let sub = root.join("b").join("sub");
    std::fs::create_dir_all(&sub).expect("mkdir b/sub");
    std::os::unix::fs::symlink(&sub, root.join("a").join("link")).expect("symlink");
    let real = workspace_dir(&root.join("b"), "ws");

    let lease = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::task(WorkspaceKind::TaskCopy, real.clone(), "agent-a", TTL),
        NOW,
    )
    .await
    .expect("acquire the real directory");

    let through_symlink = root.join("a").join("link").join("..").join("ws");
    let error = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::task(
            WorkspaceKind::TaskCopy,
            through_symlink.clone(),
            "agent-b",
            TTL,
        ),
        NOW,
    )
    .await
    .expect_err("the same directory reached through a symlinked parent is an alias");
    match error {
        WorkspaceError::LeaseHeld(detail) => assert!(detail.path_conflict, "{detail:?}"),
        other => panic!("expected a path conflict, got {other:?}"),
    }

    // The lookup resolves the alias onto the same record, too.
    assert_eq!(
        WorkspaceStore::find_live_by_path_with_conn(&db.conn, &through_symlink)
            .await
            .expect("lookup")
            .expect("alias resolves")
            .workspace_id,
        lease.workspace_id
    );
    assert_eq!(live_row_count(&db.conn).await, 1);
}

/// The alias guard also holds across KINDS: a linked workspace and a task copy
/// pointed at the same directory are still one directory.
#[tokio::test]
async fn path_conflict_is_reported_across_kinds() {
    let db = open_db().await;
    let root = db.path.parent().expect("db parent");
    let path = workspace_dir(root, "shared");

    WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-1", path.clone(), "agent-a", TTL),
        NOW,
    )
    .await
    .expect("linked acquire");

    let error = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::task(
            WorkspaceKind::TaskCopy,
            root.join("shared").join("."),
            "agent-b",
            TTL,
        ),
        NOW,
    )
    .await
    .expect_err("a task workspace must not claim a leased directory");

    match &error {
        WorkspaceError::LeaseHeld(detail) => {
            assert!(
                detail.path_conflict,
                "a directory collision must be reported as a path conflict: {detail:?}"
            );
            assert_eq!(detail.holder.as_deref(), Some("agent-a"));
        }
        other => panic!("expected a lease-held refusal, got {other:?}"),
    }
    assert_eq!(error.stable_code().as_str(), "LBR-AGENT-022");
}

/// Lookups accept any spelling too — the store canonicalizes on read as well as
/// on write, so a caller holding an alias still finds the live record.
#[tokio::test]
async fn live_lookup_by_path_accepts_aliases() {
    let db = open_db().await;
    let root = db.path.parent().expect("db parent");
    let path = workspace_dir(root, "task-copy");

    let lease = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::task(WorkspaceKind::TaskCopy, path.clone(), "agent-a", TTL),
        NOW,
    )
    .await
    .expect("acquire");

    let found = WorkspaceStore::find_live_by_path_with_conn(
        &db.conn,
        &root
            .join("task-copy")
            .join(".")
            .join("..")
            .join("task-copy"),
    )
    .await
    .expect("lookup")
    .expect("alias resolves to the live record");
    assert_eq!(found.workspace_id, lease.workspace_id);
}

// ---------------------------------------------------------------------------
// §C.8: owner + monotonic fence conditional writes
// ---------------------------------------------------------------------------

/// The core fence guarantee: after a doctor/scavenger reclaim mints a higher
/// fence, the previous owner can no longer renew, activate, or RELEASE — a
/// stale release would hand the directory to a third party while the new owner
/// is still writing to it.
#[tokio::test]
async fn workspace_lease_stale_owner_cannot_release_new_fence() {
    let db = open_db().await;
    let path = workspace_dir(db.path.parent().expect("db parent"), "linked-a");

    let stale = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-1", path, "agent-a", TTL),
        NOW,
    )
    .await
    .expect("first acquire");
    assert_eq!(stale.fence, 1);

    // The owner goes dark; its lease deadline passes.
    let after_expiry = NOW + TTL + 1;
    assert!(stale.is_expired(after_expiry));

    let reclaimed = WorkspaceStore::reclaim_expired(
        &db.conn,
        &stale.workspace_id,
        "doctor-b",
        TTL,
        after_expiry,
    )
    .await
    .expect("expired lease is reclaimable");
    assert_eq!(reclaimed.fence, 2, "a takeover must mint a higher fence");
    assert_eq!(reclaimed.workspace_id, stale.workspace_id);

    for attempt in [
        WorkspaceStore::release_with_conn(&db.conn, &stale, after_expiry)
            .await
            .err(),
        WorkspaceStore::renew_with_conn(&db.conn, &stale, TTL, after_expiry)
            .await
            .err(),
        WorkspaceStore::begin_release_with_conn(&db.conn, &stale, after_expiry)
            .await
            .err(),
        WorkspaceStore::activate_with_conn(&db.conn, &stale, after_expiry)
            .await
            .err(),
    ] {
        let error = attempt.expect("a stale owner's lease mutation must be refused");
        assert!(
            matches!(error, WorkspaceError::LeaseLost { .. }),
            "expected a stale-fence refusal, got {error:?}"
        );
        assert_eq!(error.stable_code().as_str(), "LBR-AGENT-023");
    }

    // The record still belongs to the new owner, untouched by the stale calls.
    let record = WorkspaceStore::get_with_conn(&db.conn, &stale.workspace_id)
        .await
        .expect("read record")
        .expect("record still exists");
    assert_eq!(record.lease_owner.as_deref(), Some("doctor-b"));
    assert_eq!(record.lease_fence, 2);
    assert_eq!(record.state, WorkspaceState::Active);

    // ...and the new owner can still release it normally.
    WorkspaceStore::release_with_conn(&db.conn, &reclaimed, after_expiry + 1)
        .await
        .expect("the current owner releases its own lease");
}

/// A lease that is merely expired is NOT free for the taking: acquire never
/// steals, so a paused owner keeps its workspace until an explicit reclaim.
#[tokio::test]
async fn expired_lease_is_not_implicitly_stolen_by_acquire() {
    let db = open_db().await;
    let path = workspace_dir(db.path.parent().expect("db parent"), "linked-a");

    let lease = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-1", path.clone(), "agent-a", TTL),
        NOW,
    )
    .await
    .expect("acquire");

    let after_expiry = NOW + TTL + 1;
    let error = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-1", path, "agent-b", TTL),
        after_expiry,
    )
    .await
    .expect_err("an expired lease still refuses a plain acquire");
    assert!(matches!(error, WorkspaceError::LeaseHeld(_)));

    // The original owner may still renew — nobody took it over.
    let renewed = WorkspaceStore::renew_with_conn(&db.conn, &lease, TTL, after_expiry)
        .await
        .expect("the owner of an unreclaimed lease can renew it");
    assert_eq!(
        renewed.fence, lease.fence,
        "renew must not mint a new fence"
    );
    assert_eq!(renewed.expires_at, Some(after_expiry + TTL));
}

/// Reclaim refuses while the lease is still inside its deadline — the doctor
/// path is for confirmed-dead owners, not a `lease steal` back door (§C.8).
#[tokio::test]
async fn reclaim_refuses_a_live_lease() {
    let db = open_db().await;
    let path = workspace_dir(db.path.parent().expect("db parent"), "linked-a");

    let lease = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-1", path, "agent-a", TTL),
        NOW,
    )
    .await
    .expect("acquire");

    let error =
        WorkspaceStore::reclaim_expired(&db.conn, &lease.workspace_id, "doctor-b", TTL, NOW + 1)
            .await
            .expect_err("a live lease must not be reclaimable");
    assert!(matches!(error, WorkspaceError::LeaseHeld(_)));
    assert_eq!(error.stable_code().as_str(), "LBR-AGENT-022");

    let record = WorkspaceStore::get_with_conn(&db.conn, &lease.workspace_id)
        .await
        .expect("read record")
        .expect("record");
    assert_eq!(record.lease_owner.as_deref(), Some("agent-a"));
    assert_eq!(
        record.lease_fence, 1,
        "a refused reclaim must not bump the fence"
    );
}

/// Two doctors taking over the same lease, with the window CONTROLLED: the
/// first takeover is held open in an uncommitted transaction while the second
/// enters `reclaim` and blocks on its write lock, then the first commits and
/// the second proceeds.
///
/// The point is what each caller is handed back. Both takeovers succeed here,
/// so a reclaim that read its new fence back with a separate SELECT could
/// return the OTHER doctor's fence — and then release a lease it does not own.
/// Reporting the fence from the same statement that writes it is what makes
/// `doctor-1` see 2 and `doctor-2` see 3, every time.
#[cfg(debug_assertions)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial(workspace_failpoints)]
async fn concurrent_reclaims_hand_out_only_the_fence_they_wrote() {
    use std::sync::Arc as StdArc;

    use sea_orm::TransactionTrait;

    let _failpoints = FailpointGuard;

    let db = open_db().await;
    let path = workspace_dir(db.path.parent().expect("db parent"), "linked-a");
    let second = connect(&db.path).await;

    let stale = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-1", path, "agent-a", TTL),
        NOW,
    )
    .await
    .expect("acquire");

    // Doctor 1 takes over in an OPEN transaction: the row is updated and its
    // fence already reported, but nothing is published yet.
    let late = NOW + TTL + 1;
    let identity = RepoIdentity::resolve(&db.conn)
        .await
        .expect("repository identity");
    let txn = db.conn.begin().await.expect("begin doctor-1");
    let first = WorkspaceStore::reclaim_expired_with_conn(
        &txn,
        &identity,
        &stale.workspace_id,
        "doctor-1",
        TTL,
        late,
    )
    .await
    .expect("doctor-1 takes over the expired lease");
    assert_eq!(first.fence, 2, "the first takeover mints fence 2");

    // Doctor 2 enters `reclaim` (proven by the failpoint) with a clock past
    // the deadline doctor 1 just wrote, and blocks on the write lock.
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let entered_tx = StdArc::new(tokio::sync::Mutex::new(Some(entered_tx)));
    test_hooks::set_before_write(Some(StdArc::new(move || {
        let entered_tx = entered_tx.clone();
        Box::pin(async move {
            let signal = entered_tx.lock().await.take();
            if let Some(tx) = signal {
                let _ = tx.send(());
            }
        })
    })));

    let later = late + TTL + 1;
    let workspace_id = stale.workspace_id.clone();
    let second_task = tokio::spawn(async move {
        WorkspaceStore::reclaim_expired(&second, &workspace_id, "doctor-2", TTL, later).await
    });
    entered_rx
        .await
        .expect("doctor-2 reports from inside reclaim");

    txn.commit().await.expect("publish doctor-1's takeover");
    let second_lease = second_task
        .await
        .expect("doctor-2 task")
        .expect("doctor-2 takes over once the deadline has passed again");

    assert_eq!(
        second_lease.fence, 3,
        "the second takeover mints its own fence, and the first was told 2 — neither may be \
         handed the other's"
    );
    let record = WorkspaceStore::get_with_conn(&db.conn, &stale.workspace_id)
        .await
        .expect("read")
        .expect("record");
    assert_eq!(record.lease_fence, 3);
    assert_eq!(record.lease_owner.as_deref(), Some("doctor-2"));

    // Every superseded holder — including the original owner — is locked out.
    for superseded in [&stale, &first] {
        assert!(matches!(
            WorkspaceStore::release_with_conn(&db.conn, superseded, later + 1)
                .await
                .expect_err("a superseded fence cannot release the current lease"),
            WorkspaceError::LeaseLost { .. }
        ));
    }
    WorkspaceStore::release_with_conn(&db.conn, &second_lease, later + 2)
        .await
        .expect("the current owner releases its own lease");
}

/// The regression the `RETURNING` takeover exists to prevent, exercised on a
/// BARE connection: doctor-1's statement lands and commits (autocommit), then
/// the post-write failpoint holds it in exactly the window where an
/// `UPDATE`-then-`SELECT` implementation would re-read the row — and while it
/// is parked, doctor-2 takes the lease over with a higher fence.
///
/// Reporting the fence from the same statement that wrote it makes doctor-1
/// return 2. An implementation that read the fence back here would return 3 and
/// then be able to release doctor-2's lease, which the last assertions pin.
#[cfg(debug_assertions)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial(workspace_failpoints)]
async fn takeover_reports_its_own_fence_even_if_another_lands_immediately_after() {
    use std::sync::Arc as StdArc;

    let _failpoints = FailpointGuard;
    let db = open_db().await;
    let path = workspace_dir(db.path.parent().expect("db parent"), "linked-a");
    let second = connect(&db.path).await;

    let stale = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-1", path, "agent-a", TTL),
        NOW,
    )
    .await
    .expect("acquire");

    let late = NOW + TTL + 1;
    let later = late + TTL + 1;

    // doctor-1 parks AFTER its takeover statement, before it returns.
    let (parked_tx, parked_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
    let parked_tx = StdArc::new(tokio::sync::Mutex::new(Some(parked_tx)));
    let resume_rx = StdArc::new(tokio::sync::Mutex::new(Some(resume_rx)));
    test_hooks::set_after_write(Some(StdArc::new(move || {
        let parked_tx = parked_tx.clone();
        let resume_rx = resume_rx.clone();
        Box::pin(async move {
            let signal = parked_tx.lock().await.take();
            if let Some(tx) = signal {
                let _ = tx.send(());
                let wait = resume_rx.lock().await.take();
                if let Some(rx) = wait {
                    let _ = rx.await;
                }
            }
        })
    })));

    // Deliberately the bare-connection form: the statement autocommits, so
    // doctor-1 holds NO lock while it is parked and doctor-2 can really land.
    let identity = RepoIdentity::resolve(&db.conn)
        .await
        .expect("repository identity");
    let first_conn = db.conn.clone();
    let first_id = stale.workspace_id.clone();
    let first_task = tokio::spawn(async move {
        WorkspaceStore::reclaim_expired_with_conn(
            &first_conn,
            &identity,
            &first_id,
            "doctor-1",
            TTL,
            late,
        )
        .await
    });
    parked_rx
        .await
        .expect("doctor-1 parks in the post-write window");

    // While doctor-1 is parked, doctor-2 takes the lease over.
    let second_lease =
        WorkspaceStore::reclaim_expired(&second, &stale.workspace_id, "doctor-2", TTL, later)
            .await
            .expect("doctor-2 takes over the now-expired lease");
    assert_eq!(second_lease.fence, 3);

    let _ = resume_tx.send(());
    let first = first_task
        .await
        .expect("doctor-1 task")
        .expect("doctor-1's takeover already landed");
    assert_eq!(
        first.fence, 2,
        "a takeover must report the fence IT wrote, never the one that \
         overtook it while it was returning"
    );

    // ...and that stale fence cannot release doctor-2's lease.
    assert!(matches!(
        WorkspaceStore::release_with_conn(&db.conn, &first, later + 1)
            .await
            .expect_err("doctor-1's superseded fence must not release doctor-2's lease"),
        WorkspaceError::LeaseLost { .. }
    ));
    let record = WorkspaceStore::get_with_conn(&db.conn, &stale.workspace_id)
        .await
        .expect("read")
        .expect("record");
    assert_eq!(record.lease_fence, 3);
    assert_eq!(record.lease_owner.as_deref(), Some("doctor-2"));
}

/// Fences are monotonic across repeated takeovers, so an owner two generations
/// back is refused just as firmly as the previous one.
#[tokio::test]
async fn fences_are_monotonic_across_repeated_takeovers() {
    let db = open_db().await;
    let path = workspace_dir(db.path.parent().expect("db parent"), "linked-a");

    let first = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-1", path, "agent-a", TTL),
        NOW,
    )
    .await
    .expect("acquire");

    let second =
        WorkspaceStore::reclaim_expired(&db.conn, &first.workspace_id, "agent-b", TTL, NOW + TTL)
            .await
            .expect("first takeover");
    let third = WorkspaceStore::reclaim_expired(
        &db.conn,
        &first.workspace_id,
        "agent-c",
        TTL,
        NOW + 2 * TTL,
    )
    .await
    .expect("second takeover");

    assert_eq!((first.fence, second.fence, third.fence), (1, 2, 3));
    for stale in [&first, &second] {
        let error = WorkspaceStore::release_with_conn(&db.conn, stale, NOW + 3 * TTL)
            .await
            .expect_err("every superseded fence stays refused");
        assert!(matches!(error, WorkspaceError::LeaseLost { .. }));
    }
}

// ---------------------------------------------------------------------------
// Lifecycle: provisioning → active → releasing → released / orphaned
// ---------------------------------------------------------------------------

/// A task workspace is published as `provisioning` and only becomes `active`
/// once its directory really exists (§C.8 — a failed provision must never
/// leave a fake active record).
#[tokio::test]
async fn provisioning_publishes_active_only_after_materialization() {
    let db = open_db().await;
    let path = workspace_dir(db.path.parent().expect("db parent"), "task");

    let lease = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::task(WorkspaceKind::TaskFuse, path, "agent-a", TTL)
            .with_owner(WorkspaceOwnerKind::Agent, Some("agent-a".to_string()))
            .with_base(Some("0".repeat(40)), Some("main".to_string())),
        NOW,
    )
    .await
    .expect("acquire");

    let record = WorkspaceStore::get_with_conn(&db.conn, &lease.workspace_id)
        .await
        .expect("read")
        .expect("record");
    assert_eq!(record.state, WorkspaceState::Provisioning);
    assert!(
        record.state.holds_identity(),
        "provisioning reserves the path"
    );

    WorkspaceStore::activate_with_conn(&db.conn, &lease, NOW + 5)
        .await
        .expect("activate after materialization");
    let record = WorkspaceStore::get_with_conn(&db.conn, &lease.workspace_id)
        .await
        .expect("read")
        .expect("record");
    assert_eq!(record.state, WorkspaceState::Active);
    assert_eq!(record.branch.as_deref(), Some("main"));

    // Activating twice is refused rather than silently re-published.
    let error = WorkspaceStore::activate_with_conn(&db.conn, &lease, NOW + 6)
        .await
        .expect_err("a second activate has no provisioning row to publish");
    assert!(matches!(error, WorkspaceError::LeaseLost { .. }));
}

/// Releasing settles the record and frees BOTH the linked identity and the
/// canonical path for a fresh acquire — which mints a new workspace id, never
/// re-leases the settled row.
#[tokio::test]
async fn released_workspace_frees_identity_and_path() {
    let db = open_db().await;
    let path = workspace_dir(db.path.parent().expect("db parent"), "linked-a");
    let request = AcquireRequest::linked("wt-1", path, "agent-a", TTL);

    let first = WorkspaceStore::acquire(&db.conn, &request, NOW)
        .await
        .expect("acquire");
    WorkspaceStore::begin_release_with_conn(&db.conn, &first, NOW + 1)
        .await
        .expect("begin release");

    // While releasing, the identity is still reserved.
    let error = WorkspaceStore::acquire(&db.conn, &request, NOW + 2)
        .await
        .expect_err("a releasing workspace still holds its identity");
    assert!(matches!(error, WorkspaceError::LeaseHeld(_)));

    WorkspaceStore::release_with_conn(&db.conn, &first, NOW + 3)
        .await
        .expect("release");

    let second = WorkspaceStore::acquire(&db.conn, &request, NOW + 4)
        .await
        .expect("the identity is free once the record settles");
    assert_ne!(second.workspace_id, first.workspace_id);
    assert_eq!(second.fence, 1, "a fresh record starts its own fence line");

    // The settled row keeps its audit trail and is not resurrected.
    let settled = WorkspaceStore::get_with_conn(&db.conn, &first.workspace_id)
        .await
        .expect("read")
        .expect("record");
    assert_eq!(settled.state, WorkspaceState::Released);
    assert_eq!(settled.lease_expires_at, None);
    assert_eq!(live_row_count(&db.conn).await, 1);
}

/// A crashed owner's workspace is marked `orphaned`: the identity frees up so
/// work can continue, the row survives for diagnosis, and a doctor can adopt it
/// afterwards.
#[tokio::test]
async fn orphaned_workspace_frees_identity_and_stays_diagnosable() {
    let db = open_db().await;
    let root = db.path.parent().expect("db parent");
    let path = workspace_dir(root, "task");

    let lease = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::task(WorkspaceKind::TaskCopy, path.clone(), "agent-a", TTL)
            .with_association(Some("task-7".to_string()), Some("session-7".to_string())),
        NOW,
    )
    .await
    .expect("acquire");

    WorkspaceStore::abandon_with_conn(&db.conn, &lease, NOW + 1)
        .await
        .expect("cleanup failure orphans the record");

    let record = WorkspaceStore::get_with_conn(&db.conn, &lease.workspace_id)
        .await
        .expect("read")
        .expect("record");
    assert_eq!(record.state, WorkspaceState::Orphaned);
    assert!(!record.state.holds_identity());
    assert_eq!(record.task_id.as_deref(), Some("task-7"));
    assert_eq!(record.session_id.as_deref(), Some("session-7"));

    // The directory can be re-provisioned by someone else...
    let replacement = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::task(WorkspaceKind::TaskCopy, path, "agent-b", TTL),
        NOW + 2,
    )
    .await
    .expect("an orphan no longer blocks the path");

    // ...after which adopting the orphan back onto that live path is refused
    // instead of double-claiming the directory.
    let error = WorkspaceStore::reclaim_expired(
        &db.conn,
        &lease.workspace_id,
        "doctor",
        TTL,
        NOW + TTL + 3,
    )
    .await
    .expect_err("an orphan cannot be revived onto a path someone else owns");
    assert!(matches!(error, WorkspaceError::LeaseHeld(_)));
    assert_eq!(
        WorkspaceStore::get_with_conn(&db.conn, &replacement.workspace_id)
            .await
            .expect("read")
            .expect("record")
            .lease_owner
            .as_deref(),
        Some("agent-b")
    );
}

/// Orphaning is not fence-conditional (the owner may be dead), but it can only
/// move LIVE rows — a settled record is never dragged back out of its terminal
/// state, and an unknown id is an error rather than a silent no-op.
#[tokio::test]
async fn orphaning_never_resurrects_a_settled_record() {
    let db = open_db().await;
    let path = workspace_dir(db.path.parent().expect("db parent"), "task");

    let lease = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::task(WorkspaceKind::TaskCopy, path, "agent-a", TTL),
        NOW,
    )
    .await
    .expect("acquire");
    WorkspaceStore::release_with_conn(&db.conn, &lease, NOW + 1)
        .await
        .expect("release");

    WorkspaceStore::abandon_with_conn(&db.conn, &lease, NOW + 2)
        .await
        .expect("abandoning a settled record is a no-op, not an error");
    assert_eq!(
        WorkspaceStore::get_with_conn(&db.conn, &lease.workspace_id)
            .await
            .expect("read")
            .expect("record")
            .state,
        WorkspaceState::Released
    );

    let error = WorkspaceStore::orphan_expired_with_conn(&db.conn, "no-such-workspace", NOW + 3)
        .await
        .expect_err("an unknown workspace id must not be silently ignored");
    assert!(matches!(error, WorkspaceError::NotFound(_)));
}

/// The scavenger sweep may only orphan a workspace whose lease deadline has
/// passed — a healthy agent must never have its workspace swept out from under
/// it — and a stale owner cannot abandon (and so free) a reclaimed workspace.
#[tokio::test]
async fn orphan_sweep_and_abandon_respect_live_owners() {
    let db = open_db().await;
    let path = workspace_dir(db.path.parent().expect("db parent"), "task");

    let lease = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::task(WorkspaceKind::TaskCopy, path, "agent-a", TTL),
        NOW,
    )
    .await
    .expect("acquire");

    let refused = WorkspaceStore::orphan_expired_with_conn(&db.conn, &lease.workspace_id, NOW + 1)
        .await
        .expect_err("a live lease must not be swept");
    assert!(matches!(refused, WorkspaceError::LeaseHeld(_)));
    assert_eq!(
        WorkspaceStore::get_with_conn(&db.conn, &lease.workspace_id)
            .await
            .expect("read")
            .expect("record")
            .state,
        WorkspaceState::Provisioning
    );

    // Once the owner is gone and the deadline passes, the sweep applies...
    let after_expiry = NOW + TTL + 1;
    WorkspaceStore::orphan_expired_with_conn(&db.conn, &lease.workspace_id, after_expiry)
        .await
        .expect("an expired lease is sweepable");
    assert_eq!(
        WorkspaceStore::get_with_conn(&db.conn, &lease.workspace_id)
            .await
            .expect("read")
            .expect("record")
            .state,
        WorkspaceState::Orphaned
    );

    // ...and a doctor adopting the orphan takes it as RELEASING (teardown
    // ownership), never re-advertising a half-built directory as usable.
    let adopted = WorkspaceStore::reclaim_expired(
        &db.conn,
        &lease.workspace_id,
        "doctor",
        TTL,
        after_expiry + 1,
    )
    .await
    .expect("adopt the orphan");
    assert_eq!(adopted.fence, 2);
    let record = WorkspaceStore::get_with_conn(&db.conn, &lease.workspace_id)
        .await
        .expect("read")
        .expect("record");
    assert_eq!(record.state, WorkspaceState::Releasing);

    // The dead owner can no longer abandon what the doctor now holds.
    let stale = WorkspaceStore::abandon_with_conn(&db.conn, &lease, after_expiry + 2)
        .await
        .expect_err("a stale owner cannot orphan a reclaimed workspace");
    assert!(matches!(stale, WorkspaceError::LeaseLost { .. }));
    assert_eq!(
        WorkspaceStore::get_with_conn(&db.conn, &lease.workspace_id)
            .await
            .expect("read")
            .expect("record")
            .state,
        WorkspaceState::Releasing
    );
}

/// A takeover of a LIVE record keeps its lifecycle state: a half-materialized
/// provisioning workspace must not become `active` merely because its lease
/// changed hands.
#[tokio::test]
async fn reclaim_preserves_the_lifecycle_state_of_a_live_record() {
    let db = open_db().await;
    let path = workspace_dir(db.path.parent().expect("db parent"), "task");

    let lease = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::task(WorkspaceKind::TaskFuse, path, "agent-a", TTL),
        NOW,
    )
    .await
    .expect("acquire");

    let reclaimed = WorkspaceStore::reclaim_expired(
        &db.conn,
        &lease.workspace_id,
        "doctor",
        TTL,
        NOW + TTL + 1,
    )
    .await
    .expect("reclaim");
    assert_eq!(reclaimed.fence, 2);

    let record = WorkspaceStore::get_with_conn(&db.conn, &lease.workspace_id)
        .await
        .expect("read")
        .expect("record");
    assert_eq!(
        record.state,
        WorkspaceState::Provisioning,
        "a takeover must not advertise a half-built workspace as active"
    );

    // The new owner still has to publish it explicitly.
    WorkspaceStore::activate_with_conn(&db.conn, &reclaimed, NOW + TTL + 2)
        .await
        .expect("the new owner activates once materialization completes");
}

// ---------------------------------------------------------------------------
// Human path, scavenger work list, and machine-facing pagination
// ---------------------------------------------------------------------------

/// A human using a linked worktree never takes a lease, so nothing in this
/// table can lock them out (§C.8 acceptance: "人类 linked worktree 未申请 Agent
/// lease 时不被无故锁死").
#[tokio::test]
async fn human_linked_worktree_has_no_lease_record() {
    let db = open_db().await;
    assert!(
        WorkspaceStore::find_live_linked_with_conn(&db.conn, "wt-1")
            .await
            .expect("lookup")
            .is_none()
    );

    // And a human-owned record, when one is created, is still just a record.
    let path = workspace_dir(db.path.parent().expect("db parent"), "linked-a");
    let lease = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-1", path, "human-cli", TTL)
            .with_owner(WorkspaceOwnerKind::Human, Some("eli".to_string())),
        NOW,
    )
    .await
    .expect("acquire");
    let record = WorkspaceStore::find_live_linked_with_conn(&db.conn, "wt-1")
        .await
        .expect("lookup")
        .expect("record");
    assert_eq!(record.workspace_id, lease.workspace_id);
    assert_eq!(record.owner_kind, WorkspaceOwnerKind::Human);
    assert_eq!(record.owner_id.as_deref(), Some("eli"));
}

/// The scavenger's work list is bounded, ordered oldest-deadline-first, and
/// contains only leases whose deadline has actually passed.
#[tokio::test]
async fn expired_lease_sweep_is_bounded_and_ordered() {
    let db = open_db().await;
    let root = db.path.parent().expect("db parent");

    for (index, ttl) in [(0u32, 30_000i64), (1, 10_000), (2, 20_000)] {
        WorkspaceStore::acquire(
            &db.conn,
            &AcquireRequest::task(
                WorkspaceKind::TaskCopy,
                workspace_dir(root, &format!("task-{index}")),
                format!("agent-{index}"),
                ttl,
            ),
            NOW,
        )
        .await
        .expect("acquire");
    }
    // A fourth lease that is still comfortably alive.
    WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::task(
            WorkspaceKind::TaskCopy,
            workspace_dir(root, "task-live"),
            "agent-live",
            10 * TTL,
        ),
        NOW,
    )
    .await
    .expect("acquire");

    let expired = WorkspaceStore::expired_leases_with_conn(&db.conn, NOW + 30_000, 10)
        .await
        .expect("sweep");
    let owners: Vec<_> = expired
        .iter()
        .map(|record| record.lease_owner.clone().unwrap_or_default())
        .collect();
    assert_eq!(
        owners,
        vec!["agent-1", "agent-2", "agent-0"],
        "expired leases come back oldest-deadline-first, and live ones stay out"
    );

    let bounded = WorkspaceStore::expired_leases_with_conn(&db.conn, NOW + 30_000, 2)
        .await
        .expect("sweep");
    assert_eq!(bounded.len(), 2, "the sweep respects its bound");
}

/// The machine listing walks every record exactly once through its keyset
/// cursor, and never returns an unbounded page (§C.14).
#[tokio::test]
async fn listing_pages_without_duplicates_or_loss() {
    let db = open_db().await;
    let root = db.path.parent().expect("db parent");

    let mut created = Vec::new();
    for index in 0..7 {
        let lease = WorkspaceStore::acquire(
            &db.conn,
            &AcquireRequest::task(
                WorkspaceKind::TaskCopy,
                workspace_dir(root, &format!("task-{index}")),
                format!("agent-{index}"),
                TTL,
            ),
            NOW,
        )
        .await
        .expect("acquire");
        created.push(lease.workspace_id);
    }
    created.sort();

    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = WorkspaceStore::list_with_conn(
            &db.conn,
            &WorkspaceListQuery::new().with_page(Some(3), cursor.clone()),
        )
        .await
        .expect("list page");
        assert!(page.items.len() <= 3);
        seen.extend(page.items.iter().map(|record| record.workspace_id.clone()));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(
        seen, created,
        "the cursor walk sees every record exactly once"
    );

    // State filters and the default/cap bounds.
    let released = WorkspaceStore::list_with_conn(
        &db.conn,
        &WorkspaceListQuery::new().with_states(vec![WorkspaceState::Released]),
    )
    .await
    .expect("filtered list");
    assert!(released.items.is_empty());
    const { assert!(DEFAULT_LIST_LIMIT <= MAX_LIST_LIMIT) };

    let unbounded_request = WorkspaceStore::list_with_conn(
        &db.conn,
        &WorkspaceListQuery::new().with_page(Some(u64::MAX), None),
    )
    .await
    .expect("clamped list");
    assert!(unbounded_request.items.len() <= MAX_LIST_LIMIT as usize);
}

/// `agent workspace list` pages by `(repo_id, workspace_id)`, so the production
/// migration must install a matching index. Otherwise the documented cap-500
/// machine interface can still degrade into a table scan as other repository
/// identities accumulate rows.
#[tokio::test]
async fn workspace_listing_hits_repo_paging_index() {
    let db = open_db().await;
    let root = db.path.parent().expect("db parent");
    let first = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-1", workspace_dir(root, "ws-1"), "owner-1", TTL),
        NOW,
    )
    .await
    .expect("first workspace");
    let _second = WorkspaceStore::acquire(
        &db.conn,
        &AcquireRequest::linked("wt-2", workspace_dir(root, "ws-2"), "owner-2", TTL),
        NOW + 1,
    )
    .await
    .expect("second workspace");

    let rows = db
        .conn
        .query_all(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "EXPLAIN QUERY PLAN \
             SELECT workspace_id, repo_id, kind, worktree_id, path, owner_kind, owner_id, \
                    task_id, session_id, base_commit, branch, state, lease_owner, lease_fence, \
                    lease_expires_at, created_at, updated_at \
             FROM workspace_record WHERE repo_id = ? AND workspace_id > ? \
             ORDER BY workspace_id ASC LIMIT ?",
            [REPO_ID.into(), first.workspace_id.into(), 51i64.into()],
        ))
        .await
        .expect("explain workspace pagination");
    let plan = rows
        .iter()
        .map(|row| row.try_get_by::<String, _>("detail").unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        plan.contains("idx_workspace_repo_paging"),
        "workspace pagination must use idx_workspace_repo_paging, got:\n{plan}"
    );
    assert!(
        !plan.contains("TEMP B-TREE"),
        "workspace pagination must not sort via temp B-tree, got:\n{plan}"
    );
    for line in plan.lines() {
        assert!(
            !line.trim_start().starts_with("SCAN workspace_record") || line.contains("USING"),
            "workspace pagination must not full-scan, got:\n{plan}"
        );
    }
}

/// The registry stores association IDs only — no prompts, transcripts, or tool
/// payloads may ever leak into it (§C.8: "path、prompt、transcript、tool
/// payload 不进入该表"). Pin the column set so a future column has to argue
/// with this test.
#[tokio::test]
async fn workspace_record_stores_only_association_ids() {
    let db = open_db().await;
    let rows = db
        .conn
        .query_all(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM pragma_table_info('workspace_record') ORDER BY cid".to_string(),
        ))
        .await
        .expect("read table shape");
    let columns: Vec<String> = rows
        .iter()
        .map(|row| row.try_get_by_index(0).expect("column name"))
        .collect();
    assert_eq!(
        columns,
        vec![
            "workspace_id",
            "repo_id",
            "kind",
            "worktree_id",
            "path",
            "owner_kind",
            "owner_id",
            "task_id",
            "session_id",
            "base_commit",
            "branch",
            "state",
            "lease_owner",
            "lease_fence",
            "lease_expires_at",
            "created_at",
            "updated_at",
        ]
    );
}

/// Validation happens before anything is written: a malformed request never
/// leaves a partial record behind.
#[tokio::test]
async fn invalid_requests_are_refused_before_any_write() {
    let db = open_db().await;
    let path = workspace_dir(db.path.parent().expect("db parent"), "linked-a");

    let mut missing_worktree = AcquireRequest::linked("wt-1", path.clone(), "agent-a", TTL);
    missing_worktree.worktree_id = None;
    let mut bad_state = AcquireRequest::linked("wt-1", path.clone(), "agent-a", TTL);
    bad_state.initial_state = WorkspaceState::Released;

    for request in [
        missing_worktree,
        bad_state,
        // Empty lease owner, non-positive TTL, and a cwd-relative path (which
        // would key one directory differently for every caller).
        AcquireRequest::linked("wt-1", path.clone(), "   ", TTL),
        AcquireRequest::linked("wt-1", path, "agent-a", 0),
        AcquireRequest::linked("wt-1", "relative/workspace", "agent-a", TTL),
    ] {
        let error = WorkspaceStore::acquire(&db.conn, &request, NOW)
            .await
            .expect_err("invalid request");
        assert!(
            matches!(error, WorkspaceError::Invalid(_)),
            "expected a validation refusal, got {error:?}"
        );
    }
    assert_eq!(live_row_count(&db.conn).await, 0);
}

/// A lease token for a workspace that never existed is refused as a lost lease
/// rather than creating anything.
#[tokio::test]
async fn unknown_workspace_lease_mutations_are_refused() {
    let db = open_db().await;
    let ghost = WorkspaceLease {
        workspace_id: "ghost".to_string(),
        owner: "agent-a".to_string(),
        fence: 1,
        expires_at: Some(NOW + TTL),
    };
    assert!(matches!(
        WorkspaceStore::release_with_conn(&db.conn, &ghost, NOW)
            .await
            .expect_err("no such workspace"),
        WorkspaceError::LeaseLost { .. }
    ));
    assert!(matches!(
        WorkspaceStore::reclaim_expired(&db.conn, "ghost", "doctor", TTL, NOW)
            .await
            .expect_err("no such workspace"),
        WorkspaceError::NotFound(_)
    ));
    assert_eq!(live_row_count(&db.conn).await, 0);
}
