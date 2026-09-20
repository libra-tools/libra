//! Session-level advisory lock and CAS conflict acceptance (plan-20260920
//! KEEP: the session store survives the `libra code` removal).
//!
//! The session-level advisory lock is the building block for the concurrency
//! contract: two writers must not interleave on the same
//! `(working_dir, thread_id)` pair, but writers for *different* sessions
//! must not block one another. These tests pin five hermetic properties:
//!
//! - **lock + drop release** — `lock_session()` returns a `SessionFileLock`
//!   guard; dropping the guard releases the lock so the next acquirer
//!   succeeds immediately (no zombie lock files).
//! - **same-id serialization** — two acquirers for the same id must NOT
//!   both hold the lock simultaneously; the second observes
//!   `WouldBlock`-style timeout if the first lingers past
//!   `SESSION_LOCK_TIMEOUT` (5 s), but completes promptly once the first
//!   releases.
//! - **cross-id independence** — locks on `agent-a` and `agent-b` are
//!   independent; holding one must not block the other.
//! - **lock content shape** — the on-disk lock file records `pid=` +
//!   `created_at_ns=` so stale-lock detection can age it out after the
//!   30-second `STALE_SESSION_LOCK_AGE` window without an in-memory
//!   handshake.
//! - **drop + reacquire roundtrip** — after a guard is dropped, a fresh
//!   acquire on the same id succeeds and produces a new lock file.
//!
//! SQLite-busy and worktree-reservation contracts are intentionally out of
//! scope here — they live on the `ai_thread_provider_metadata` /
//! `FuseProvisionState` surfaces respectively and are covered by
//! `db_migration_test.rs` and the worktree-fuse tests. This file pins the
//! filesystem-level advisory-lock invariant that the doc lists first.
//!
//! Layer: L1 — hermetic, no real binary, just `SessionStore` against a
//! tempdir.

use std::{sync::Arc, time::Duration};

use libra::internal::ai::session::SessionStore;
use tempfile::TempDir;

/// Lock + drop release: acquiring is fast, the file appears, dropping the
/// guard removes the file, and a re-acquire on the same id succeeds.
#[test]
fn lock_session_releases_on_drop_so_reacquire_succeeds() {
    let tmp = TempDir::new().unwrap();
    let store = SessionStore::from_storage_path(tmp.path());
    let id = "lock-roundtrip";

    let lock_path = tmp.path().join("sessions").join(format!("{id}.lock"));
    assert!(!lock_path.exists(), "lock file must not pre-exist");

    {
        let _guard = store.lock_session(id).unwrap();
        assert!(lock_path.exists(), "lock file present while guard alive");
    }
    assert!(
        !lock_path.exists(),
        "lock file must be removed when guard drops; otherwise re-acquire would spin"
    );

    // Drop-then-reacquire must succeed immediately.
    let started = std::time::Instant::now();
    let _again = store.lock_session(id).unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "second acquire after drop should be effectively instant; took {:?}",
        started.elapsed(),
    );
}

/// Same-id serialization: while one guard is alive, a concurrent acquire on
/// the same id from another thread MUST NOT succeed; once the first guard
/// drops, the second acquire completes promptly.
#[test]
fn lock_session_blocks_concurrent_acquire_on_same_id_until_drop() {
    let tmp = TempDir::new().unwrap();
    let store = Arc::new(SessionStore::from_storage_path(tmp.path()));
    let id = "lock-serialize";

    let holder = store.lock_session(id).unwrap();

    let store_clone = Arc::clone(&store);
    let id_owned = id.to_string();
    let waiter = std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let guard = store_clone
            .lock_session(&id_owned)
            .expect("waiter must eventually acquire after holder drops");
        (started.elapsed(), guard)
    });

    // Let the waiter actually start blocking before we drop the holder; a
    // generous 200 ms is well below the 5 s timeout but well above the
    // 50 ms poll interval, so the waiter will be looping inside
    // `lock_session` when we release.
    std::thread::sleep(Duration::from_millis(200));
    drop(holder);

    let (elapsed, _waiter_guard) = waiter.join().unwrap();
    assert!(
        elapsed >= Duration::from_millis(150),
        "waiter must observe the holder; got elapsed {elapsed:?} which is too small",
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "waiter must succeed well before the 5s timeout; got {elapsed:?}",
    );
}

/// Cross-id independence: holding `agent-a` must not block `agent-b`.
/// Both guards live simultaneously and dropping order is independent.
#[test]
fn lock_session_does_not_contend_across_different_ids() {
    let tmp = TempDir::new().unwrap();
    let store = SessionStore::from_storage_path(tmp.path());

    let started = std::time::Instant::now();
    let guard_a = store.lock_session("agent-a").unwrap();
    let guard_b = store.lock_session("agent-b").unwrap();
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_millis(200),
        "two independent ids must both be lockable instantly; took {elapsed:?}",
    );

    let lock_a = tmp.path().join("sessions").join("agent-a.lock");
    let lock_b = tmp.path().join("sessions").join("agent-b.lock");
    assert!(
        lock_a.exists() && lock_b.exists(),
        "both lock files coexist"
    );

    drop(guard_a);
    assert!(
        !lock_a.exists(),
        "agent-a lock removed but agent-b lock still held"
    );
    assert!(lock_b.exists(), "dropping agent-a must not touch agent-b");
    drop(guard_b);
    assert!(
        !lock_b.exists(),
        "agent-b lock removed after its guard drops"
    );
}

/// Lock-content shape: the on-disk lock file records `pid=` + `created_at_ns=`
/// in a parseable shape so stale-lock detection can age it out after the
/// 30-second `STALE_SESSION_LOCK_AGE` window. The content carries the live
/// pid (so a forensic operator can identify the holder) and a nanosecond
/// timestamp (so the staleness check can use monotonic-ish comparisons).
#[test]
fn lock_session_writes_pid_and_created_at_ns_to_lock_file() {
    let tmp = TempDir::new().unwrap();
    let store = SessionStore::from_storage_path(tmp.path());
    let id = "lock-content";

    let _guard = store.lock_session(id).unwrap();
    let lock_path = tmp.path().join("sessions").join(format!("{id}.lock"));
    let content = std::fs::read_to_string(&lock_path).unwrap();

    assert!(
        content.contains(&format!("pid={}", std::process::id())),
        "lock file must record the holder's pid; got:\n{content}",
    );

    // `created_at_ns=` must be followed by digits; we don't assert a
    // specific value because the test runs at an indeterminate wall time,
    // but we *do* assert the field is present and numerically parseable.
    let ns_line = content
        .lines()
        .find(|line| line.starts_with("created_at_ns="))
        .expect("lock file must contain a created_at_ns= field");
    let ns_value = ns_line.trim_start_matches("created_at_ns=").trim();
    assert!(
        ns_value.parse::<u128>().is_ok(),
        "created_at_ns must be a u128 ns timestamp; got {ns_value:?}",
    );
}

/// Repeated lock + drop cycles on the same id are stable: 50 round-trips
/// in a tight loop should not accumulate state, leak handles, or trip
/// stale-lock cleanup paths.
#[test]
fn lock_session_supports_repeated_acquire_release_cycles() {
    let tmp = TempDir::new().unwrap();
    let store = SessionStore::from_storage_path(tmp.path());
    let id = "lock-cycles";
    let lock_path = tmp.path().join("sessions").join(format!("{id}.lock"));

    for i in 0..50 {
        let guard = store.lock_session(id).unwrap();
        assert!(lock_path.exists(), "lock file must exist on iter {i}");
        drop(guard);
        assert!(
            !lock_path.exists(),
            "lock file must be removed by Drop on iter {i}",
        );
    }
}
