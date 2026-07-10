//! Integration coverage for `libra agent checkpoint` mutation paths.

use std::{fs, path::Path, time::Duration};

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement, Value};
use serial_test::serial;

use super::{
    ChangeDirGuard, Head, assert_cli_success, create_committed_repo_via_cli, parse_json_stdout,
    run_libra_command,
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

async fn seed_checkpoint_for_parent(conn: &DatabaseConnection, checkpoint_id: &str, parent: &str) {
    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at, stopped_at
         ) VALUES ('session-rewind', 'gemini', 'provider-rewind', 'stopped',
                   '/tmp/libra-agent-checkpoint-test', '{}', '{}', 10, 20, 30)",
        [],
    ))
    .await
    .expect("insert agent_session");

    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (
            checkpoint_id, session_id, scope, parent_commit, tree_oid,
            metadata_blob_oid, traces_commit, created_at
         ) VALUES (?, 'session-rewind', 'committed', ?, ?, ?, ?, 40)",
        vec![
            Value::from(checkpoint_id),
            Value::from(parent),
            Value::from("1111111111111111111111111111111111111111"),
            Value::from("2222222222222222222222222222222222222222"),
            Value::from("3333333333333333333333333333333333333333"),
        ],
    ))
    .await
    .expect("insert agent_checkpoint");
}

#[tokio::test]
#[serial]
async fn agent_checkpoint_rewind_dry_run_and_apply_restore_worktree_only() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());
    let base = Head::current_commit()
        .await
        .expect("base commit exists")
        .to_string();

    fs::write(repo.path().join("tracked.txt"), "changed\n").unwrap();
    fs::write(repo.path().join("extra.txt"), "extra\n").unwrap();
    let output = run_libra_command(&["add", "tracked.txt", "extra.txt"], repo.path());
    assert_cli_success(&output, "add second commit files");
    let output = run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path());
    assert_cli_success(&output, "create second commit");
    let head_before_rewind = Head::current_commit()
        .await
        .expect("second commit exists")
        .to_string();
    assert_ne!(base, head_before_rewind);

    let conn = connect_repo_db(repo.path()).await;
    seed_checkpoint_for_parent(&conn, "cp-rewind", &base).await;

    let output = run_libra_command(
        &[
            "--json",
            "agent",
            "checkpoint",
            "rewind",
            "cp-rewind",
            "--dry-run",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "agent checkpoint rewind dry-run");
    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "agent_checkpoint_rewind");
    assert_eq!(json["data"]["applied"], false);
    assert_eq!(json["data"]["parent_commit"], base);
    let restore_paths = json["data"]["would_restore_paths"].as_array().unwrap();
    assert!(restore_paths.iter().any(|path| path == "tracked.txt"));
    let delete_paths = json["data"]["would_delete_paths"].as_array().unwrap();
    assert!(delete_paths.iter().any(|path| path == "extra.txt"));
    assert_eq!(
        fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
        "changed\n"
    );
    assert!(repo.path().join("extra.txt").exists());
    assert_eq!(
        Head::current_commit().await.unwrap().to_string(),
        head_before_rewind
    );

    let output = run_libra_command(
        &[
            "--json",
            "agent",
            "checkpoint",
            "rewind",
            "cp-rewind",
            "--apply",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "agent checkpoint rewind apply");
    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "agent_checkpoint_rewind");
    assert_eq!(json["data"]["applied"], true);
    assert_eq!(json["data"]["transcript_truncation"]["supported"], false);
    assert_eq!(
        fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
        "tracked\n"
    );
    assert!(!repo.path().join("extra.txt").exists());
    assert_eq!(
        Head::current_commit().await.unwrap().to_string(),
        head_before_rewind
    );
}

/// A0-02: `checkpoint list` distinguishes a `scope='subagent'` checkpoint
/// from the session's `committed` checkpoints, and the subagent row carries
/// its parent-checkpoint linkage.
#[tokio::test]
#[serial]
async fn agent_checkpoint_subagent_scope_listed_and_linked() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());
    let conn = connect_repo_db(repo.path()).await;

    // A committed parent + a subagent child linked back to it.
    seed_checkpoint_for_parent(&conn, "cp-committed", "deadbeef").await;
    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (
            checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit,
            tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
            subagent_session_id, description, created_at
         ) VALUES ('cp-subagent', 'session-rewind', 'cp-committed', 'subagent', 'deadbeef',
                   '4444444444444444444444444444444444444444',
                   '5555555555555555555555555555555555555555',
                   '6666666666666666666666666666666666666666',
                   'Task', 'sub-run-9', 'subagent end via Task', 41)",
        [],
    ))
    .await
    .expect("insert subagent checkpoint");

    let output = run_libra_command(&["--json", "agent", "checkpoint", "list"], repo.path());
    assert_cli_success(&output, "agent checkpoint list");
    let json = parse_json_stdout(&output);
    let rows = json["data"]["checkpoints"]
        .as_array()
        .expect("checkpoints array");
    let scope_of = |id: &str| -> String {
        rows.iter()
            .find(|r| r["checkpoint_id"] == id)
            .and_then(|r| r["scope"].as_str())
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(scope_of("cp-committed"), "committed", "rows: {rows:?}");
    assert_eq!(scope_of("cp-subagent"), "subagent", "rows: {rows:?}");

    // Parent linkage persisted on the subagent row.
    let link = conn
        .query_one(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT parent_checkpoint_id FROM agent_checkpoint WHERE checkpoint_id = 'cp-subagent'",
            [],
        ))
        .await
        .expect("query linkage")
        .expect("subagent row present");
    assert_eq!(
        link.try_get_by::<String, _>("parent_checkpoint_id")
            .unwrap(),
        "cp-committed"
    );
}
