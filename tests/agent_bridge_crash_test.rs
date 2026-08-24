//! `tests/agent_bridge_crash_test.rs` — crash-after-write-before-ack recovery
//! (plan-20260818 LB-03, GC-LB-09).
//!
//! An event batch is committed durably before the ack is written back. If the
//! process dies in that window, the peer must be able to reconnect and replay
//! the same batch: the replay is a no-op (duplicate, no new rows) and the ack
//! cursor advances without re-creating events or returning a false ack.

use libra::internal::{
    ai::agent_bridge::storage::{
        AppendEvent, append_events, contiguous_acked_seq, mark_acked, open_session,
    },
    db::migration::run_builtin_migrations,
};
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

async fn fresh_db() -> sea_orm::DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.expect("connect");
    run_builtin_migrations(&db).await.expect("apply migrations");
    db
}

async fn event_count(db: &sea_orm::DatabaseConnection, session: &str) -> i64 {
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT COUNT(*) AS n FROM agent_bridge_event WHERE bridge_session_id = ?",
            [session.into()],
        ))
        .await
        .expect("count");
    rows.into_iter()
        .next()
        .and_then(|r| r.try_get::<i64>("", "n").ok())
        .unwrap_or(0)
}

/// A crash after the batch is committed but before the ack is written leaves
/// durable events with a stale ack cursor; a reconnect replay converges
/// without duplication or a false ack.
#[tokio::test]
async fn crash_after_write_before_ack_recovers_without_duplication() {
    let db = fresh_db().await;
    open_session(&db, "s1", "repo1", None, None, None, None, 1000)
        .await
        .expect("create");

    let batch = vec![
        AppendEvent {
            event_seq: 1,
            event_type: "tool/result".into(),
            payload: r#"{"tool":"a"}"#.into(),
            operation_id: None,
        },
        AppendEvent {
            event_seq: 2,
            event_type: "tool/result".into(),
            payload: r#"{"tool":"b"}"#.into(),
            operation_id: None,
        },
    ];

    // First attempt commits the batch durably but "crashes" before ack:
    // append_events commits, but the ack cursor was never advanced.
    append_events(&db, "s1", &batch, 1500)
        .await
        .expect("commit");
    assert_eq!(event_count(&db, "s1").await, 2, "events are durable");
    let before_ack = libra::internal::ai::agent_bridge::storage::get_session(&db, "s1")
        .await
        .expect("get")
        .expect("exists");
    assert_eq!(
        before_ack.last_acked_seq, 0,
        "ack not yet written (crash window)"
    );

    // Reconnect: the peer replays the exact same batch. Every event is a
    // duplicate (same digest), no new rows are created.
    let outcome = append_events(&db, "s1", &batch, 1600)
        .await
        .expect("replay");
    use libra::internal::ai::agent_bridge::storage::EventStatus::*;
    assert_eq!(outcome.per_event, vec![Duplicate, Duplicate]);
    assert_eq!(
        event_count(&db, "s1").await,
        2,
        "no duplicate rows on replay"
    );

    // The durable cursor is now discoverable and the ack advances exactly to
    // the contiguous end (no false ack beyond committed data).
    let contiguous = contiguous_acked_seq(&db, "s1").await.expect("cursor");
    assert_eq!(contiguous, 2);
    mark_acked(&db, "s1", contiguous, 1700).await.expect("ack");
    let row = libra::internal::ai::agent_bridge::storage::get_session(&db, "s1")
        .await
        .expect("get")
        .expect("exists");
    assert_eq!(row.last_acked_seq, 2);
}

/// A gap in the sequence stops the ack cursor (a peer never believes a later
/// event is acked when an earlier one is missing).
#[tokio::test]
async fn ack_cursor_stops_at_gaps() {
    let db = fresh_db().await;
    open_session(&db, "s1", "repo1", None, None, None, None, 1000)
        .await
        .expect("create");

    // Commit seq 3 only — seqs 1,2 are missing, so the contiguous ack is 0.
    append_events(
        &db,
        "s1",
        &[AppendEvent {
            event_seq: 3,
            event_type: "tool/result".into(),
            payload: "{}".into(),
            operation_id: None,
        }],
        1500,
    )
    .await
    .expect("commit");
    assert_eq!(contiguous_acked_seq(&db, "s1").await.expect("cursor"), 0);

    // Fill the gap; now the cursor advances through 3.
    append_events(
        &db,
        "s1",
        &[
            AppendEvent {
                event_seq: 1,
                event_type: "tool/result".into(),
                payload: "{}".into(),
                operation_id: None,
            },
            AppendEvent {
                event_seq: 2,
                event_type: "tool/result".into(),
                payload: "{}".into(),
                operation_id: None,
            },
        ],
        1600,
    )
    .await
    .expect("fill gap");
    assert_eq!(contiguous_acked_seq(&db, "s1").await.expect("cursor"), 3);
}
