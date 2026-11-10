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
            history::{
                CheckpointCommitParams, CheckpointScope, HistoryManager, TracesInflightMarker,
                clear_traces_inflight_marker_if_generation, write_traces_inflight_marker,
            },
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
    conn.execute_raw(Statement::from_sql_and_values(
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
    let storage = Arc::new(ClientStorage::try_init(repo_path.join("objects")).expect("storage"));
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
            reasoning_artifacts: &[],
            checkpoint_id,
            session_id,
            marker_generation: marker.generation.as_deref().expect("new marker generation"),
            capture_scope: None,
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

    conn.execute_raw(Statement::from_sql_and_values(
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
    clear_traces_inflight_marker_if_generation(
        conn,
        session_id,
        checkpoint_id,
        &written.marker_generation,
    )
    .await
    .expect("retire seeded checkpoint writer marker");

    // Direct library writes outlive the foreground seed; the next CLI may
    // otherwise correctly refuse cleanup while their durable markers remain.
    wait_for_seeded_object_index(repo)
        .await
        .expect("seeded checkpoint object index must be ready before invoking the CLI");

    written.commit_hash.to_string()
}

#[cfg(unix)]
async fn local_traces_tip(conn: &DatabaseConnection) -> Option<String> {
    conn.query_one_raw(Statement::from_sql_and_values(
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
#[serial(cwd, env, hash_kind)]
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
#[serial(cwd, env, hash_kind)]
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

#[cfg(unix)]
async fn wait_for_seeded_object_index(repo: &Path) -> std::io::Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    if !ClientStorage::wait_for_background_tasks_until(deadline).await {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "seeded checkpoint object-index queue did not drain within 10 seconds",
        ));
    }

    // A drained queue may have failed terminally. Never repair or ignore its
    // durable markers here: that would conceal a fixture indexing failure.
    let marker_dir = repo.join(".libra/object-index-repair");
    let mut entries = match fs::read_dir(&marker_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(std::io::Error::new(
                error.kind(),
                format!(
                    "cannot inspect seeded checkpoint index markers at '{}': {error}",
                    marker_dir.display()
                ),
            ));
        }
    };
    if let Some(entry) = entries.next() {
        let entry = entry.map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!(
                    "cannot read seeded checkpoint index marker entry at '{}': {error}",
                    marker_dir.display()
                ),
            )
        })?;
        return Err(std::io::Error::other(format!(
            "seeded checkpoint index queue drained but still contains a durable marker: {}",
            entry.path().display()
        )));
    }
    Ok(())
}

#[cfg(unix)]
#[test]
#[serial(hash_kind)]
fn agent_push_seed_drains_object_index_queue() {
    let repo = tempfile::tempdir().expect("create seed-readiness fixture");
    init_repo_base(repo.path());
    let runtime = tokio::runtime::Runtime::new().expect("create seed-readiness runtime");
    runtime.block_on(async {
        let conn = connect_repo_db(repo.path()).await;
        seed_stopped_session(&conn, "seed-readiness-session").await;
        seed_checkpoint_commit(
            &conn,
            repo.path(),
            "cc000000-0000-4000-8000-000000000013",
            "seed-readiness-session",
            CheckpointScope::Temporary,
            702,
        )
        .await;

        let drained = ClientStorage::wait_for_background_tasks_until(
            std::time::Instant::now() + Duration::from_secs(10),
        )
        .await;
        conn.close().await.expect("close seed-readiness connection");
        assert!(drained, "seed-readiness regression cleanup did not drain");
        wait_for_seeded_object_index(repo.path())
            .await
            .expect("checkpoint seed should retire its durable index markers");
    });
}

#[cfg(unix)]
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn agent_push_seed_readiness_rejects_terminal_marker() {
    let repo = tempfile::tempdir().expect("create terminal-marker fixture");
    let marker_dir = repo.path().join(".libra/object-index-repair");
    fs::create_dir_all(&marker_dir).expect("create marker directory");
    let marker = marker_dir.join("pending.json");
    fs::write(&marker, b"fixture-only").expect("create durable marker witness");
    let error = wait_for_seeded_object_index(repo.path())
        .await
        .expect_err("a terminal marker must not be treated as ready");
    assert!(
        error
            .to_string()
            .contains("still contains a durable marker")
    );
    assert!(
        marker.exists(),
        "readiness must not remove or replay markers"
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn agent_push_seed_readiness_rejects_marker_path_io_error() {
    let repo = tempfile::tempdir().expect("create marker-path fixture");
    fs::create_dir(repo.path().join(".libra")).expect("create storage directory");
    fs::write(
        repo.path().join(".libra/object-index-repair"),
        b"not a directory",
    )
    .expect("create invalid marker-directory witness");
    let error = wait_for_seeded_object_index(repo.path())
        .await
        .expect_err("marker-directory I/O errors must not mean ready");
    assert_eq!(error.kind(), std::io::ErrorKind::NotADirectory);
    assert!(
        error
            .to_string()
            .contains("cannot inspect seeded checkpoint index markers")
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn agent_push_seed_readiness_accepts_missing_or_empty_marker_dir() {
    let repo = tempfile::tempdir().expect("create empty-marker fixture");
    wait_for_seeded_object_index(repo.path())
        .await
        .expect("a missing marker directory is ready");
    fs::create_dir_all(repo.path().join(".libra/object-index-repair"))
        .expect("create empty marker directory");
    wait_for_seeded_object_index(repo.path())
        .await
        .expect("an empty marker directory is ready");
}

/// LTR-12 wires `--mega2` and proves the force-rewrite guard never admits
/// a network attempt. The epoch is stored on the local attempt and reused.
#[cfg(unix)]
#[test]
fn mega2_scoped_push_contract() {
    let root = tempfile::tempdir().expect("repo");
    let repo = root.path();
    let output = libra_command(repo).args(["init"]).output().expect("init");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let blocked = libra_command(repo)
        .args(["agent", "push", "--mega2", "--force-rewrite"])
        .output()
        .expect("guard");
    assert!(
        !blocked.status.success(),
        "force-rewrite must fail before network"
    );
    let blocked_text = String::from_utf8_lossy(&blocked.stderr);
    assert!(
        blocked_text.contains("before any network"),
        "guard text missing: {blocked_text}"
    );

    let first = libra_command(repo)
        .args(["agent", "push", "--mega2", "--new-stream"])
        .output()
        .expect("first epoch");
    assert!(!first.status.success());
    let first_text = String::from_utf8_lossy(&first.stderr);
    assert!(
        first_text.contains("before any network attempt"),
        "first attempt must stop before the network: {first_text}"
    );
    let epoch = libra_command(repo)
        .args(["config", "get", "libra.traces.mega2.epoch"])
        .output()
        .expect("read epoch");
    assert!(
        epoch.status.success(),
        "{}",
        String::from_utf8_lossy(&epoch.stderr)
    );
    let epoch_value = String::from_utf8_lossy(&epoch.stdout).trim().to_string();
    assert_eq!(epoch_value.len(), 36, "epoch must be a UUID: {epoch_value}");

    let retry = libra_command(repo)
        .args(["agent", "push", "--mega2"])
        .output()
        .expect("retry");
    assert!(!retry.status.success());
    let retry_text = String::from_utf8_lossy(&retry.stderr);
    assert!(
        retry_text.contains(&epoch_value),
        "retry must reuse {epoch_value}: {retry_text}"
    );
    assert!(retry_text.contains("before any network attempt"));

    // Every remaining local precondition also stops before a socket: the
    // closed loopback port would turn a network attempt into another error.
    let stop = |args: &[&str], expected: &str| {
        let output = libra_command(repo)
            .env_remove("LIBRA_MEGA2_TOKEN")
            .args(args)
            .output()
            .expect("local stop");
        assert!(!output.status.success());
        let text = String::from_utf8_lossy(&output.stderr);
        assert!(text.contains(expected), "{expected}: {text}");
        assert!(text.contains("before any network attempt"), "{text}");
        assert!(text.contains(&epoch_value), "{text}");
    };
    let set_remote = |url: &str| {
        assert!(
            libra_command(repo)
                .args(["config", "set", "remote.origin.url", url])
                .output()
                .expect("remote")
                .status
                .success()
        );
    };
    set_remote("http://127.0.0.1:9");
    stop(
        &["agent", "push", "--mega2"],
        "does not name a Mega2 project path",
    );
    stop(
        &["agent", "push", "--mega2", "--target", "/project"],
        "has no checkpoint commit",
    );
    set_remote("ftp://127.0.0.1/project");
    stop(
        &["agent", "push", "--mega2"],
        "is not an http(s):// or ssh://",
    );
    let base = init_repo_base(&root.path().join("with-tip"));
    let tipped = root.path().join("with-tip");
    assert!(
        libra_command(&tipped)
            .args(["update-ref", "refs/heads/traces", &base])
            .output()
            .expect("update-ref")
            .status
            .success()
    );
    assert!(
        libra_command(&tipped)
            .args([
                "config",
                "set",
                "remote.origin.url",
                "http://127.0.0.1:9/project.git"
            ])
            .output()
            .expect("remote")
            .status
            .success()
    );
    let tokenless = libra_command(&tipped)
        .env_remove("LIBRA_MEGA2_TOKEN")
        .args(["agent", "push", "--mega2"])
        .output()
        .expect("tokenless");
    assert!(!tokenless.status.success());
    let text = String::from_utf8_lossy(&tokenless.stderr);
    assert!(text.contains("has no token"), "{text}");
    assert!(text.contains("before any network attempt"), "{text}");
}

/// Loopback receive-pack stub for the Mega2 traces receipt matrix. Each
/// request is answered from a script indexed by request order.
#[cfg(unix)]
type SeenRequests = std::sync::Arc<std::sync::Mutex<Vec<(String, Vec<u8>)>>>;

#[cfg(unix)]
fn serve_traces_receive(script: Vec<(u16, &'static str, Vec<u8>)>) -> (u16, SeenRequests) {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
    };
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for (status, content_type, payload) in script {
            let Ok((mut socket, _)) = listener.accept() else {
                break;
            };
            let _ = socket.set_read_timeout(Some(Duration::from_secs(5)));
            let mut request = Vec::new();
            let mut buffer = [0_u8; 8192];
            let header_end = loop {
                let read = socket.read(&mut buffer).unwrap_or(0);
                if read == 0 {
                    break None;
                }
                request.extend_from_slice(&buffer[..read]);
                if let Some(at) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break Some(at + 4);
                }
            };
            let Some(header_end) = header_end else {
                continue;
            };
            let head = String::from_utf8_lossy(&request[..header_end]).to_string();
            let length = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            while request.len() < header_end + length {
                let read = socket.read(&mut buffer).unwrap_or(0);
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
            log.lock()
                .unwrap()
                .push((path, request[header_end..].to_vec()));
            let mut response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            )
            .into_bytes();
            response.extend_from_slice(&payload);
            let _ = socket.write_all(&response);
        }
    });
    (port, seen)
}

#[cfg(unix)]
fn pkt_line(line: &str) -> Vec<u8> {
    format!("{:04x}{line}", line.len() + 4).into_bytes()
}

#[cfg(unix)]
fn receive_advertisement(tip: Option<&str>, caps: &str) -> (u16, &'static str, Vec<u8>) {
    let mut body = pkt_line("# service=git-receive-pack\n");
    body.extend_from_slice(b"0000");
    let line = match tip {
        Some(tip) => format!("{tip} refs/libra/traces\0{caps}\n"),
        None => format!("{} capabilities^{{}}\0{caps}\n", "0".repeat(40)),
    };
    body.extend(pkt_line(&line));
    body.extend_from_slice(b"0000");
    (200, "application/x-git-receive-pack-advertisement", body)
}

/// LTR-12: a lost scope read keeps the push successful as
/// `scope_resolution_pending`; the retry confirms the same tip and epoch;
/// a confirmed scope is reused through by-scope; a remote without
/// `libra-traces-v1` is refused after discovery.
#[cfg(unix)]
#[test]
fn mega2_receipt_recovery_matrix() {
    const SERVER: &str = "libra-traces-v1 libra-traces-server=11111111-1111-4111-8111-111111111111";
    const SCOPE: &str = "33333333-3333-4333-8333-333333333333";
    let temp = tempfile::tempdir().expect("root");
    let repo = temp.path().join("repo");
    let head = init_repo_base(&repo);
    let tip = libra_command(&repo)
        .args(["update-ref", "refs/heads/traces", &head])
        .output()
        .expect("update-ref");
    assert!(
        tip.status.success(),
        "{}",
        String::from_utf8_lossy(&tip.stderr)
    );
    let token = temp.path().join("token");
    fs::write(&token, "matrix-token\n").expect("token");
    let caps = format!("report-status no-thin agent=mega/0.1.0 object-format=sha1 {SERVER}");
    let scoped = format!("{caps} libra-traces-scope={SCOPE}");
    let mut report = pkt_line("unpack ok\n");
    report.extend(pkt_line("ok refs/libra/traces\n"));
    report.extend_from_slice(b"0000");
    let (port, seen) = serve_traces_receive(vec![
        receive_advertisement(None, &caps),
        (200, "application/x-git-receive-pack-result", report),
        (500, "text/plain", b"scope read lost".to_vec()),
        receive_advertisement(Some(&head), &scoped),
        receive_advertisement(Some(&head), &scoped),
    ]);
    let remote = format!("http://127.0.0.1:{port}/project/foo.git");
    assert!(
        libra_command(&repo)
            .args(["config", "set", "remote.origin.url", &remote])
            .output()
            .expect("remote")
            .status
            .success()
    );
    let push = |expect_scope_warning: bool| {
        let output = libra_command(&repo)
            .env("LIBRA_LOG", "warn")
            .env_remove("RUST_LOG")
            .env_remove("LIBRA_LOG_FILE")
            .args([
                "--json",
                "agent",
                "push",
                "--mega2",
                "--token-file",
                token.to_str().unwrap(),
            ])
            .output()
            .expect("push");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json = serde_json::from_slice::<serde_json::Value>(&output.stdout)
            .expect("stdout must contain only the command's JSON response");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            stderr.contains("traces scope discovery after push failed"),
            expect_scope_warning,
            "scope discovery diagnostics must be observable on stderr: {stderr}"
        );
        json["data"].clone()
    };

    let first = push(true);
    assert_eq!(first["state"], "scope_resolution_pending");
    assert_eq!(first["scope"], serde_json::Value::Null);
    assert_eq!(first["target"], "/project/foo");
    assert_eq!(first["up_to_date"], false);
    let epoch = first["epoch"].as_str().expect("epoch").to_string();

    let retry = push(false);
    assert_eq!(retry["state"], "confirmed");
    assert_eq!(retry["scope"], SCOPE);
    assert_eq!(
        retry["epoch"],
        epoch.as_str(),
        "a retry must not open an epoch"
    );
    assert_eq!(
        retry["up_to_date"], true,
        "the acknowledged tip is not re-sent"
    );
    assert!(
        retry["locator"]
            .as_str()
            .unwrap()
            .starts_with("/.traces/by-path/")
    );

    let reused = push(false);
    assert_eq!(
        reused["locator"],
        format!("/.traces/by-scope/{SCOPE}.git").as_str()
    );
    assert_eq!(reused["epoch"], epoch.as_str());

    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 5);
    assert!(requests[1].0.ends_with("/git-receive-pack"));
    let command = String::from_utf8_lossy(&requests[1].1);
    assert!(
        command.contains(&format!(
            "{} {head} refs/libra/traces\0report-status agent=libra/",
            "0".repeat(40)
        )),
        "{command}"
    );
    assert!(command.contains("PACK"), "the request must carry a pack");
    assert!(
        requests[4]
            .0
            .starts_with(&format!("/.traces/by-scope/{SCOPE}.git/"))
    );

    let (plain, _) = serve_traces_receive(vec![receive_advertisement(
        None,
        "report-status agent=git/2.43.0",
    )]);
    let refused_repo = temp.path().join("refused");
    let refused_head = init_repo_base(&refused_repo);
    assert!(
        libra_command(&refused_repo)
            .args(["update-ref", "refs/heads/traces", &refused_head])
            .output()
            .expect("update-ref")
            .status
            .success()
    );
    assert!(
        libra_command(&refused_repo)
            .args([
                "config",
                "set",
                "remote.origin.url",
                &format!("http://127.0.0.1:{plain}/project/foo.git"),
            ])
            .output()
            .expect("remote")
            .status
            .success()
    );
    let refused = libra_command(&refused_repo)
        .args([
            "agent",
            "push",
            "--mega2",
            "--token-file",
            token.to_str().unwrap(),
        ])
        .output()
        .expect("refused push");
    assert!(!refused.status.success());
    let text = String::from_utf8_lossy(&refused.stderr);
    assert!(text.contains("libra-traces-v1"), "{text}");
    assert!(!text.contains("stopped before"), "{text}");
}
