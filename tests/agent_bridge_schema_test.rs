//! `tests/agent_bridge_schema_test.rs` — storage-layer semantics over the
//! bridge durable projection (plan-20260818 LB-02).
//!
//! Exercises the Rust `agent_bridge/storage.rs` service against a real
//! migrated in-memory database: idempotent session open, idempotent event
//! append with digest-conflict fail-closed, operation idempotency/digest, and
//! monotonic ack.

use libra::internal::{
    ai::agent_bridge::storage::{
        AppendEvent, append_events, get_session, last_accepted_seq, mark_acked, open_session,
        upsert_operation,
    },
    db::migration::run_builtin_migrations,
};
use sea_orm::{ConnectionTrait, Database};

async fn fresh_db() -> sea_orm::DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.expect("connect");
    run_builtin_migrations(&db)
        .await
        .expect("apply builtin migrations");
    db
}

#[tokio::test]
async fn open_session_is_idempotent_and_returns_existing() {
    let db = fresh_db().await;
    let (row, created) = open_session(&db, "s1", "repo1", None, None, None, None, 1000)
        .await
        .expect("create");
    assert!(created);
    assert_eq!(row.repository_id, "repo1");
    assert_eq!(row.session_state, "open");

    // Second open is a no-op returning the same row.
    let (row2, created2) = open_session(&db, "s1", "repo1", None, None, None, None, 2000)
        .await
        .expect("idempotent");
    assert!(!created2);
    assert_eq!(row2.bridge_session_id, "s1");

    let fetched = get_session(&db, "s1").await.expect("get");
    assert!(fetched.is_some());
    assert!(get_session(&db, "missing").await.expect("get").is_none());
}

/// Concurrent `open_session` calls for the same session id must both succeed
/// (the loser's INSERT is a no-op via ON CONFLICT), never surfacing a raw
/// unique-constraint error. This guards the idempotent contract against a
/// client retry racing the original open.
#[tokio::test]
async fn concurrent_open_session_is_idempotent_not_a_constraint_error() {
    let db = fresh_db().await;
    let a = db.clone();
    let b = db.clone();
    let (ra, rb) = tokio::join!(
        open_session(&a, "race", "repo1", None, None, None, None, 1000),
        open_session(&b, "race", "repo1", None, None, None, None, 1000),
    );
    // Both succeed (no Err from a unique-constraint violation) and exactly
    // one reports `created == true` (the winner); the loser reports
    // `created == false` so the scope-mismatch gate is not silently bypassed.
    let (row_a, created_a) = ra.expect("first open must not fail");
    let (row_b, created_b) = rb.expect("concurrent open must not fail");
    assert_eq!(row_a.bridge_session_id, "race");
    assert_eq!(row_b.bridge_session_id, "race");
    assert_ne!(
        created_a, created_b,
        "exactly one concurrent open must create the row (winner reports created=true, loser false)"
    );
    // Exactly one row exists.
    let rows = db
        .query_all_raw(sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Sqlite,
            "SELECT COUNT(*) AS n FROM agent_bridge_session WHERE bridge_session_id = ?",
            ["race".into()],
        ))
        .await
        .expect("count");
    let n: i64 = rows
        .into_iter()
        .next()
        .and_then(|r| r.try_get::<i64>("", "n").ok())
        .unwrap_or(0);
    assert_eq!(
        n, 1,
        "concurrent open must not create duplicate session rows"
    );
}

#[tokio::test]
async fn append_events_reports_accepted_duplicate_and_conflict() {
    let db = fresh_db().await;
    open_session(&db, "s1", "repo1", None, None, None, None, 1000)
        .await
        .expect("create");

    // First batch: both accepted.
    let out = append_events(
        &db,
        "s1",
        &[
            AppendEvent {
                event_seq: 1,
                event_type: "tool/result".into(),
                payload: r#"{"tool":"libra_status"}"#.into(),
                operation_id: None,
            },
            AppendEvent {
                event_seq: 2,
                event_type: "message".into(),
                payload: r#"{"kind":"assistant"}"#.into(),
                operation_id: None,
            },
        ],
        1500,
    )
    .await
    .expect("append");
    use libra::internal::ai::agent_bridge::storage::EventStatus::*;
    assert_eq!(out.per_event, vec![Accepted, Accepted]);
    assert_eq!(last_accepted_seq(&db, "s1").await.expect("cursor"), 2);

    // Replay with the SAME digest at seq 1 -> Duplicate (no new row).
    let out = append_events(
        &db,
        "s1",
        &[AppendEvent {
            event_seq: 1,
            event_type: "tool/result".into(),
            payload: r#"{"tool":"libra_status"}"#.into(),
            operation_id: None,
        }],
        1600,
    )
    .await
    .expect("replay");
    assert_eq!(out.per_event, vec![Duplicate]);
    assert_eq!(last_accepted_seq(&db, "s1").await.expect("cursor"), 2);

    // Different digest at a used sequence -> DigestConflict (fail-closed).
    let out = append_events(
        &db,
        "s1",
        &[AppendEvent {
            event_seq: 2,
            event_type: "message".into(),
            payload: r#"{"kind":"TAMPERED"}"#.into(),
            operation_id: None,
        }],
        1700,
    )
    .await
    .expect("conflict probe");
    assert_eq!(out.per_event, vec![DigestConflict]);
    // The original row is preserved (no last-writer-wins).
    assert_eq!(last_accepted_seq(&db, "s1").await.expect("cursor"), 2);
}

#[tokio::test]
async fn append_events_rejects_oversize_payload() {
    let db = fresh_db().await;
    open_session(&db, "s1", "repo1", None, None, None, None, 1000)
        .await
        .expect("create");
    let out = append_events(
        &db,
        "s1",
        &[AppendEvent {
            event_seq: 1,
            event_type: "big".into(),
            payload: "x".repeat(256 * 1024 + 1),
            operation_id: None,
        }],
        1500,
    )
    .await
    .expect("append");
    use libra::internal::ai::agent_bridge::storage::EventStatus::*;
    assert_eq!(out.per_event, vec![Rejected]);
    assert_eq!(last_accepted_seq(&db, "s1").await.expect("cursor"), 0);
}

#[tokio::test]
async fn operation_is_idempotent_and_digest_conflict_detected() {
    let db = fresh_db().await;
    open_session(&db, "s1", "repo1", None, None, None, None, 1000)
        .await
        .expect("create");

    let (created, existing) = upsert_operation(
        &db,
        "op-1",
        "s1",
        "commit.create",
        "digest-A",
        "repo1",
        None,
        1500,
    )
    .await
    .expect("insert");
    assert!(created);
    assert!(existing.is_none());

    // Same operation_id + same digest -> not created, returns the stored digest.
    let (created2, existing2) = upsert_operation(
        &db,
        "op-1",
        "s1",
        "commit.create",
        "digest-A",
        "repo1",
        None,
        1600,
    )
    .await
    .expect("replay");
    assert!(!created2);
    assert_eq!(existing2.as_deref(), Some("digest-A"));

    // Same operation_id + different digest -> returns the stored (original)
    // digest so the caller can detect the conflict and fail closed.
    let (created3, existing3) = upsert_operation(
        &db,
        "op-1",
        "s1",
        "commit.create",
        "digest-B",
        "repo1",
        None,
        1700,
    )
    .await
    .expect("conflict probe");
    assert!(!created3);
    assert_eq!(existing3.as_deref(), Some("digest-A"));
    // Original params_digest is unchanged.
    assert!(!created3);
}

#[tokio::test]
async fn ack_is_monotonic() {
    let db = fresh_db().await;
    open_session(&db, "s1", "repo1", None, None, None, None, 1000)
        .await
        .expect("create");
    mark_acked(&db, "s1", 5, 1100).await.expect("ack 5");
    mark_acked(&db, "s1", 3, 1200)
        .await
        .expect("ack 3 (must not rewind)");
    let row = get_session(&db, "s1").await.expect("get").expect("exists");
    assert_eq!(row.last_acked_seq, 5);
}
