//! Integration coverage for `libra agent clean`.
//!
//! The clean command is destructive over the external-agent checkpoint catalog,
//! so the stopped-vs-active session boundary is part of the command contract.

use std::{path::Path, str::FromStr, sync::Arc, time::Duration};

use git_internal::{
    hash::ObjectHash,
    internal::object::{ObjectTrait, commit::Commit},
};
use libra::{
    internal::{
        ai::{
            history::{
                AGENT_TRACES_INFLIGHT_TTL_MS, CheckpointCommitParams, CheckpointScope,
                HistoryManager, TracesInflightMarker, clear_traces_inflight_marker_if_generation,
                write_traces_inflight_marker,
            },
            observed_agents::Redactor,
        },
        branch::TRACES_BRANCH,
    },
    utils::{client_storage::ClientStorage, object::read_git_object},
};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement, Value};

use super::{
    assert_cli_success, init_repo_via_cli, parse_json_stdout, run_libra_command,
    run_libra_command_with_stdin_and_env,
};

async fn connect_repo_db(repo: &Path) -> DatabaseConnection {
    let db_path = repo.join(".libra").join("libra.db");
    let mut opts = ConnectOptions::new(format!("sqlite://{}", db_path.display()));
    opts.sqlx_logging(false)
        .connect_timeout(Duration::from_secs(5));
    Database::connect(opts)
        .await
        .expect("connect repository database")
}

async fn seed_session(
    conn: &DatabaseConnection,
    session_id: &str,
    state: &str,
    started_at: i64,
    last_event_at: i64,
    stopped_at: i64,
) {
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at, stopped_at
         ) VALUES (?, 'claude_code', ?, ?, ?, '{}', '{}', ?, ?, ?)",
        vec![
            Value::from(session_id),
            Value::from(format!("provider-{session_id}")),
            Value::from(state),
            Value::from("/tmp/libra-agent-clean-test"),
            Value::from(started_at),
            Value::from(last_event_at),
            Value::from(stopped_at),
        ],
    ))
    .await
    .expect("insert agent_session");
}

async fn seed_checkpoint(
    conn: &DatabaseConnection,
    checkpoint_id: &str,
    session_id: &str,
    scope: &str,
    created_at: i64,
) {
    let parent_commit = format!("{created_at:040x}");
    let tree_oid = format!("{:040x}", created_at + 1);
    let metadata_blob_oid = format!("{:040x}", created_at + 2);
    let traces_commit = format!("{:040x}", created_at + 3);

    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (
            checkpoint_id, session_id, scope, parent_commit, tree_oid,
            metadata_blob_oid, traces_commit, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        vec![
            Value::from(checkpoint_id),
            Value::from(session_id),
            Value::from(scope),
            Value::from(parent_commit),
            Value::from(tree_oid),
            Value::from(metadata_blob_oid),
            Value::from(traces_commit),
            Value::from(created_at),
        ],
    ))
    .await
    .expect("insert agent_checkpoint");
}

async fn seed_checkpoint_commit(
    conn: &DatabaseConnection,
    repo: &Path,
    checkpoint_id: &str,
    session_id: &str,
    scope: CheckpointScope,
    created_at: i64,
) -> String {
    let repo_path = repo.join(".libra");
    let storage = Arc::new(ClientStorage::init(repo_path.join("objects")));
    let history = HistoryManager::new_with_ref(
        storage,
        repo_path.clone(),
        Arc::new(conn.clone()),
        TRACES_BRANCH,
    );
    let redactor = Redactor::new_default();
    let (redacted, _) = redactor.redact(format!("transcript for {checkpoint_id}").as_bytes());
    let metadata = format!(r#"{{"checkpoint_id":"{checkpoint_id}"}}"#);
    let (meta_redacted, _) = redactor.redact(metadata.as_bytes());
    let (events_redacted, _) = redactor.redact(b"{}\n");
    let (report_redacted, _) = redactor.redact(b"{}");
    let marker = TracesInflightMarker::new(
        session_id,
        checkpoint_id,
        chrono::Utc::now().timestamp_millis(),
    );
    write_traces_inflight_marker(conn, &marker)
        .await
        .expect("register seeded checkpoint writer marker");
    let written = history
        .append_checkpoint_commit(CheckpointCommitParams {
            checkpoint_id,
            session_id,
            marker_generation: marker.generation.as_deref().expect("new marker generation"),
            agent_kind: "claude_code",
            parent_commit: None,
            scope,
            tool_use_id: None,
            metadata_json: &meta_redacted,
            transcript_redacted: &redacted,
            lifecycle_events_jsonl: &events_redacted,
            redaction_report_json: &report_redacted,
            txn_extra: None,
            deadline: None,
        })
        .await
        .expect("append checkpoint commit");
    clear_traces_inflight_marker_if_generation(
        conn,
        session_id,
        checkpoint_id,
        &written.marker_generation,
    )
    .await
    .expect("clear seeded checkpoint writer marker");

    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (
            checkpoint_id, session_id, scope, parent_commit, tree_oid,
            metadata_blob_oid, traces_commit, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        vec![
            Value::from(checkpoint_id),
            Value::from(session_id),
            Value::from(scope.as_str()),
            Value::from(format!("{created_at:040x}")),
            Value::from(written.tree_oid.to_string()),
            Value::from(written.metadata_blob_oid.to_string()),
            Value::from(written.commit_hash.to_string()),
            Value::from(created_at),
        ],
    ))
    .await
    .expect("insert real agent_checkpoint");

    // This helper writes through HistoryManager outside a CLI invocation, so
    // its index work intentionally uses the direct-library FIFO lane. Wait for
    // this repository's durable markers to retire before a test invokes
    // destructive cleanup; otherwise the test exercises preflight backlog
    // refusal instead of the traces/catalog condition it was built to assert.
    let marker_dir = repo_path.join("object-index-repair");
    for _ in 0..400 {
        let marker_count = std::fs::read_dir(&marker_dir)
            .map(|entries| entries.count())
            .unwrap_or(0);
        if marker_count == 0 {
            return written.commit_hash.to_string();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "seeded checkpoint object-index markers did not retire within 10 seconds: {}",
        marker_dir.display()
    );
}

async fn checkpoint_exists(conn: &DatabaseConnection, checkpoint_id: &str) -> bool {
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE checkpoint_id = ?",
            [Value::from(checkpoint_id)],
        ))
        .await
        .expect("query checkpoint count")
        .expect("count row");
    let count: i64 = row.try_get_by("n").expect("decode count");
    count == 1
}

async fn scalar_count(conn: &DatabaseConnection, sql: &str) -> i64 {
    conn.query_one_raw(Statement::from_string(
        conn.get_database_backend(),
        sql.to_string(),
    ))
    .await
    .expect("query count")
    .expect("count row")
    .try_get_by("n")
    .expect("decode count")
}

async fn checkpoint_traces_commit(
    conn: &DatabaseConnection,
    checkpoint_id: &str,
) -> Option<String> {
    conn.query_one_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "SELECT traces_commit FROM agent_checkpoint WHERE checkpoint_id = ? LIMIT 1",
        [Value::from(checkpoint_id)],
    ))
    .await
    .expect("query checkpoint traces_commit")
    .map(|row| {
        row.try_get_by("traces_commit")
            .expect("decode traces_commit")
    })
}

async fn agent_traces_head(conn: &DatabaseConnection) -> Option<String> {
    conn.query_one_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "SELECT `commit` FROM reference WHERE name = ? AND kind = 'Branch' LIMIT 1",
        [Value::from(TRACES_BRANCH)],
    ))
    .await
    .expect("query traces ref")
    .and_then(|row| row.try_get_by("commit").ok().flatten())
}

fn reachable_agent_trace_commits(repo: &Path, head: &str) -> Vec<String> {
    let repo_path = repo.join(".libra");
    let mut commits = Vec::new();
    let mut next = Some(ObjectHash::from_str(head).expect("parse head"));
    while let Some(oid) = next {
        let data = read_git_object(&repo_path, &oid).expect("read commit object");
        let commit = Commit::from_bytes(&data, oid).expect("parse commit object");
        commits.push(oid.to_string());
        next = commit.parent_commit_ids.first().copied();
    }
    commits
}

#[tokio::test]
async fn agent_clean_all_does_not_drop_active_session_checkpoints() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());

    let conn = connect_repo_db(repo.path()).await;
    seed_session(&conn, "stopped-session", "stopped", 10, 20, 30).await;
    seed_session(&conn, "active-session", "active", 40, 50, 0).await;
    seed_checkpoint(
        &conn,
        "cp-stopped-temp",
        "stopped-session",
        "temporary",
        100,
    )
    .await;
    seed_checkpoint(
        &conn,
        "cp-stopped-committed",
        "stopped-session",
        "committed",
        101,
    )
    .await;
    seed_checkpoint(&conn, "cp-active-temp", "active-session", "temporary", 102).await;
    conn.close().await.expect("close seed connection");

    let output = run_libra_command(&["--quiet", "agent", "clean", "--all"], repo.path());
    assert_cli_success(&output, "libra agent clean --all");

    let conn = connect_repo_db(repo.path()).await;
    assert!(
        !checkpoint_exists(&conn, "cp-stopped-temp").await,
        "--all should drop temporary checkpoints for stopped sessions"
    );
    assert!(
        checkpoint_exists(&conn, "cp-stopped-committed").await,
        "committed checkpoints must never be dropped"
    );
    assert!(
        checkpoint_exists(&conn, "cp-active-temp").await,
        "--all must not drop temporary checkpoints for active sessions"
    );
}

/// plan-20260713 GC-DR-11: pruning a checkpoint reconciles the turn's current
/// claim/revision and import cursor in the same transaction. Dry-run reports
/// the candidate without mutating any companion state.
#[tokio::test]
async fn agent_clean_claims_gc_reconciles_companion_import_state() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());
    let conn = connect_repo_db(repo.path()).await;
    let old = chrono::Utc::now().timestamp() - 3 * 86_400;
    seed_session(&conn, "gc-coverage", "stopped", old, old, old).await;
    let traces_commit = seed_checkpoint_commit(
        &conn,
        repo.path(),
        "cp-gc-coverage",
        "gc-coverage",
        CheckpointScope::Committed,
        old,
    )
    .await;
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_coverage_claim (
            session_id, logical_turn_key, coverage_schema_version,
            coverage_digest, completeness, revision, state, checkpoint_id,
            traces_commit, source_channel, created_at, updated_at
         ) VALUES (?, 'turn-1', 1, 'digest', 'complete', 1,
                   'conflicted', ?, ?, 'import', ?, ?)",
        [
            "gc-coverage".into(),
            "cp-gc-coverage".into(),
            traces_commit.clone().into(),
            old.into(),
            old.into(),
        ],
    ))
    .await
    .expect("seed coverage claim");
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_coverage_revision (
            session_id, logical_turn_key, coverage_schema_version, revision,
            checkpoint_id, coverage_digest, completeness, source_channel, created_at
         ) VALUES (?, 'turn-1', 1, 1, ?, 'digest', 'complete', 'import', ?)",
        ["gc-coverage".into(), "cp-gc-coverage".into(), old.into()],
    ))
    .await
    .expect("seed coverage revision");
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_coverage_conflict (
            session_id, logical_turn_key, coverage_schema_version,
            incumbent_revision, incumbent_digest, incumbent_checkpoint_id,
            incoming_digest, incoming_source_channel, incoming_observed_at,
            incoming_canonical_json, incoming_redaction_report_json
         ) VALUES (?, 'turn-1', 1, 1, 'digest', ?, 'challenger', 'import', ?,
                   '[]', '{\"matches\":[],\"bytes_scanned\":0,\"bytes_redacted\":0}')",
        ["gc-coverage".into(), "cp-gc-coverage".into(), old.into()],
    ))
    .await
    .expect("seed conflict evidence for final claim");
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_import_identity (
            identity_id, agent_kind, provider_session_id, source_kind, source_id,
            schema_version, observed_digest, committed_digest, attempt_checkpoint_id,
            next_ordinal, state, fence_token, created_at, updated_at
         ) VALUES ('identity-gc', 'claude_code', 'provider-gc-coverage',
                   'file', 'projects/gc.jsonl', 1, 'digest', 'digest', ?, 1,
                   'committed', 1, ?, ?)",
        ["cp-gc-coverage".into(), old.into(), old.into()],
    ))
    .await
    .expect("seed import identity");
    conn.close().await.expect("close seed db");

    let dry = run_libra_command(
        &[
            "--quiet",
            "agent",
            "clean",
            "--gc",
            "--dry-run",
            "--retention-days",
            "1",
            "--json",
        ],
        repo.path(),
    );
    assert_cli_success(&dry, "agent clean claims dry-run");
    assert_eq!(
        parse_json_stdout(&dry)["data"]["import_identities_pruned"],
        1,
        "dry-run must preview the identity made ownerless by its selected final checkpoint"
    );
    let conn = connect_repo_db(repo.path()).await;
    assert_eq!(
        scalar_count(&conn, "SELECT COUNT(*) AS n FROM agent_coverage_claim").await,
        1
    );
    assert_eq!(
        scalar_count(&conn, "SELECT COUNT(*) AS n FROM agent_coverage_revision").await,
        1
    );
    assert_eq!(
        scalar_count(&conn, "SELECT COUNT(*) AS n FROM agent_coverage_conflict").await,
        1,
        "dry-run must retain durable conflict evidence"
    );
    assert_eq!(
        scalar_count(&conn, "SELECT COUNT(*) AS n FROM agent_import_identity").await,
        1,
        "dry-run must retain import recovery identity"
    );
    conn.close().await.expect("close dry-run db");

    let clean = run_libra_command(
        &["--quiet", "agent", "clean", "--gc", "--retention-days", "1"],
        repo.path(),
    );
    assert_cli_success(&clean, "agent clean claims gc");
    let conn = connect_repo_db(repo.path()).await;
    assert_eq!(
        scalar_count(&conn, "SELECT COUNT(*) AS n FROM agent_coverage_claim").await,
        0
    );
    assert_eq!(
        scalar_count(&conn, "SELECT COUNT(*) AS n FROM agent_coverage_revision").await,
        0
    );
    assert_eq!(
        scalar_count(&conn, "SELECT COUNT(*) AS n FROM agent_coverage_conflict").await,
        0,
        "pruning a final incumbent must remove its now-unresolvable challenger evidence"
    );
    assert_eq!(
        scalar_count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_import_identity WHERE identity_id = 'identity-gc'"
        )
        .await,
        0,
        "GC must physically delete an identity whose final coverage claim was pruned"
    );
}

#[tokio::test]
async fn checkpoint_prune_rewinds_conflicted_claim_and_removes_stale_challenger() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());
    let conn = connect_repo_db(repo.path()).await;
    seed_session(&conn, "gc-conflict-rewind", "stopped", 1, 2, 3).await;
    let first_commit = seed_checkpoint_commit(
        &conn,
        repo.path(),
        "cp-conflict-first",
        "gc-conflict-rewind",
        CheckpointScope::Committed,
        10,
    )
    .await;
    let second_commit = seed_checkpoint_commit(
        &conn,
        repo.path(),
        "cp-conflict-second",
        "gc-conflict-rewind",
        CheckpointScope::Temporary,
        20,
    )
    .await;
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_coverage_claim (
            session_id, logical_turn_key, coverage_schema_version,
            coverage_digest, completeness, revision, state, checkpoint_id,
            traces_commit, source_channel, created_at, updated_at
         ) VALUES ('gc-conflict-rewind', 'turn-rewind', 1, 'digest-second',
                   'complete', 2, 'conflicted', 'cp-conflict-second', ?,
                   'import', 10, 20)",
        [second_commit.into()],
    ))
    .await
    .expect("seed current conflicted claim");
    for (revision, checkpoint_id, digest, created_at) in [
        (1_i64, "cp-conflict-first", "digest-first", 10_i64),
        (2_i64, "cp-conflict-second", "digest-second", 20_i64),
    ] {
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_coverage_revision (
                session_id, logical_turn_key, coverage_schema_version, revision,
                checkpoint_id, coverage_digest, completeness, source_channel, created_at
             ) VALUES ('gc-conflict-rewind', 'turn-rewind', 1, ?, ?, ?,
                       'complete', 'import', ?)",
            [
                revision.into(),
                checkpoint_id.into(),
                digest.into(),
                created_at.into(),
            ],
        ))
        .await
        .expect("seed coverage revision");
    }
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_coverage_conflict (
            session_id, logical_turn_key, coverage_schema_version,
            incumbent_revision, incumbent_digest, incumbent_checkpoint_id,
            incoming_digest, incoming_source_channel, incoming_observed_at,
            incoming_canonical_json, incoming_redaction_report_json
         ) VALUES ('gc-conflict-rewind', 'turn-rewind', 1, 2, 'digest-second',
                   'cp-conflict-second', 'digest-challenger', 'import', 21,
                   '[]', '{\"matches\":[],\"bytes_scanned\":0,\"bytes_redacted\":0}')"
            .to_string(),
    ))
    .await
    .expect("seed challenger against second revision");

    let repo_path = repo.path().join(".libra");
    let manager = HistoryManager::new_with_ref(
        Arc::new(ClientStorage::init(repo_path.join("objects"))),
        repo_path,
        Arc::new(conn.clone()),
        TRACES_BRANCH,
    );
    let outcome = manager
        .prune_checkpoint_commits(&["cp-conflict-second".to_string()])
        .await
        .expect("prune conflicted incumbent checkpoint");
    assert_eq!(outcome.removed_checkpoints, 1);
    assert_eq!(
        scalar_count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_checkpoint_prune_tombstone
             WHERE checkpoint_id = 'cp-conflict-second'"
        )
        .await,
        1,
        "ordinary prune must leave a durable cloud anti-resurrection fence"
    );
    let claim = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT state, revision, checkpoint_id, coverage_digest, traces_commit
             FROM agent_coverage_claim WHERE session_id = 'gc-conflict-rewind'"
                .to_string(),
        ))
        .await
        .expect("query rewound claim")
        .expect("rewound claim row");
    assert_eq!(
        claim.try_get_by::<String, _>("state").unwrap(),
        "catalog_committed"
    );
    assert_eq!(claim.try_get_by::<i64, _>("revision").unwrap(), 1);
    assert_eq!(
        claim.try_get_by::<String, _>("checkpoint_id").unwrap(),
        "cp-conflict-first"
    );
    assert_eq!(
        claim.try_get_by::<String, _>("coverage_digest").unwrap(),
        "digest-first"
    );
    let retained_checkpoint_row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT traces_commit, sync_revision FROM agent_checkpoint
             WHERE checkpoint_id = 'cp-conflict-first'"
                .to_string(),
        ))
        .await
        .expect("query retained checkpoint")
        .expect("retained checkpoint row");
    let retained_checkpoint = retained_checkpoint_row
        .try_get_by::<String, _>("traces_commit")
        .expect("decode retained checkpoint commit");
    assert_eq!(
        retained_checkpoint_row
            .try_get_by::<i64, _>("sync_revision")
            .expect("decode retained checkpoint cloud generation"),
        2,
        "a prune rewrite must advance the checkpoint replication generation"
    );
    assert_eq!(
        claim.try_get_by::<String, _>("traces_commit").unwrap(),
        retained_checkpoint,
        "rewound claim must follow the retained checkpoint's rewritten traces commit"
    );
    assert_ne!(
        retained_checkpoint, first_commit,
        "prune rewrites retained history rather than preserving the old descendant chain"
    );
    assert_eq!(
        scalar_count(&conn, "SELECT COUNT(*) AS n FROM agent_coverage_conflict").await,
        0,
        "challenger tied to the pruned incumbent revision must not survive the rewind"
    );
    assert_eq!(
        scalar_count(&conn, "SELECT COUNT(*) AS n FROM agent_coverage_revision").await,
        1
    );
}

#[tokio::test]
async fn agent_clean_gc_deletes_zero_checkpoint_import_identity() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());
    let conn = connect_repo_db(repo.path()).await;
    let now = chrono::Utc::now().timestamp_millis();
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_import_identity (
            identity_id, agent_kind, provider_session_id, source_kind, source_id,
            schema_version, next_ordinal, state, created_at, updated_at
         ) VALUES ('zero-checkpoint-identity', 'claude_code', 'never-captured',
                   'file', 'projects/never.jsonl', 1, 0, 'discovered', ?, ?)",
        [now.into(), now.into()],
    ))
    .await
    .expect("seed zero-checkpoint import identity");
    conn.close().await.expect("close seed db");

    let output = run_libra_command(&["agent", "clean", "--gc", "--json"], repo.path());
    assert_cli_success(&output, "agent clean zero-checkpoint identity gc");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["import_identities_pruned"], 1);
    let conn = connect_repo_db(repo.path()).await;
    assert_eq!(
        scalar_count(&conn, "SELECT COUNT(*) AS n FROM agent_import_identity").await,
        0
    );
}

#[tokio::test]
async fn agent_clean_default_only_drops_most_recent_stopped_session() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());

    let conn = connect_repo_db(repo.path()).await;
    seed_session(&conn, "older-stopped", "stopped", 10, 20, 30).await;
    seed_session(&conn, "newer-stopped", "stopped", 40, 50, 60).await;
    seed_session(&conn, "active-session", "active", 70, 80, 0).await;
    seed_checkpoint(&conn, "cp-older-temp", "older-stopped", "temporary", 200).await;
    seed_checkpoint(&conn, "cp-newer-temp", "newer-stopped", "temporary", 201).await;
    seed_checkpoint(&conn, "cp-active-temp", "active-session", "temporary", 202).await;
    conn.close().await.expect("close seed connection");

    let output = run_libra_command(&["--quiet", "agent", "clean"], repo.path());
    assert_cli_success(&output, "libra agent clean");

    let conn = connect_repo_db(repo.path()).await;
    assert!(
        checkpoint_exists(&conn, "cp-older-temp").await,
        "default clean should leave older stopped sessions for a later --all run"
    );
    assert!(
        !checkpoint_exists(&conn, "cp-newer-temp").await,
        "default clean should drop the most recently stopped session"
    );
    assert!(
        checkpoint_exists(&conn, "cp-active-temp").await,
        "default clean must not drop temporary checkpoints for active sessions"
    );
}

/// AG-20 window A/B guard: ANY live in-flight writer marker — even for a
/// session unrelated to the ones being pruned — blocks the prune
/// fail-closed (the prune is a whole-chain rewrite, so the granularity is
/// deliberately global). An expired marker stops blocking.
#[tokio::test]
async fn agent_clean_refuses_while_traces_write_marker_is_live() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());

    let conn = connect_repo_db(repo.path()).await;
    seed_session(&conn, "stopped-session", "stopped", 10, 20, 30).await;
    seed_checkpoint_commit(
        &conn,
        repo.path(),
        "cc000000-0000-4000-8000-000000000001",
        "stopped-session",
        CheckpointScope::Temporary,
        400,
    )
    .await;

    // Live marker for an UNRELATED session: still blocks (global guard).
    let now_ms = chrono::Utc::now().timestamp_millis();
    let marker = TracesInflightMarker::new("unrelated-active-session", "attempt-live-1", now_ms);
    write_traces_inflight_marker(&conn, &marker)
        .await
        .expect("write live in-flight marker");
    conn.close().await.expect("close seed connection");

    let output = run_libra_command(&["--quiet", "agent", "clean", "--all"], repo.path());
    assert!(
        !output.status.success(),
        "clean must fail closed while a live in-flight marker exists"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("checkpoint write is in flight"),
        "refusal must explain the in-flight write, stderr: {stderr}"
    );
    assert!(
        stderr.contains("unrelated-active-session") && stderr.contains("attempt-live-1"),
        "refusal must name the blocking marker's session and attempt, stderr: {stderr}"
    );
    assert!(
        stderr.contains(&AGENT_TRACES_INFLIGHT_TTL_MS.to_string()),
        "refusal must state the marker TTL, stderr: {stderr}"
    );

    let conn = connect_repo_db(repo.path()).await;
    assert!(
        checkpoint_exists(&conn, "cc000000-0000-4000-8000-000000000001").await,
        "a refused prune must not delete any checkpoint rows"
    );

    // Replace the marker with an expired one (upsert): the guard unblocks.
    let expired = TracesInflightMarker::new(
        "unrelated-active-session",
        "attempt-live-1",
        now_ms - AGENT_TRACES_INFLIGHT_TTL_MS - 60_000,
    );
    write_traces_inflight_marker(&conn, &expired)
        .await
        .expect("write expired in-flight marker");
    conn.close().await.expect("close reseed connection");

    let output = run_libra_command(&["--quiet", "agent", "clean", "--all"], repo.path());
    assert_cli_success(&output, "clean succeeds once the marker expired");

    let conn = connect_repo_db(repo.path()).await;
    assert!(
        !checkpoint_exists(&conn, "cc000000-0000-4000-8000-000000000001").await,
        "the temporary checkpoint should be pruned once no live marker remains"
    );
}

/// AG-20 window-B residue guard: when `refs/libra/traces` reaches commits
/// with no `agent_checkpoint` catalog row (a crashed writer between ref CAS
/// and catalog INSERT), the catalog-driven rebuild would drop them — the
/// prune must refuse deterministically and defer to doctor.
#[tokio::test]
async fn agent_clean_refuses_when_traces_ref_reaches_uncataloged_commits() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());

    let conn = connect_repo_db(repo.path()).await;
    seed_session(&conn, "stopped-session", "stopped", 10, 20, 30).await;
    seed_checkpoint_commit(
        &conn,
        repo.path(),
        "dd000000-0000-4000-8000-000000000001",
        "stopped-session",
        CheckpointScope::Temporary,
        500,
    )
    .await;
    seed_checkpoint_commit(
        &conn,
        repo.path(),
        "ee000000-0000-4000-8000-000000000002",
        "stopped-session",
        CheckpointScope::Committed,
        501,
    )
    .await;
    // Simulate window-B residue: the committed checkpoint's commit stays
    // reachable from the ref, but its catalog row vanishes.
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "DELETE FROM agent_checkpoint WHERE checkpoint_id = ?",
        [Value::from("ee000000-0000-4000-8000-000000000002")],
    ))
    .await
    .expect("drop catalog row to fabricate window-B residue");
    let head_before = agent_traces_head(&conn).await;
    conn.close().await.expect("close seed connection");

    let output = run_libra_command(&["--quiet", "agent", "clean", "--all"], repo.path());
    assert!(
        !output.status.success(),
        "clean must refuse while the ref reaches uncataloged commits"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no agent_checkpoint catalog row"),
        "refusal must explain the ref-vs-catalog mismatch, stderr: {stderr}"
    );
    assert!(
        stderr.contains("libra agent doctor --repair"),
        "refusal must point at doctor --repair, stderr: {stderr}"
    );

    let conn = connect_repo_db(repo.path()).await;
    assert!(
        checkpoint_exists(&conn, "dd000000-0000-4000-8000-000000000001").await,
        "a refused prune must not delete the targeted temporary checkpoint"
    );
    assert_eq!(
        agent_traces_head(&conn).await,
        head_before,
        "a refused prune must not move refs/libra/traces"
    );
}

async fn object_index_row_count(conn: &DatabaseConnection, oids: &[String]) -> i64 {
    let placeholders = vec!["?"; oids.len()].join(", ");
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            format!("SELECT COUNT(*) AS n FROM object_index WHERE o_id IN ({placeholders})"),
            oids.iter().map(|oid| Value::from(oid.clone())),
        ))
        .await
        .expect("query object_index")
        .expect("count row");
    row.try_get_by("n").expect("decode count")
}

async fn checkpoint_catalog_oids(conn: &DatabaseConnection, checkpoint_id: &str) -> Vec<String> {
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT traces_commit, tree_oid, metadata_blob_oid FROM agent_checkpoint \
             WHERE checkpoint_id = ? LIMIT 1",
            [Value::from(checkpoint_id)],
        ))
        .await
        .expect("query checkpoint oids")
        .expect("checkpoint row");
    ["traces_commit", "tree_oid", "metadata_blob_oid"]
        .into_iter()
        .map(|column| row.try_get_by::<String, _>(column).expect("decode oid"))
        .collect()
}

/// AG-20 object_index cleanup: pruning drops the index rows for OIDs the
/// prune made unreachable — conservatively only the removed checkpoints'
/// own commit/root-tree/metadata-blob OIDs — while retained checkpoints'
/// rows survive. Re-running is an idempotent no-op.
#[tokio::test]
async fn agent_clean_drops_object_index_rows_only_for_removed_checkpoints() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());

    let conn = connect_repo_db(repo.path()).await;
    seed_session(&conn, "stopped-session", "stopped", 10, 20, 30).await;
    seed_checkpoint_commit(
        &conn,
        repo.path(),
        "aa110000-0000-4000-8000-000000000001",
        "stopped-session",
        CheckpointScope::Temporary,
        600,
    )
    .await;
    seed_checkpoint_commit(
        &conn,
        repo.path(),
        "bb110000-0000-4000-8000-000000000002",
        "stopped-session",
        CheckpointScope::Committed,
        601,
    )
    .await;

    let removed_oids = checkpoint_catalog_oids(&conn, "aa110000-0000-4000-8000-000000000001").await;
    let retained_old_oids =
        checkpoint_catalog_oids(&conn, "bb110000-0000-4000-8000-000000000002").await;

    // The writer enqueues object_index rows on a background channel; wait
    // for the seeded checkpoints' rows to land so the delete has real rows
    // to act on.
    let mut all_oids = removed_oids.clone();
    all_oids.extend(retained_old_oids.iter().cloned());
    all_oids.sort();
    all_oids.dedup();
    let expected = all_oids.len() as i64;
    for _ in 0..200 {
        if object_index_row_count(&conn, &all_oids).await >= expected {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        object_index_row_count(&conn, &all_oids).await,
        expected,
        "seeded checkpoint OIDs must be indexed before the prune"
    );
    conn.close().await.expect("close seed connection");

    let output = run_libra_command(&["--json", "agent", "clean", "--all"], repo.path());
    assert_cli_success(&output, "libra agent clean --all (object_index cleanup)");
    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "agent_clean");
    assert_eq!(
        json["data"]["window_guard"], "markers_and_catalog_verified",
        "both prune guards must have run and passed"
    );
    assert_eq!(
        json["data"]["object_index_rows_dropped"], 3,
        "exactly the removed checkpoint's commit/tree/metadata rows are dropped"
    );

    let conn = connect_repo_db(repo.path()).await;
    assert_eq!(
        object_index_row_count(&conn, &removed_oids).await,
        0,
        "removed checkpoint OIDs must leave object_index"
    );
    assert_eq!(
        object_index_row_count(&conn, &retained_old_oids).await,
        retained_old_oids.len() as i64,
        "retained checkpoint rows are conservatively kept (shared-OID safety)"
    );
    conn.close().await.expect("close verify connection");

    // Idempotent: nothing left to prune, nothing further deleted.
    let output = run_libra_command(&["--json", "agent", "clean", "--all"], repo.path());
    assert_cli_success(&output, "second clean run is a no-op");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["window_guard"], "noop");
    assert_eq!(json["data"]["object_index_rows_dropped"], 0);
}

#[tokio::test]
async fn agent_clean_fails_closed_until_pending_index_marker_can_retire() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());
    let checkpoint_id = "cc110000-0000-4000-8000-000000000003";

    let conn = connect_repo_db(repo.path()).await;
    seed_session(&conn, "stopped-session", "stopped", 10, 20, 30).await;
    seed_checkpoint_commit(
        &conn,
        repo.path(),
        checkpoint_id,
        "stopped-session",
        CheckpointScope::Temporary,
        602,
    )
    .await;
    ClientStorage::wait_for_background_tasks();
    conn.close().await.expect("close seed connection");

    let indexed = run_libra_command_with_stdin_and_env(
        &["hash-object", "--stdin", "-w"],
        repo.path(),
        "marker retirement must fence cleanup",
        &[("LIBRA_TEST_OBJECT_INDEX_MARKER_RETIRE_FAIL", "1")],
    );
    assert_cli_success(
        &indexed,
        "local object write should complete with a repair warning",
    );
    assert!(
        String::from_utf8_lossy(&indexed.stderr).contains("durable repair queue"),
        "marker retirement failure should be reported: {}",
        String::from_utf8_lossy(&indexed.stderr)
    );

    let blocked = run_libra_command_with_stdin_and_env(
        &["agent", "clean", "--all"],
        repo.path(),
        "",
        &[("LIBRA_TEST_OBJECT_INDEX_MARKER_RETIRE_FAIL", "1")],
    );
    assert!(
        !blocked.status.success(),
        "destructive cleanup must not run while a repair marker cannot retire"
    );
    let blocked_stderr = String::from_utf8_lossy(&blocked.stderr);
    assert!(
        blocked_stderr.contains(
            "cannot run this operation while durable local object-index repair is pending"
        ) && blocked_stderr.contains("LBR-IO-002"),
        "unexpected fail-closed error: {blocked_stderr}"
    );
    let conn = connect_repo_db(repo.path()).await;
    assert!(
        checkpoint_exists(&conn, checkpoint_id).await,
        "preflight failure must occur before checkpoint and object-index deletion"
    );
    conn.close().await.expect("close blocked-state connection");

    let repaired = run_libra_command(&["status", "--short"], repo.path());
    assert_cli_success(
        &repaired,
        "ordinary command should retire the marker on retry",
    );
    let cleaned = run_libra_command(&["agent", "clean", "--all"], repo.path());
    assert_cli_success(&cleaned, "cleanup should proceed after marker retirement");
    let conn = connect_repo_db(repo.path()).await;
    assert!(
        !checkpoint_exists(&conn, checkpoint_id).await,
        "checkpoint should be pruned only after repair ownership is retired"
    );
}

#[tokio::test]
async fn agent_clean_rewrites_agent_traces_when_temporary_commit_is_ancestor() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());

    let conn = connect_repo_db(repo.path()).await;
    seed_session(&conn, "stopped-session", "stopped", 10, 20, 30).await;
    let temporary_commit = seed_checkpoint_commit(
        &conn,
        repo.path(),
        "aa000000-0000-4000-8000-000000000001",
        "stopped-session",
        CheckpointScope::Temporary,
        300,
    )
    .await;
    let original_committed_commit = seed_checkpoint_commit(
        &conn,
        repo.path(),
        "bb000000-0000-4000-8000-000000000002",
        "stopped-session",
        CheckpointScope::Committed,
        301,
    )
    .await;
    conn.close().await.expect("close seed connection");

    let output = run_libra_command(&["--quiet", "agent", "clean", "--all"], repo.path());
    assert_cli_success(&output, "libra agent clean --all");

    let conn = connect_repo_db(repo.path()).await;
    assert!(
        !checkpoint_exists(&conn, "aa000000-0000-4000-8000-000000000001").await,
        "temporary checkpoint row should be deleted"
    );
    let rewritten_committed_commit =
        checkpoint_traces_commit(&conn, "bb000000-0000-4000-8000-000000000002")
            .await
            .expect("committed checkpoint should remain");
    assert_ne!(
        rewritten_committed_commit, original_committed_commit,
        "retained committed checkpoints must be re-pointed at the rewritten history"
    );
    let head = agent_traces_head(&conn)
        .await
        .expect("traces should still point at the retained committed checkpoint");
    assert_eq!(head, rewritten_committed_commit);

    let reachable = reachable_agent_trace_commits(repo.path(), &head);
    assert!(
        !reachable.contains(&temporary_commit),
        "temporary checkpoint commit must become unreachable from traces"
    );
    assert!(
        reachable.contains(&rewritten_committed_commit),
        "rewritten committed checkpoint must remain reachable"
    );
    assert!(
        !reachable.contains(&original_committed_commit),
        "the old committed commit descended from the temporary checkpoint and must be replaced"
    );
}

/// An importer commits its leased identity before reserving turn claims. A
/// concurrent checkpoint prune in that window must not mistake the absence
/// of claims for terminal, ownerless recovery state.
#[tokio::test]
async fn checkpoint_prune_preserves_leased_import_identity_before_claim_registration() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());

    let conn = connect_repo_db(repo.path()).await;
    seed_session(&conn, "prune-race", "stopped", 10, 20, 30).await;
    let checkpoint_id = "cc000000-0000-4000-8000-000000000003";
    seed_checkpoint_commit(
        &conn,
        repo.path(),
        checkpoint_id,
        "prune-race",
        CheckpointScope::Temporary,
        302,
    )
    .await;
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_import_identity (
            identity_id, agent_kind, provider_session_id, source_kind, source_id,
            schema_version, observed_digest, next_ordinal, state, owner,
            lease_expires_at, fence_token, created_at, updated_at
         ) VALUES ('identity-prune-race', 'claude_code', 'provider-prune-race',
                   'file', 'projects/prune-race.jsonl', 1, 'digest', 0,
                   'leased', 'active-importer', 9999999999999, 1, 1, 1)",
        Vec::<Value>::new(),
    ))
    .await
    .expect("seed leased import identity before claim registration");

    let repo_path = repo.path().join(".libra");
    let manager = HistoryManager::new_with_ref(
        Arc::new(ClientStorage::init(repo_path.join("objects"))),
        repo_path,
        Arc::new(conn.clone()),
        TRACES_BRANCH,
    );
    let outcome = manager
        .prune_checkpoint_commits(&[checkpoint_id.to_string()])
        .await
        .expect("prune checkpoint while importer holds its pre-claim lease");
    assert_eq!(outcome.removed_checkpoints, 1);
    assert_eq!(
        outcome.deleted_import_identities, 0,
        "checkpoint prune must not collect an actively leased import identity"
    );
    assert_eq!(
        scalar_count(
            &conn,
            "SELECT COUNT(*) AS n FROM agent_import_identity
             WHERE identity_id = 'identity-prune-race'
               AND state = 'leased' AND owner = 'active-importer'"
        )
        .await,
        1,
        "active identity must survive until its importer reserves claims or releases the lease"
    );
}

/// AG-24a retention GC (`agent clean --gc`): drops checkpoints older than
/// the transcript retention window from stopped sessions REGARDLESS of
/// scope (unlike the default/temporary path), leaves recent ones, and
/// never touches the append-only `agent_audit_log`.
#[tokio::test]
async fn agent_clean_gc_drops_expired_all_scopes_and_keeps_audit_log() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());

    let now = chrono::Utc::now().timestamp();
    let ancient = now - 200 * 86_400; // 200 days ago — past the 90-day window
    let recent = now - 3 * 86_400; // 3 days ago — inside the window

    let conn = connect_repo_db(repo.path()).await;
    seed_session(&conn, "stopped-session", "stopped", 10, 20, 30).await;
    // Expired: both a temporary AND a committed checkpoint must be swept.
    seed_checkpoint(
        &conn,
        "cp-old-temp",
        "stopped-session",
        "temporary",
        ancient,
    )
    .await;
    seed_checkpoint(
        &conn,
        "cp-old-committed",
        "stopped-session",
        "committed",
        ancient,
    )
    .await;
    // Recent: inside the window, must survive.
    seed_checkpoint(
        &conn,
        "cp-recent-committed",
        "stopped-session",
        "committed",
        recent,
    )
    .await;
    // An audit row that GC must never delete.
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_audit_log \
         (audit_id, timestamp, action, checkpoint_id, scope, granted) \
         VALUES (?, ?, ?, ?, ?, ?)",
        vec![
            Value::from("audit-1"),
            Value::from("2026-07-05T00:00:00Z"),
            Value::from("raw_export"),
            Value::from("cp-old-committed"),
            Value::from("transcript"),
            Value::from(1i64),
        ],
    ))
    .await
    .expect("seed audit row");
    conn.close().await.expect("close seed connection");

    let output = run_libra_command(&["--quiet", "agent", "clean", "--gc"], repo.path());
    assert_cli_success(&output, "libra agent clean --gc");

    let conn = connect_repo_db(repo.path()).await;
    assert!(
        !checkpoint_exists(&conn, "cp-old-temp").await,
        "GC drops expired temporary checkpoints"
    );
    assert!(
        !checkpoint_exists(&conn, "cp-old-committed").await,
        "GC drops expired committed checkpoints (scope-agnostic)"
    );
    assert!(
        checkpoint_exists(&conn, "cp-recent-committed").await,
        "GC keeps checkpoints inside the retention window"
    );
    // The audit log survives GC untouched.
    let audit = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT COUNT(*) AS n FROM agent_audit_log WHERE audit_id = ?",
            [Value::from("audit-1")],
        ))
        .await
        .expect("query audit")
        .expect("row");
    assert_eq!(
        audit.try_get_by::<i64, _>("n").expect("count"),
        1,
        "GC must never delete agent_audit_log rows"
    );
}

/// `--retention-days` overrides the configured window and `--retention-days 0`
/// is rejected as usage.
#[tokio::test]
async fn agent_clean_gc_retention_days_override_and_zero_rejected() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());

    let now = chrono::Utc::now().timestamp();
    let two_days_ago = now - 2 * 86_400;

    let conn = connect_repo_db(repo.path()).await;
    seed_session(&conn, "stopped-session", "stopped", 10, 20, 30).await;
    seed_checkpoint(&conn, "cp-2d", "stopped-session", "committed", two_days_ago).await;
    conn.close().await.expect("close seed connection");

    // Window of 1 day → the 2-day-old checkpoint is expired.
    let output = run_libra_command(
        &["--quiet", "agent", "clean", "--gc", "--retention-days", "1"],
        repo.path(),
    );
    assert_cli_success(&output, "libra agent clean --gc --retention-days 1");
    let conn = connect_repo_db(repo.path()).await;
    assert!(
        !checkpoint_exists(&conn, "cp-2d").await,
        "--retention-days 1 expires a 2-day-old checkpoint"
    );
    conn.close().await.expect("close");

    // --retention-days 0 is a usage error.
    let output = run_libra_command(
        &["agent", "clean", "--gc", "--retention-days", "0"],
        repo.path(),
    );
    assert!(
        !output.status.success(),
        "--retention-days 0 must be rejected"
    );
}

/// Materialize an `agent-runs/<run_id>/` directory with a shared E8-style
/// `manifest.json`, aggregate files and reviewer stdout/stderr logs — enough
/// for the stderr-window GC to classify it.
fn write_agent_run(runs_root: &Path, run_id: &str, terminal_state: Option<&str>, updated_at: &str) {
    let run_dir = runs_root.join(run_id);
    std::fs::create_dir_all(run_dir.join("reviewers")).expect("create run dir");
    let terminal = match terminal_state {
        Some(state) => format!("\"{state}\""),
        None => "null".to_string(),
    };
    let manifest = format!(
        "{{\"schema_version\":1,\"run_id\":\"{run_id}\",\"kind\":\"review\",\
         \"terminal_state\":{terminal},\"created_at\":\"{updated_at}\",\
         \"updated_at\":\"{updated_at}\"}}"
    );
    std::fs::write(run_dir.join("manifest.json"), manifest).expect("write manifest");
    std::fs::write(run_dir.join("state.json"), "{}").expect("write state");
    std::fs::write(run_dir.join("findings.md"), "# findings").expect("write findings");
    std::fs::write(
        run_dir.join("reviewers/codex.stderr.redacted.log"),
        "reviewer stderr diagnostic",
    )
    .expect("write stderr log");
    std::fs::write(
        run_dir.join("reviewers/codex.stdout.redacted.log"),
        "reviewer stdout findings",
    )
    .expect("write stdout log");
}

/// AG-24a stderr window (`agent clean --gc`, Task A8.6): prunes reviewer stderr
/// diagnostic logs of aged **terminal** review/investigate runs while preserving
/// the aggregate record (manifest/state/findings + stdout logs), and leaves
/// recent or in-flight runs untouched.
#[tokio::test]
async fn agent_clean_gc_prunes_aged_reviewer_stderr_logs_and_keeps_aggregate() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());

    let runs_root = repo.path().join(".libra/sessions/agent-runs");
    // Past the 30-day stderr window but INSIDE the 90-day findings window, so
    // the stderr blob is pruned while the aggregate record (and the whole run)
    // is preserved — the A0-09 findings GC only removes runs older than 90d.
    let stderr_expired = (chrono::Utc::now() - chrono::Duration::days(60))
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    let ancient = "2000-01-01T00:00:00.000000Z"; // used only for a non-terminal run
    let recent = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);

    write_agent_run(
        &runs_root,
        "aged-terminal-run",
        Some("success"),
        &stderr_expired,
    );
    write_agent_run(&runs_root, "recent-terminal-run", Some("success"), &recent);
    write_agent_run(&runs_root, "aged-inflight-run", None, ancient);

    let output = run_libra_command(&["--quiet", "agent", "clean", "--gc"], repo.path());
    assert_cli_success(&output, "libra agent clean --gc");

    // Aged terminal run: stderr diagnostic pruned; aggregate + stdout preserved.
    assert!(
        !runs_root
            .join("aged-terminal-run/reviewers/codex.stderr.redacted.log")
            .exists(),
        "aged terminal run's reviewer stderr log must be pruned"
    );
    assert!(
        runs_root
            .join("aged-terminal-run/reviewers/codex.stdout.redacted.log")
            .exists(),
        "stdout log (findings provenance) is preserved"
    );
    assert!(runs_root.join("aged-terminal-run/manifest.json").exists());
    assert!(runs_root.join("aged-terminal-run/findings.md").exists());
    // Recent terminal + aged in-flight runs: stderr preserved.
    assert!(
        runs_root
            .join("recent-terminal-run/reviewers/codex.stderr.redacted.log")
            .exists(),
        "a run inside the stderr window is untouched"
    );
    assert!(
        runs_root
            .join("aged-inflight-run/reviewers/codex.stderr.redacted.log")
            .exists(),
        "an in-flight (non-terminal) run's diagnostics are never pruned"
    );
}

/// Write a terminal review run with an objectized findings blob (A0-06 shape):
/// manifest carries `findings_oid`, and the loose blob object exists under
/// `.libra/objects/`. Returns the findings OID.
fn write_agent_run_with_findings(
    repo: &Path,
    run_id: &str,
    terminal_state: Option<&str>,
    updated_at: &str,
) -> String {
    let run_dir = repo.join(".libra/sessions/agent-runs").join(run_id);
    std::fs::create_dir_all(run_dir.join("reviewers")).expect("create run dir");
    let content = format!("# findings for {run_id}\n");
    let oid =
        libra::utils::object::write_git_object(&repo.join(".libra"), "blob", content.as_bytes())
            .expect("write findings blob")
            .to_string();
    let terminal = match terminal_state {
        Some(state) => format!("\"{state}\""),
        None => "null".to_string(),
    };
    let manifest = format!(
        "{{\"schema_version\":1,\"run_id\":\"{run_id}\",\"kind\":\"review\",\
         \"terminal_state\":{terminal},\"created_at\":\"{updated_at}\",\
         \"updated_at\":\"{updated_at}\",\"findings_oid\":\"{oid}\",\"manual_attach\":[]}}"
    );
    std::fs::write(run_dir.join("manifest.json"), manifest).expect("write manifest");
    std::fs::write(run_dir.join("state.json"), "{}").expect("write state");
    std::fs::write(run_dir.join("findings.md"), &content).expect("write findings");
    oid
}

/// A0-09: `agent clean --gc` removes aged terminal review/investigate run
/// directories (the objectized findings blob is content-addressed and left for
/// a future repo-wide object GC), keeps recent/in-flight runs, previews under
/// `--dry-run`, and is idempotent.
#[tokio::test]
async fn agent_clean_findings_retention_gc() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());
    let runs_root = repo.path().join(".libra/sessions/agent-runs");
    let objects_root = repo.path().join(".libra/objects");
    let ancient = "2000-01-01T00:00:00.000000Z"; // far past the 90-day window
    let recent = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);

    let aged_oid = write_agent_run_with_findings(repo.path(), "aged-run", Some("success"), ancient);
    let recent_oid =
        write_agent_run_with_findings(repo.path(), "recent-run", Some("success"), &recent);
    let inflight_oid = write_agent_run_with_findings(repo.path(), "inflight-run", None, ancient);

    let loose = |oid: &str| objects_root.join(&oid[..2]).join(&oid[2..]);
    assert!(loose(&aged_oid).exists(), "findings blob written");

    // Dry-run: previews the aged run without deleting anything.
    let dry = run_libra_command(
        &["agent", "clean", "--gc", "--dry-run", "--json"],
        repo.path(),
    );
    assert_cli_success(&dry, "clean --gc --dry-run");
    let json = parse_json_stdout(&dry);
    assert_eq!(json["data"]["dry_run"], true);
    assert_eq!(
        json["data"]["findings_runs_pruned"], 1,
        "dry-run previews the aged run: {json}"
    );
    assert!(
        runs_root.join("aged-run").exists(),
        "dry-run deletes nothing"
    );
    assert!(loose(&aged_oid).exists(), "dry-run keeps the blob");

    // Real GC.
    let out = run_libra_command(&["agent", "clean", "--gc", "--json"], repo.path());
    assert_cli_success(&out, "clean --gc");
    let json = parse_json_stdout(&out);
    assert_eq!(
        json["data"]["findings_runs_pruned"], 1,
        "one aged run pruned: {json}"
    );
    assert_eq!(json["data"]["dry_run"], false);

    // Aged terminal run DIR: gone — and since PD-04 so is the findings blob
    // it owned. Leaving it meant the retention window expired while the
    // bytes stayed in the repository forever. Reclamation is still
    // conservative: an oid a surviving run names, or one history reaches,
    // is never deleted (see the unit test in `agent::clean`).
    assert!(!runs_root.join("aged-run").exists(), "aged run dir removed");
    assert!(
        !loose(&aged_oid).exists(),
        "the aged run's findings blob is reclaimed with it, not leaked"
    );
    // Recent terminal + aged in-flight runs: KEPT.
    assert!(runs_root.join("recent-run").exists(), "recent run kept");
    assert!(loose(&recent_oid).exists(), "recent blob kept");
    assert!(
        runs_root.join("inflight-run").exists(),
        "an in-flight (non-terminal) run is never GC'd"
    );
    assert!(loose(&inflight_oid).exists());

    // Idempotent: a second sweep finds nothing more to prune (missing-object safe).
    let again = run_libra_command(&["agent", "clean", "--gc", "--json"], repo.path());
    assert_cli_success(&again, "clean --gc idempotent");
    assert_eq!(parse_json_stdout(&again)["data"]["findings_runs_pruned"], 0);
}

/// A0-09 (codex P1): a findings/attachment blob shared (content-addressed) by
/// an expired run AND a retained run must NOT be deleted when the expired run
/// is GC'd — the retained run still references it.
#[tokio::test]
async fn agent_clean_findings_gc_keeps_shared_object_of_retained_run() {
    let repo = tempfile::tempdir().expect("repo tempdir");
    init_repo_via_cli(repo.path());
    let runs_root = repo.path().join(".libra/sessions/agent-runs");
    let objects_root = repo.path().join(".libra/objects");
    let ancient = "2000-01-01T00:00:00.000000Z";
    let recent = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);

    // Identical findings + attachment content in both runs → shared OIDs.
    let shared = "# identical shared findings\n";
    let oid = libra::utils::object::write_git_object(
        &repo.path().join(".libra"),
        "blob",
        shared.as_bytes(),
    )
    .expect("write shared findings blob")
    .to_string();
    let attach = "shared attachment body\n";
    let attach_oid = libra::utils::object::write_git_object(
        &repo.path().join(".libra"),
        "blob",
        attach.as_bytes(),
    )
    .expect("write shared attach blob")
    .to_string();

    let write_shared = |run_id: &str, updated: &str| {
        let run_dir = runs_root.join(run_id);
        std::fs::create_dir_all(run_dir.join("reviewers")).unwrap();
        let manifest = format!(
            "{{\"schema_version\":1,\"run_id\":\"{run_id}\",\"kind\":\"review\",\
             \"terminal_state\":\"success\",\"created_at\":\"{updated}\",\"updated_at\":\"{updated}\",\
             \"findings_oid\":\"{oid}\",\"manual_attach\":[{{\"oid\":\"{attach_oid}\",\
             \"name\":\"a\",\"provenance\":\"manual\",\"size\":1,\"attached_at\":\"{updated}\"}}]}}"
        );
        std::fs::write(run_dir.join("manifest.json"), manifest).unwrap();
        std::fs::write(run_dir.join("state.json"), "{}").unwrap();
        std::fs::write(run_dir.join("findings.md"), shared).unwrap();
    };
    write_shared("aged-run", ancient);
    write_shared("recent-run", &recent);

    let loose = |o: &str| objects_root.join(&o[..2]).join(&o[2..]);
    assert!(loose(&oid).exists() && loose(&attach_oid).exists());

    let out = run_libra_command(&["agent", "clean", "--gc", "--json"], repo.path());
    assert_cli_success(&out, "clean --gc");
    let json = parse_json_stdout(&out);
    assert_eq!(
        json["data"]["findings_runs_pruned"], 1,
        "only the aged run is pruned: {json}"
    );
    // Aged run gone; recent run kept.
    assert!(!runs_root.join("aged-run").exists());
    assert!(runs_root.join("recent-run").exists());
    // Shared objects SURVIVE — the retained run still references them.
    assert!(
        loose(&oid).exists(),
        "shared findings blob must be kept for the retained run"
    );
    assert!(
        loose(&attach_oid).exists(),
        "shared attach blob must be kept for the retained run"
    );
}
