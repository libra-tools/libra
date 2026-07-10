//! AG-24a raw-access gate for `libra agent checkpoint export` (plan.md
//! Task A8.5): redacted export needs no authorization; a raw export
//! requires `--allow-raw`, a raw request without it is refused fail-closed
//! (`LBR-AGENT-013`), and every raw access (grant or deny) appends one
//! append-only `agent_audit_log` row.

use std::{path::Path, sync::Arc, time::Duration};

use libra::{
    internal::{
        ai::{
            history::{CheckpointCommitParams, CheckpointScope, HistoryManager},
            observed_agents::Redactor,
        },
        branch::TRACES_BRANCH,
    },
    utils::client_storage::ClientStorage,
};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement, Value};

use super::{init_repo_via_cli, run_libra_command};

const SECRET: &str = "AKIAIOSFODNN7EXAMPLE";

async fn connect_repo_db(repo: &Path) -> DatabaseConnection {
    let db_path = repo.join(".libra").join("libra.db");
    let mut opts = ConnectOptions::new(format!("sqlite://{}", db_path.display()));
    opts.sqlx_logging(false)
        .connect_timeout(Duration::from_secs(5));
    Database::connect(opts).await.expect("connect repo db")
}

/// Seed a stopped session plus a real E4-libra checkpoint whose transcript
/// embeds `SECRET`, and return the checkpoint id.
async fn seed_checkpoint_with_secret(repo: &Path) -> String {
    let conn = connect_repo_db(repo).await;
    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at, stopped_at
         ) VALUES ('sess-x', 'claude_code', 'p-x', 'stopped', '/tmp/x', '{}', '{}', 1, 2, 3)",
        Vec::<Value>::new(),
    ))
    .await
    .expect("insert session");

    let repo_path = repo.join(".libra");
    let storage = Arc::new(ClientStorage::init(repo_path.join("objects")));
    let history =
        HistoryManager::new_with_ref(storage, repo_path, Arc::new(conn.clone()), TRACES_BRANCH);
    let redactor = Redactor::new_default();
    let (redacted, _) = redactor.redact(format!("transcript with key {SECRET} inside").as_bytes());
    let (meta_redacted, _) = redactor.redact(br#"{"checkpoint_id":"x"}"#);
    let (events_redacted, _) = redactor.redact(b"{}\n");
    let (report_redacted, _) = redactor.redact(b"{}");
    let checkpoint_id = "aabbccddeeff00112233445566778899".to_string();
    let written = history
        .append_checkpoint_commit(CheckpointCommitParams {
            checkpoint_id: &checkpoint_id,
            session_id: "sess-x",
            agent_kind: "claude_code",
            parent_commit: None,
            scope: CheckpointScope::Committed,
            tool_use_id: None,
            metadata_json: &meta_redacted,
            transcript_redacted: &redacted,
            lifecycle_events_jsonl: &events_redacted,
            redaction_report_json: &report_redacted,
        })
        .await
        .expect("append checkpoint");

    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (
            checkpoint_id, session_id, scope, parent_commit, tree_oid,
            metadata_blob_oid, traces_commit, created_at
         ) VALUES (?, 'sess-x', 'committed', NULL, ?, ?, ?, 100)",
        vec![
            Value::from(checkpoint_id.clone()),
            Value::from(written.tree_oid.to_string()),
            Value::from(written.metadata_blob_oid.to_string()),
            Value::from(written.commit_hash.to_string()),
        ],
    ))
    .await
    .expect("insert checkpoint row");
    conn.close().await.expect("close seed conn");
    checkpoint_id
}

async fn audit_rows(repo: &Path) -> Vec<(String, i64)> {
    let conn = connect_repo_db(repo).await;
    let rows = conn
        .query_all(Statement::from_string(
            conn.get_database_backend(),
            "SELECT checkpoint_id, granted FROM agent_audit_log ORDER BY timestamp".to_string(),
        ))
        .await
        .expect("query audit");
    rows.into_iter()
        .map(|r| {
            (
                r.try_get_by::<String, _>("checkpoint_id").unwrap(),
                r.try_get_by::<i64, _>("granted").unwrap(),
            )
        })
        .collect()
}

/// Full gate: redacted export needs no auth and writes no audit; a raw
/// request without --allow-raw is refused (LBR-AGENT-013) + audited as a
/// denial; --allow-raw --raw grants + audits + returns the un-redacted
/// bytes.
#[tokio::test]
async fn allow_raw_gate() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());
    let checkpoint_id = seed_checkpoint_with_secret(repo.path()).await;

    // (1) Default redacted export: succeeds, no audit row. The stored
    // transcript is redacted at capture (P0), so the secret is already
    // gone from every path — the redacted export re-scrubs defensively.
    let out = run_libra_command(
        &["agent", "checkpoint", "export", &checkpoint_id],
        repo.path(),
    );
    assert!(out.status.success(), "redacted export must succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("transcript with key"),
        "redacted export returns the (capture-redacted) transcript body: {stdout}"
    );
    assert!(!stdout.contains(SECRET), "the secret is never present");
    assert!(
        audit_rows(repo.path()).await.is_empty(),
        "redacted path writes no audit"
    );

    // (2) Raw request WITHOUT --allow-raw: fail-closed + audited denial.
    let out = run_libra_command(
        &["agent", "checkpoint", "export", &checkpoint_id, "--raw"],
        repo.path(),
    );
    assert!(!out.status.success(), "raw without --allow-raw must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LBR-AGENT-013"),
        "denial carries the stable code: {stderr}"
    );
    let rows = audit_rows(repo.path()).await;
    assert_eq!(rows.len(), 1, "the refusal is audited");
    assert_eq!(
        rows[0],
        (checkpoint_id.clone(), 0),
        "denial recorded granted=0"
    );

    // (3) --allow-raw --raw: granted + audited + un-redacted bytes.
    let out = run_libra_command(
        &[
            "agent",
            "checkpoint",
            "export",
            &checkpoint_id,
            "--allow-raw",
            "--raw",
        ],
        repo.path(),
    );
    assert!(out.status.success(), "authorized raw export must succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("transcript with key"),
        "raw export returns the stored transcript body as-is: {stdout}"
    );
    assert!(
        !stdout.contains(SECRET),
        "even raw export cannot leak a secret redacted at capture (P0)"
    );
    let rows = audit_rows(repo.path()).await;
    assert_eq!(rows.len(), 2, "the grant is audited too");
    assert_eq!(rows[1], (checkpoint_id, 1), "grant recorded granted=1");
}

/// The raw gate is fail-closed BEFORE the checkpoint lookup: `export
/// <missing-id> --raw` returns LBR-AGENT-013 (not a "no checkpoint" error
/// that would leak an existence oracle) and audits the refusal.
#[tokio::test]
async fn raw_denial_precedes_lookup_no_existence_oracle() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());
    // Deliberately do NOT seed the checkpoint.
    let out = run_libra_command(
        &[
            "agent",
            "checkpoint",
            "export",
            "deadbeefdeadbeefdeadbeefdeadbeef",
            "--raw",
        ],
        repo.path(),
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LBR-AGENT-013"),
        "denial fires before lookup, not a 'no checkpoint' error: {stderr}"
    );
    assert!(
        !stderr.contains("no checkpoint matches"),
        "must not reveal whether the id exists: {stderr}"
    );
    let rows = audit_rows(repo.path()).await;
    assert_eq!(rows.len(), 1, "the pre-lookup refusal is still audited");
    assert_eq!(rows[0].1, 0, "recorded as a denial");
}

/// `--allow-raw` on its own (without `--raw`) is NOT a raw export: it
/// falls through to the redacted path and writes no audit row.
#[tokio::test]
async fn allow_raw_without_raw_is_redacted_no_audit() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());
    let checkpoint_id = seed_checkpoint_with_secret(repo.path()).await;
    let out = run_libra_command(
        &[
            "agent",
            "checkpoint",
            "export",
            &checkpoint_id,
            "--allow-raw",
        ],
        repo.path(),
    );
    assert!(
        out.status.success(),
        "--allow-raw alone must succeed (redacted)"
    );
    assert!(
        audit_rows(repo.path()).await.is_empty(),
        "--allow-raw without --raw takes the redacted path and writes no audit"
    );
}
