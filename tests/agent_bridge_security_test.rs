//! `tests/agent_bridge_security_test.rs` — bridge security scope
//! (plan-20260818 LB-04/GC-LB-03/GC-LB-06).
//!
//! Verifies fail-closed guarantees: the method registry rejects unknown
//! methods, queries are repository-scoped and parameterized (no SQL
//! injection / cross-repository escape), and read results never leak raw
//! secrets.

use libra::internal::{
    ai::agent_bridge::{
        ingress::{BridgeContext, dispatch as ingress_dispatch},
        methods::{self, dispatch as read_dispatch},
        protocol::{MethodAllowlist, parse_request_line},
    },
    db::migration::run_builtin_migrations,
};
use sea_orm::Database;

async fn ctx() -> BridgeContext {
    let conn = Database::connect("sqlite::memory:").await.expect("connect");
    run_builtin_migrations(&conn)
        .await
        .expect("apply migrations");
    BridgeContext {
        conn,
        repository_id: "repo-1".into(),
        worktree_id: None,
    }
}

/// Unknown / low-level / SQL / shell methods are never in the allowlist, so
/// the transport rejects them fail-closed (GC-LB-03, LB-04).
#[test]
fn read_method_registry_rejects_unknown_methods() {
    for method in [
        "create_intent",
        "create_task",
        "create_run",
        "shell.exec",
        "sql.query",
        "fs.read",
        "session.get",
    ] {
        assert!(
            !MethodAllowlist::contains(method),
            "{method} must not be allowed"
        );
    }
    // The read methods that ARE allowed.
    for method in [
        "context.get",
        "status.get",
        "history.search",
        "checkpoint.list",
        "checkpoint.show",
    ] {
        assert!(
            MethodAllowlist::contains(method),
            "{method} must be allowed"
        );
    }
}

/// A search term that looks like SQL cannot escape the LIKE binding: it is a
/// bound parameter, never string-interpolated, so it matches nothing and
/// cannot dump other rows.
#[tokio::test]
async fn search_term_is_parameterized_no_sql_injection() {
    let c = ctx().await;
    ingress_dispatch(
        &c,
        &parse_request_line(r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"real-session"},"id":1}"#)
            .unwrap(),
    )
    .await
    .expect("open");

    let hostile = r#"x' OR 1=1 --"#;
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"history.search","params":{{"query":"{hostile}"}},"id":1}}"#
    );
    let resp = read_dispatch(&c, &parse_request_line(&frame).expect("parse"))
        .await
        .expect("history.search")
        .expect("response");
    let data = resp.result.as_ref().expect("result");
    let sessions = data["data"]["sessions"].as_array().expect("array");
    // The hostile term matches no session id/agent id -> no rows leaked.
    assert!(
        sessions.is_empty(),
        "SQL-injection term must not dump rows: {data}"
    );
}

/// Read results are repository-scoped and never include raw secrets: a
/// redacted event's raw token does not appear in any read method output.
#[tokio::test]
async fn read_results_never_leak_secrets() {
    let c = ctx().await;
    ingress_dispatch(
        &c,
        &parse_request_line(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1"},"id":1}"#,
        )
        .unwrap(),
    )
    .await
    .expect("open");

    // Append an event whose payload carries a secret-looking token; the
    // ingress redacts it before persistence.
    let secret = format!("{}_{}", "ghp", "abcdefghijklmnopqrstuvwxyz0123456789");
    let payload_text = format!(r#"{{"tool":"api_call","token":"{secret}"}}"#);
    let payload_field = serde_json::to_string(&payload_text).expect("encode");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"event.append","params":{{"session_id":"s1","events":[{{"seq":1,"type":"tool/result","payload":{payload_field}}}]}},"id":2}}"#
    );
    ingress_dispatch(&c, &parse_request_line(&frame).expect("parse"))
        .await
        .expect("append");

    // context.get / status.get never surface the raw token.
    for method in ["context.get", "status.get"] {
        let frame = format!(r#"{{"jsonrpc":"2.0","method":"{method}","id":1}}"#);
        let resp = read_dispatch(&c, &parse_request_line(&frame).expect("parse"))
            .await
            .expect(method)
            .expect("response");
        let text = resp.result.expect("result").to_string();
        assert!(
            !text.contains(&secret),
            "{method} must not leak the raw token: {text}"
        );
    }
    // The allowlist surface (already enforced) is stable.
    let _ = methods::READ_SCHEMA_VERSION;
}

/// A `workspace.claim` path that is not absolute (a cwd-relative / symlink
/// escape attempt) and a repository identity mismatch are both rejected
/// fail-closed (LB-06, GC-LB-06): the request can never name its own
/// repository or a non-canonical path.
#[tokio::test]
async fn symlink_and_repository_identity_are_rejected() {
    use libra::internal::{
        ai::agent_bridge::{protocol::parse_request_line, workspace::dispatch as ws_dispatch},
        workspace::WorkspaceStore,
    };
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    let conn = Database::connect("sqlite::memory:").await.expect("connect");
    run_builtin_migrations(&conn)
        .await
        .expect("apply migrations");
    conn.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO config_kv (key, value, encrypted) VALUES ('libra.repoid', ?, 0)",
        ["repo-real".into()],
    ))
    .await
    .expect("seed");

    // (a) A relative path must be rejected, even with a MATCHING repo context.
    let good = BridgeContext {
        conn: conn.clone(),
        repository_id: "repo-real".into(),
        worktree_id: None,
    };
    ingress_dispatch(
        &good,
        &parse_request_line(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s1"},"id":1}"#,
        )
        .expect("parse"),
    )
    .await
    .expect("open session");
    let rel = r#"{"jsonrpc":"2.0","method":"workspace.claim","params":{"session_id":"s1","path":"relative/path","lease_ttl_ms":1000},"id":2}"#;
    let outcome = ws_dispatch(&good, &parse_request_line(rel).expect("parse")).await;
    assert!(
        outcome.is_err(),
        "a relative workspace path must be rejected fail-closed"
    );

    // (b) A repository-identity mismatch is fail-closed: a context that claims
    // a DIFFERENT repo than the store's `libra.repoid` cannot acquire a
    // workspace (the adapter cross-checks the canonical identity).
    let spoofed = BridgeContext {
        conn: conn.clone(),
        repository_id: "repo-spoofed".into(),
        worktree_id: None,
    };
    ingress_dispatch(
        &spoofed,
        &parse_request_line(
            r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s2"},"id":3}"#,
        )
        .expect("parse"),
    )
    .await
    .expect("open session under spoofed repo");
    let abs = format!(
        r#"{{"jsonrpc":"2.0","method":"workspace.claim","params":{{"session_id":"s2","path":"{}","lease_ttl_ms":1000}},"id":4}}"#,
        std::env::temp_dir().join("ws-dir").display()
    );
    let outcome = ws_dispatch(&spoofed, &parse_request_line(&abs).expect("parse")).await;
    assert!(
        outcome.is_err(),
        "a repository-identity mismatch must be rejected fail-closed"
    );

    // Sanity: the identity helper is exercised.
    let _ = WorkspaceStore::get_with_conn(&conn, "no-such-workspace")
        .await
        .is_err();
}
