//! Integration coverage for `libra agent push`.
//!
//! The external-agent capture plan reserves `refs/libra/traces` for
//! transport. This test keeps the wrapper pinned to that private destination
//! instead of accidentally publishing `traces` as a normal branch.

#[cfg(unix)]
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};

#[cfg(unix)]
use libra::{
    internal::{
        ai::{
            history::{CheckpointCommitParams, CheckpointScope, HistoryManager},
            observed_agents::Redactor,
        },
        branch::{Branch as InternalBranch, TRACES_BRANCH},
    },
    utils::{client_storage::ClientStorage, test::ChangeDirGuard},
};
#[cfg(unix)]
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement, Value};
#[cfg(unix)]
use serial_test::serial;

#[cfg(unix)]
fn libra_command(cwd: &Path) -> Command {
    let home = cwd.join(".libra-test-home");
    let config_home = home.join(".config");
    fs::create_dir_all(&config_home).expect("failed to create isolated HOME");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
    cmd.current_dir(cwd)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("USERPROFILE", &home);
    cmd
}

#[cfg(unix)]
fn create_fake_ssh_script(root: &Path) -> PathBuf {
    let script_path = root.join("fake_ssh.sh");
    let script = r#"#!/bin/sh
set -eu

remote_cmd=""
for arg in "$@"; do
  remote_cmd="$arg"
done

if [ -z "$remote_cmd" ]; then
  echo "missing remote command" >&2
  exit 2
fi

exec sh -c "$remote_cmd"
"#;
    fs::write(&script_path, script).expect("failed to write fake ssh script");

    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&script_path)
        .expect("failed to stat fake ssh script")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("failed to chmod fake ssh script");

    script_path
}

/// Init a repo with identity + one commit; returns HEAD. Does NOT touch
/// the traces branch — callers decide what the traces chain looks like.
#[cfg(unix)]
fn init_repo_base(local_dir: &Path) -> String {
    fs::create_dir_all(local_dir).expect("failed to create local repo dir");

    let init_out = libra_command(local_dir)
        .args(["init"])
        .output()
        .expect("failed to init local libra repo");
    assert!(
        init_out.status.success(),
        "local init failed: {}",
        String::from_utf8_lossy(&init_out.stderr)
    );

    for (key, value) in [
        ("user.name", "Agent Push Test"),
        ("user.email", "agent-push@example.com"),
    ] {
        let config_out = libra_command(local_dir)
            .args(["config", key, value])
            .output()
            .expect("failed to configure identity");
        assert!(
            config_out.status.success(),
            "config {key} failed: {}",
            String::from_utf8_lossy(&config_out.stderr)
        );
    }

    fs::write(local_dir.join("tracked.txt"), "agent traces source\n")
        .expect("failed to write tracked file");
    let add_out = libra_command(local_dir)
        .args(["add", "tracked.txt"])
        .output()
        .expect("failed to add tracked file");
    assert!(
        add_out.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&add_out.stderr)
    );

    let commit_out = libra_command(local_dir)
        .args(["commit", "-m", "base"])
        .output()
        .expect("failed to commit");
    assert!(
        commit_out.status.success(),
        "commit failed: {}",
        String::from_utf8_lossy(&commit_out.stderr)
    );

    let head_out = libra_command(local_dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("failed to read HEAD");
    assert!(
        head_out.status.success(),
        "rev-parse HEAD failed: {}",
        String::from_utf8_lossy(&head_out.stderr)
    );
    String::from_utf8(head_out.stdout)
        .expect("HEAD hash not utf8")
        .trim()
        .to_string()
}

#[cfg(unix)]
fn init_repo_with_agent_traces_tip(local_dir: &Path) -> String {
    let head = init_repo_base(local_dir);

    let _guard = ChangeDirGuard::new(local_dir);
    let runtime = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    runtime
        .block_on(InternalBranch::update_branch(TRACES_BRANCH, &head, None))
        .expect("failed to point traces branch at HEAD");

    head
}

#[cfg(unix)]
async fn connect_repo_db(repo: &Path) -> DatabaseConnection {
    let db_path = repo.join(".libra").join("libra.db");
    let mut opts = ConnectOptions::new(format!("sqlite://{}", db_path.display()));
    opts.sqlx_logging(false)
        .connect_timeout(Duration::from_secs(5));
    Database::connect(opts)
        .await
        .expect("connect repository database")
}

#[cfg(unix)]
async fn seed_stopped_session(conn: &DatabaseConnection, session_id: &str) {
    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at, stopped_at
         ) VALUES (?, 'claude_code', ?, 'stopped', '/tmp/libra-agent-push-test',
                   '{}', '{}', 10, 20, 30)",
        vec![
            Value::from(session_id),
            Value::from(format!("provider-{session_id}")),
        ],
    ))
    .await
    .expect("insert agent_session");
}

/// Append a real checkpoint commit on the traces branch and insert its
/// catalog row (same seeding shape as `agent_clean_test`).
#[cfg(unix)]
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
    let written = history
        .append_checkpoint_commit(CheckpointCommitParams {
            checkpoint_id,
            session_id,
            agent_kind: "claude_code",
            parent_commit: None,
            scope,
            tool_use_id: None,
            metadata_json: &meta_redacted,
            transcript_redacted: &redacted,
            lifecycle_events_jsonl: &events_redacted,
            redaction_report_json: &report_redacted,
        })
        .await
        .expect("append checkpoint commit");

    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (
            checkpoint_id, session_id, scope, parent_commit, tree_oid,
            metadata_blob_oid, traces_commit, created_at
         ) VALUES (?, ?, ?, NULL, ?, ?, ?, ?)",
        vec![
            Value::from(checkpoint_id),
            Value::from(session_id),
            Value::from(scope.as_str()),
            Value::from(written.tree_oid.to_string()),
            Value::from(written.metadata_blob_oid.to_string()),
            Value::from(written.commit_hash.to_string()),
            Value::from(created_at),
        ],
    ))
    .await
    .expect("insert agent_checkpoint");

    written.commit_hash.to_string()
}

#[cfg(unix)]
async fn local_traces_tip(conn: &DatabaseConnection) -> Option<String> {
    conn.query_one(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "SELECT `commit` FROM reference \
         WHERE name = ? AND kind = 'Branch' AND remote IS NULL LIMIT 1",
        [Value::from(TRACES_BRANCH)],
    ))
    .await
    .expect("query traces ref")
    .and_then(|row| row.try_get_by("commit").ok().flatten())
}

#[cfg(unix)]
fn remote_traces_ref(remote_dir: &Path) -> Option<String> {
    let out = Command::new("git")
        .args([
            "--git-dir",
            remote_dir.to_str().unwrap(),
            "rev-parse",
            "--verify",
            "refs/libra/traces",
        ])
        .output()
        .expect("failed to read remote traces ref");
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8(out.stdout)
            .expect("remote ref hash not utf8")
            .trim()
            .to_string(),
    )
}

#[cfg(unix)]
fn add_fake_ssh_remote(local_dir: &Path, remote_dir: &Path) {
    let ssh_remote = format!("git@fakehost:{}", remote_dir.to_string_lossy());
    let remote_out = libra_command(local_dir)
        .args(["remote", "add", "origin", &ssh_remote])
        .output()
        .expect("failed to add fake ssh remote");
    assert!(
        remote_out.status.success(),
        "remote add failed: {}",
        String::from_utf8_lossy(&remote_out.stderr)
    );
}

#[cfg(unix)]
#[test]
#[serial]
fn agent_push_writes_private_agent_traces_ref() {
    let temp_root = tempfile::tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let local_dir = temp_root.path().join("local");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );

    let local_head = init_repo_with_agent_traces_tip(&local_dir);
    add_fake_ssh_remote(&local_dir, &remote_dir);

    let push_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["agent", "push", "--remote", "origin"])
        .output()
        .expect("failed to run libra agent push");
    assert!(
        push_out.status.success(),
        "agent push failed: {}",
        String::from_utf8_lossy(&push_out.stderr)
    );

    let private_ref_out = Command::new("git")
        .args([
            "--git-dir",
            remote_dir.to_str().unwrap(),
            "rev-parse",
            "refs/libra/traces",
        ])
        .output()
        .expect("failed to read remote private ref");
    assert!(
        private_ref_out.status.success(),
        "remote refs/libra/traces should exist, stderr: {}",
        String::from_utf8_lossy(&private_ref_out.stderr)
    );
    let private_ref = String::from_utf8(private_ref_out.stdout)
        .expect("remote ref hash not utf8")
        .trim()
        .to_string();
    assert_eq!(private_ref, local_head);

    let public_branch_out = Command::new("git")
        .args([
            "--git-dir",
            remote_dir.to_str().unwrap(),
            "rev-parse",
            "--verify",
            "refs/heads/traces",
        ])
        .output()
        .expect("failed to check public traces branch");
    assert!(
        !public_branch_out.status.success(),
        "agent push must not create refs/heads/traces"
    );
}

/// AG-20 push-after-prune contract (plan.md Task A5, option (a)):
///
/// 1. push a real checkpoint chain, prune it with `agent clean` (whole-chain
///    rewrite), and the follow-up plain push is rejected WITH a hint naming
///    `libra agent push --force-rewrite`;
/// 2. `--force-rewrite` succeeds via force-with-lease against the recorded
///    last-pushed tip;
/// 3. a divergent remote update behind our back still fails closed even
///    with `--force-rewrite` (the lease protects).
#[cfg(unix)]
#[test]
#[serial]
fn agent_push_after_prune_requires_force_rewrite_and_lease_protects() {
    let temp_root = tempfile::tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let local_dir = temp_root.path().join("local");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );

    init_repo_base(&local_dir);
    add_fake_ssh_remote(&local_dir, &remote_dir);

    // Seed a real traces chain: one temporary + one committed checkpoint.
    let runtime = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    let old_tip = runtime.block_on(async {
        let conn = connect_repo_db(&local_dir).await;
        seed_stopped_session(&conn, "push-session").await;
        seed_checkpoint_commit(
            &conn,
            &local_dir,
            "aa000000-0000-4000-8000-000000000011",
            "push-session",
            CheckpointScope::Temporary,
            700,
        )
        .await;
        let committed = seed_checkpoint_commit(
            &conn,
            &local_dir,
            "bb000000-0000-4000-8000-000000000012",
            "push-session",
            CheckpointScope::Committed,
            701,
        )
        .await;
        conn.close().await.expect("close seed connection");
        committed
    });

    // Push #1 (fast-forward create) — records the lease basis.
    let push_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["agent", "push", "--remote", "origin"])
        .output()
        .expect("failed to run first agent push");
    assert!(
        push_out.status.success(),
        "initial agent push failed: {}",
        String::from_utf8_lossy(&push_out.stderr)
    );
    assert_eq!(remote_traces_ref(&remote_dir).as_deref(), Some(&*old_tip));

    // Prune: `agent clean` rewrites the whole traces chain.
    let clean_out = libra_command(&local_dir)
        .args(["agent", "clean", "--all"])
        .output()
        .expect("failed to run agent clean");
    assert!(
        clean_out.status.success(),
        "agent clean failed: {}",
        String::from_utf8_lossy(&clean_out.stderr)
    );
    let new_tip = runtime.block_on(async {
        let conn = connect_repo_db(&local_dir).await;
        let tip = local_traces_tip(&conn)
            .await
            .expect("traces branch should survive the prune");
        conn.close().await.expect("close tip connection");
        tip
    });
    assert_ne!(new_tip, old_tip, "the prune must have rewritten the chain");

    // Push #2 (plain): rejected, with the actionable --force-rewrite hint.
    let rejected_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["agent", "push", "--remote", "origin"])
        .output()
        .expect("failed to run post-prune agent push");
    assert!(
        !rejected_out.status.success(),
        "a post-prune plain push must be rejected (non-fast-forward)"
    );
    let stderr = String::from_utf8_lossy(&rejected_out.stderr);
    assert!(
        stderr.contains("libra agent push --force-rewrite"),
        "the rejection must point at --force-rewrite, stderr: {stderr}"
    );
    assert!(
        stderr.contains("Libra-managed"),
        "the rejection must explain why (prunes rewrite the managed ref), stderr: {stderr}"
    );
    assert_eq!(
        remote_traces_ref(&remote_dir).as_deref(),
        Some(&*old_tip),
        "a rejected push must not move the remote ref"
    );

    // Push #3 (--force-rewrite): force-with-lease against the recorded tip.
    let force_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["agent", "push", "--remote", "origin", "--force-rewrite"])
        .output()
        .expect("failed to run agent push --force-rewrite");
    assert!(
        force_out.status.success(),
        "agent push --force-rewrite failed: {}",
        String::from_utf8_lossy(&force_out.stderr)
    );
    assert_eq!(remote_traces_ref(&remote_dir).as_deref(), Some(&*new_tip));

    // Divergent update behind our back: another writer moves the remote.
    assert!(
        Command::new("git")
            .args([
                "--git-dir",
                remote_dir.to_str().unwrap(),
                "update-ref",
                "refs/libra/traces",
                &old_tip,
            ])
            .status()
            .expect("failed to move remote ref behind our back")
            .success()
    );

    // Push #4 (--force-rewrite): the lease protects — still fails closed.
    let lease_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["agent", "push", "--remote", "origin", "--force-rewrite"])
        .output()
        .expect("failed to run diverged agent push --force-rewrite");
    assert!(
        !lease_out.status.success(),
        "--force-rewrite must fail when the remote moved past the recorded lease tip"
    );
    let stderr = String::from_utf8_lossy(&lease_out.stderr);
    assert!(
        stderr.contains("non-fast-forward"),
        "the lease rejection surfaces as a non-fast-forward refusal, stderr: {stderr}"
    );
    assert_eq!(
        remote_traces_ref(&remote_dir).as_deref(),
        Some(&*old_tip),
        "a lease-rejected push must not move the remote ref"
    );
}
