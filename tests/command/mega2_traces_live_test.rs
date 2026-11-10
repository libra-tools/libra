//! Real Mega2 fixture cases for traces. Default CI does not start them.
//! `cargo nextest run --run-ignored only` must execute the selected case.
//! A missing fixture fails the case. These tests do not start a stub server
//! and do not treat a local refusal as a successful upload or read.
//!
//! Push cases seed real checkpoint commits with the library writer, push them
//! with `libra agent push --mega2` over HTTP (token file) or SSH (fixture key
//! through `LIBRA_SSH_COMMAND`), and read old stream tips back with the
//! Git-based `libra mega2 traces fetch` into a fresh object store.
#![cfg(unix)]

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::Arc,
    time::Duration,
};

use libra::{
    internal::{
        ai::{
            history::{
                CheckpointCommitParams, CheckpointScope, HistoryManager, TracesInflightMarker,
                clear_traces_inflight_marker_if_generation, write_traces_inflight_marker,
            },
            observed_agents::Redactor,
        },
        branch::TRACES_BRANCH,
    },
    utils::client_storage::ClientStorage,
};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, Statement, Value};
use serde_json::Value as Json;

use super::mega2_traces_live_support::LiveFixture;

fn prepared() -> LiveFixture {
    let fixture = LiveFixture::require().unwrap_or_else(|message| panic!("{message}"));
    let binary =
        std::fs::read(env!("CARGO_BIN_EXE_libra")).expect("read the candidate libra binary");
    let actual = sha256_hex(&binary);
    assert_eq!(
        actual, fixture.client_binary_sha256,
        "the candidate binary hash does not match LIBRA_TRACES_LIVE_CLIENT_BINARY_SHA256"
    );
    let contract = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../mega2/docs/refactoring/traces.md"),
    )
    .expect("read the shared traces contract");
    assert_eq!(
        sha256_hex(&contract),
        fixture.contract_sha256,
        "the shared traces contract hash does not match LIBRA_TRACES_LIVE_CONTRACT_SHA256"
    );
    assert_eq!(fixture.server_id.len(), 36, "server id must be a UUID");
    assert_ne!(fixture.target, fixture.other_target);
    assert!(fixture.ssh_uri.starts_with("ssh://traces@"));
    assert!(
        fixture.ssh_key_file.is_file(),
        "primary SSH key file is missing"
    );
    assert!(
        fixture.peer_ssh_key_file.is_file(),
        "peer SSH key file is missing"
    );
    fixture
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn git_blob_sha1(bytes: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {}\0", bytes.len()).as_bytes());
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn run(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    super::run_libra_command_with_env(args, cwd, env)
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn ok(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed: {}",
        stderr(output)
    );
}

fn json(output: &Output) -> Json {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not one JSON document ({error}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// How one push or fetch reaches the fixture. Credentials are file paths;
/// no secret value enters the arguments or assertion text.
enum Transport {
    Http { token_file: PathBuf },
    Ssh { wrapper: PathBuf },
}

impl Transport {
    fn http(token_file: &Path) -> Self {
        Self::Http {
            token_file: token_file.to_path_buf(),
        }
    }

    /// `LIBRA_SSH_COMMAND` wrapper that pins the fixture key and a private
    /// known-hosts file for this case.
    fn ssh(dir: &Path, key: &Path, name: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let wrapper = dir.join(format!("{name}-ssh.sh"));
        let known = dir.join(format!("{name}-known_hosts"));
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexec /usr/bin/ssh -F /dev/null -o IdentitiesOnly=yes -i '{}' -o UserKnownHostsFile='{}' -o StrictHostKeyChecking=accept-new -o LogLevel=ERROR \"$@\"\n",
                key.display(),
                known.display()
            ),
        )
        .expect("write ssh wrapper");
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700))
            .expect("chmod ssh wrapper");
        Self::Ssh { wrapper }
    }

    fn remote_url(&self, fixture: &LiveFixture, target: &str) -> String {
        match self {
            Self::Http { .. } => format!("{}{target}.git", fixture.http_url.trim_end_matches('/')),
            Self::Ssh { .. } => format!(
                "ssh://git@{}{target}",
                fixture
                    .ssh_uri
                    .trim_start_matches("ssh://traces@")
                    .split('/')
                    .next()
                    .expect("ssh authority")
            ),
        }
    }

    fn args(&self) -> Vec<String> {
        match self {
            Self::Http { token_file } => {
                vec!["--token-file".to_string(), token_file.display().to_string()]
            }
            Self::Ssh { .. } => Vec::new(),
        }
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::Http { .. } => Vec::new(),
            Self::Ssh { wrapper } => vec![("LIBRA_SSH_COMMAND", wrapper.display().to_string())],
        }
    }
}

fn run_with(cwd: &Path, args: &[&str], transport: &Transport) -> Output {
    let extra = transport.args();
    let mut all: Vec<&str> = args.to_vec();
    all.extend(extra.iter().map(String::as_str));
    let env = transport.env();
    let env: Vec<(&str, &str)> = env
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    run(cwd, &all, &env)
}

fn set_remote(repo: &Path, url: &str) {
    ok(
        &run(repo, &["config", "set", "remote.origin.url", url], &[]),
        "set remote URL",
    );
}

fn init_repo(root: &Path, name: &str, remote_url: &str) -> PathBuf {
    let repo = root.join(name);
    std::fs::create_dir_all(&repo).expect("repo dir");
    ok(&run(&repo, &["init"], &[]), "init");
    for (key, value) in [
        ("user.name", "Traces Live"),
        ("user.email", "live@example.invalid"),
    ] {
        ok(&run(&repo, &["config", key, value], &[]), "identity");
    }
    set_remote(&repo, remote_url);
    repo
}

fn config_value(repo: &Path, key: &str) -> String {
    let output = run(repo, &["config", "get", key], &[]);
    ok(&output, &format!("config get {key}"));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// One real checkpoint commit appended by the library writer.
struct Seeded {
    commit: String,
    transcript: Vec<u8>,
    transcript_blob: String,
}

async fn seed(repo: &Path, session: &str, scope: CheckpointScope) -> Seeded {
    let gitdir = repo.join(".libra");
    let mut options =
        ConnectOptions::new(format!("sqlite://{}", gitdir.join("libra.db").display()));
    options
        .sqlx_logging(false)
        .connect_timeout(Duration::from_secs(5));
    let conn = Database::connect(options).await.expect("repository db");
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT OR IGNORE INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at, stopped_at
         ) VALUES (?, 'claude_code', ?, 'stopped', '/tmp/libra-traces-live',
                   '{}', '{}', 10, 20, 30)",
        vec![
            Value::from(session),
            Value::from(format!("provider-{session}")),
        ],
    ))
    .await
    .expect("insert stopped session");

    let checkpoint_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().timestamp();
    let storage = Arc::new(ClientStorage::try_init(gitdir.join("objects")).expect("storage"));
    let history = HistoryManager::new_with_ref(
        storage,
        gitdir.clone(),
        Arc::new(conn.clone()),
        TRACES_BRANCH,
    );
    let redactor = Redactor::new_default();
    let line = format!(
        "{{\"type\":\"assistant\",\"text\":\"live traces sentinel {checkpoint_id} connection timeout\"}}\n"
    );
    let (transcript, _) = redactor.redact(line.as_bytes());
    let metadata = format!(
        r#"{{"schema_version":2,"checkpoint_id":"{checkpoint_id}","session_id":"{session}","agent_kind":"claude_code","scope":"{}","provider_session_id":"provider-{session}","working_dir":"/tmp/libra-traces-live","redaction_report":{{}},"model":"unknown","created_at":{created_at}}}"#,
        scope.as_str()
    );
    let (metadata, _) = redactor.redact(metadata.as_bytes());
    let (events, _) = redactor.redact(b"{}\n");
    let (report, _) = redactor.redact(b"{}");
    let marker = TracesInflightMarker::new(
        session,
        &checkpoint_id,
        chrono::Utc::now().timestamp_millis(),
    );
    write_traces_inflight_marker(&conn, &marker)
        .await
        .expect("checkpoint writer marker");
    let written = history
        .append_checkpoint_commit(CheckpointCommitParams {
            reasoning_artifacts: &[],
            checkpoint_id: &checkpoint_id,
            session_id: session,
            marker_generation: marker.generation.as_deref().expect("marker generation"),
            capture_scope: None,
            agent_kind: "claude_code",
            parent_commit: None,
            scope,
            tool_use_id: None,
            metadata_json: &metadata,
            transcript_redacted: &transcript,
            lifecycle_events_jsonl: &events,
            redaction_report_json: &report,
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
            Value::from(checkpoint_id.as_str()),
            Value::from(session),
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
        &conn,
        session,
        &checkpoint_id,
        &written.marker_generation,
    )
    .await
    .expect("retire writer marker");
    conn.close().await.expect("close seed connection");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    assert!(
        ClientStorage::wait_for_background_tasks_until(deadline).await,
        "seeded object index did not drain"
    );
    let bytes = transcript.bytes().to_vec();
    Seeded {
        commit: written.commit_hash.to_string(),
        transcript_blob: git_blob_sha1(&bytes),
        transcript: bytes,
    }
}

/// Result of one successful `agent push --mega2 --json`.
struct Pushed {
    scope: String,
    tip: String,
    epoch: String,
    locator: String,
    up_to_date: bool,
}

fn push_output(repo: &Path, transport: &Transport, target: &str, extra: &[&str]) -> Output {
    let mut args = vec!["--json", "agent", "push", "--mega2", "--target", target];
    args.extend_from_slice(extra);
    run_with(repo, &args, transport)
}

fn push(repo: &Path, transport: &Transport, target: &str, extra: &[&str]) -> Pushed {
    let output = push_output(repo, transport, target, extra);
    ok(&output, "Mega2 traces push");
    let value = json(&output);
    let data = &value["data"];
    assert_eq!(
        data["state"], "confirmed",
        "push did not confirm a scope: {value}"
    );
    let scope = data["scope"].as_str().expect("scope").to_string();
    uuid::Uuid::parse_str(&scope).expect("scope UUID");
    Pushed {
        scope,
        tip: data["tip"].as_str().expect("tip").to_string(),
        epoch: data["epoch"].as_str().expect("epoch").to_string(),
        locator: data["locator"].as_str().expect("locator").to_string(),
        up_to_date: data["up_to_date"].as_bool().expect("up_to_date"),
    }
}

fn assert_refused_by_service(output: &Output, label: &str) {
    let text = stderr(output);
    assert!(!output.status.success(), "{label} was accepted: {text}");
    assert!(
        !text.contains("stopped before"),
        "{label} did not reach the service: {text}"
    );
}

/// Fetch `scope` into a fresh repository and return the archived tip.
fn fetched_tip(root: &Path, fixture: &LiveFixture, scope: &str, label: &str) -> String {
    let transport = Transport::http(&fixture.token_file);
    let reader = init_repo(root, label, &transport.remote_url(fixture, &fixture.target));
    let output = run_with(
        &reader,
        &[
            "--json", "mega2", "traces", "fetch", "--remote", "origin", "--stream", scope,
        ],
        &transport,
    );
    ok(&output, "traces fetch");
    let value = json(&output);
    assert_eq!(
        value["data"]["body"]["server_id"],
        fixture.server_id.as_str()
    );
    value["data"]["body"]["tip"]
        .as_str()
        .expect("fetched tip")
        .to_string()
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_source_epoch_binding() {
    let fixture = prepared();
    let root = tempfile::tempdir().expect("root");
    let http = Transport::http(&fixture.token_file);
    let repo = init_repo(
        root.path(),
        "repo",
        &http.remote_url(&fixture, &fixture.target),
    );
    assert!(
        !run(&repo, &["config", "get", "libra.traces.mega2.epoch"], &[])
            .status
            .success(),
        "a fresh repository must not have an epoch"
    );
    seed(&repo, "binding-session", CheckpointScope::Committed).await;
    let first = push(&repo, &http, &fixture.target, &[]);
    assert_eq!(
        config_value(&repo, "libra.traces.mega2.source"),
        config_value(&repo, "libra.repoid")
    );
    assert_eq!(config_value(&repo, "libra.traces.mega2.epoch"), first.epoch);
    assert_eq!(
        config_value(&repo, "libra.traces.mega2.receipt.scope"),
        first.scope
    );
    assert_eq!(
        config_value(&repo, "libra.traces.mega2.receipt.state"),
        "confirmed"
    );
    let again = push(&repo, &http, &fixture.target, &[]);
    assert!(again.up_to_date, "an unchanged tip must not be re-sent");
    assert_eq!(again.epoch, first.epoch, "a retry must reuse the epoch");
    assert_eq!(again.scope, first.scope);
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_scoped_git_transport() {
    let fixture = prepared();
    let root = tempfile::tempdir().expect("root");
    let http = Transport::http(&fixture.token_file);
    let ssh = Transport::ssh(root.path(), &fixture.ssh_key_file, "primary");
    let repo = init_repo(
        root.path(),
        "repo",
        &http.remote_url(&fixture, &fixture.target),
    );
    seed(&repo, "transport-session", CheckpointScope::Committed).await;
    let over_http = push(&repo, &http, &fixture.target, &[]);
    assert!(
        over_http.locator.starts_with("/.traces/by-path/"),
        "{}",
        over_http.locator
    );
    let second = seed(&repo, "transport-session", CheckpointScope::Committed).await;
    set_remote(&repo, &ssh.remote_url(&fixture, &fixture.target));
    let over_ssh = push(&repo, &ssh, &fixture.target, &[]);
    assert_eq!(
        over_ssh.locator,
        format!("/.traces/by-scope/{}.git", over_http.scope),
        "a confirmed scope is reused through by-scope"
    );
    assert_eq!(over_ssh.scope, over_http.scope);
    assert_eq!(over_ssh.tip, second.commit);
    assert_eq!(
        fetched_tip(root.path(), &fixture, &over_http.scope, "reader"),
        second.commit
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_scoped_push_cli() {
    let fixture = prepared();
    let root = tempfile::tempdir().expect("root");
    let http = Transport::http(&fixture.token_file);
    let ssh = Transport::ssh(root.path(), &fixture.ssh_key_file, "primary");
    let over_http = init_repo(
        root.path(),
        "http",
        &http.remote_url(&fixture, &fixture.target),
    );
    let seeded_http = seed(&over_http, "cli-http", CheckpointScope::Committed).await;
    let http_push = push(&over_http, &http, &fixture.target, &[]);
    assert_eq!(http_push.tip, seeded_http.commit);
    let over_ssh = init_repo(
        root.path(),
        "ssh",
        &ssh.remote_url(&fixture, &fixture.target),
    );
    let seeded_ssh = seed(&over_ssh, "cli-ssh", CheckpointScope::Committed).await;
    let ssh_push = push(&over_ssh, &ssh, &fixture.target, &[]);
    assert!(ssh_push.locator.starts_with("/.traces/by-path/"));
    assert_eq!(ssh_push.tip, seeded_ssh.commit);
    assert_ne!(http_push.scope, ssh_push.scope, "two sources share a scope");
    assert_eq!(
        config_value(&over_ssh, "remote.origin.url"),
        ssh.remote_url(&fixture, &fixture.target),
        "the stored code remote must not be rewritten"
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_code_clone_isolation() {
    let fixture = prepared();
    let root = tempfile::tempdir().expect("root");
    let destination = root.path().join("clone");
    let output = run(
        root.path(),
        &[
            "clone",
            "--mirror",
            fixture.http_url.as_str(),
            destination.to_str().expect("utf8 path"),
        ],
        &[],
    );
    assert!(
        output.status.success(),
        "live code clone failed: {}",
        stderr(&output)
    );
    let refs = run(&destination, &["show-ref"], &[]);
    let text = String::from_utf8_lossy(&refs.stdout);
    assert!(
        !text.contains("refs/libra/traces"),
        "code clone advertised traces refs: {text}"
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_peer_principal_write_denied() {
    let fixture = prepared();
    let root = tempfile::tempdir().expect("root");
    let http = Transport::http(&fixture.token_file);
    let repo = init_repo(
        root.path(),
        "repo",
        &http.remote_url(&fixture, &fixture.target),
    );
    seed(&repo, "peer-session", CheckpointScope::Committed).await;
    let first = push(&repo, &http, &fixture.target, &[]);
    let second = seed(&repo, "peer-session", CheckpointScope::Committed).await;
    assert_ne!(
        second.commit, first.tip,
        "the peer attempt must carry a new tip"
    );

    let peer_http = Transport::http(&fixture.peer_token_file);
    let denied = push_output(&repo, &peer_http, &fixture.target, &[]);
    assert_refused_by_service(&denied, "peer HTTP write");
    assert!(
        stderr(&denied).contains("unknown_scope"),
        "{}",
        stderr(&denied)
    );

    // The HTTP refusal left the receipt pending; restore the by-scope route
    // for the SSH peer attempt.
    for (key, value) in [
        ("libra.traces.mega2.receipt.state", "confirmed"),
        ("libra.traces.mega2.receipt.scope", first.scope.as_str()),
    ] {
        ok(
            &run(&repo, &["config", "set", key, value], &[]),
            "restore receipt",
        );
    }
    let peer_ssh = Transport::ssh(root.path(), &fixture.peer_ssh_key_file, "peer");
    set_remote(&repo, &peer_ssh.remote_url(&fixture, &fixture.target));
    let denied_ssh = push_output(&repo, &peer_ssh, &fixture.target, &[]);
    assert_refused_by_service(&denied_ssh, "peer SSH write");
    assert!(
        stderr(&denied_ssh).contains("/.traces/by-scope/")
            || stderr(&denied_ssh).contains("unknown_scope"),
        "{}",
        stderr(&denied_ssh)
    );

    assert_eq!(
        fetched_tip(root.path(), &fixture, &first.scope, "reader"),
        first.tip,
        "a refused peer write must leave the primary tip unchanged"
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_other_target_ref_isolation() {
    let fixture = prepared();
    let root = tempfile::tempdir().expect("root");
    let http = Transport::http(&fixture.token_file);
    let repo = init_repo(
        root.path(),
        "repo",
        &http.remote_url(&fixture, &fixture.target),
    );
    seed(&repo, "target-session", CheckpointScope::Committed).await;
    let primary = push(&repo, &http, &fixture.target, &[]);
    let second = seed(&repo, "target-session", CheckpointScope::Committed).await;
    assert_ne!(second.commit, primary.tip);
    let other = push(&repo, &http, &fixture.other_target, &[]);
    assert_eq!(
        other.epoch, primary.epoch,
        "the source and epoch stay the same"
    );
    assert_eq!(other.tip, second.commit);
    assert_ne!(
        other.scope, primary.scope,
        "targets {} and {} resolved to the same scope",
        fixture.target, fixture.other_target
    );
    assert_eq!(
        fetched_tip(root.path(), &fixture, &primary.scope, "reader"),
        primary.tip,
        "the other-target push changed the primary target tip"
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_other_source_ref_isolation() {
    let fixture = prepared();
    let root = tempfile::tempdir().expect("root");
    let http = Transport::http(&fixture.token_file);
    let url = http.remote_url(&fixture, &fixture.target);
    let first_repo = init_repo(root.path(), "first", &url);
    seed(&first_repo, "source-one", CheckpointScope::Committed).await;
    let first = push(&first_repo, &http, &fixture.target, &[]);
    let second_repo = init_repo(root.path(), "second", &url);
    assert_ne!(
        config_value(&first_repo, "libra.repoid"),
        config_value(&second_repo, "libra.repoid")
    );
    ok(
        &run(
            &second_repo,
            &["config", "set", "libra.traces.mega2.epoch", &first.epoch],
            &[],
        ),
        "pin the same epoch",
    );
    let seeded = seed(&second_repo, "source-two", CheckpointScope::Committed).await;
    let second = push(&second_repo, &http, &fixture.target, &[]);
    assert_eq!(second.epoch, first.epoch);
    assert_eq!(second.tip, seeded.commit);
    assert_ne!(second.tip, first.tip);
    assert_ne!(
        second.scope, first.scope,
        "two sources resolved to the same scope"
    );
    assert_eq!(
        fetched_tip(root.path(), &fixture, &first.scope, "reader"),
        first.tip,
        "the second source changed the first source tip"
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_epoch_history_retained() {
    let fixture = prepared();
    let root = tempfile::tempdir().expect("root");
    let http = Transport::http(&fixture.token_file);
    let repo = init_repo(
        root.path(),
        "repo",
        &http.remote_url(&fixture, &fixture.target),
    );
    seed(&repo, "epoch-session", CheckpointScope::Committed).await;
    let first = push(&repo, &http, &fixture.target, &[]);
    let seeded = seed(&repo, "epoch-session", CheckpointScope::Committed).await;
    let second = push(&repo, &http, &fixture.target, &["--new-stream"]);
    assert_eq!(second.tip, seeded.commit);
    assert_ne!(second.epoch, first.epoch);
    assert_ne!(
        second.scope, first.scope,
        "the second epoch reused the first scope"
    );
    assert_eq!(
        fetched_tip(root.path(), &fixture, &first.scope, "reader"),
        first.tip,
        "the new epoch changed the old epoch tip"
    );
}

/// One committed checkpoint and temporary checkpoints from two stopped
/// sessions, so `agent clean --all` rewrites a non-empty chain.
async fn seed_prunable(repo: &Path) -> Seeded {
    seed(repo, "prune-a", CheckpointScope::Temporary).await;
    seed(repo, "prune-b", CheckpointScope::Temporary).await;
    seed(repo, "prune-a", CheckpointScope::Committed).await
}

fn clean_all(repo: &Path, before: &str) -> String {
    ok(&run(repo, &["agent", "clean", "--all"], &[]), "agent clean");
    let output = run(repo, &["rev-parse", "--verify", TRACES_BRANCH], &[]);
    ok(&output, "local traces tip after clean");
    let after = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert!(!after.is_empty(), "clean left no traces tip");
    assert_ne!(after, before, "clean did not rewrite the traces chain");
    after
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_clean_rewrite_denied() {
    let fixture = prepared();
    let root = tempfile::tempdir().expect("root");
    let http = Transport::http(&fixture.token_file);
    let repo = init_repo(
        root.path(),
        "repo",
        &http.remote_url(&fixture, &fixture.target),
    );
    seed_prunable(&repo).await;
    let first = push(&repo, &http, &fixture.target, &[]);
    clean_all(&repo, &first.tip);
    let again = push_output(&repo, &http, &fixture.target, &[]);
    assert_refused_by_service(&again, "push after clean");
    assert!(
        stderr(&again).contains("non-fast-forward"),
        "{}",
        stderr(&again)
    );
    assert_eq!(
        config_value(&repo, "libra.traces.mega2.epoch"),
        first.epoch,
        "a refused rewrite must not open an epoch"
    );
    assert_eq!(
        fetched_tip(root.path(), &fixture, &first.scope, "reader"),
        first.tip
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_new_stream_epoch_created() {
    let fixture = prepared();
    let root = tempfile::tempdir().expect("root");
    let http = Transport::http(&fixture.token_file);
    let repo = init_repo(
        root.path(),
        "repo",
        &http.remote_url(&fixture, &fixture.target),
    );
    seed_prunable(&repo).await;
    let first = push(&repo, &http, &fixture.target, &[]);
    let before = config_value(&repo, "libra.traces.mega2.epoch");
    assert_eq!(before, first.epoch);
    clean_all(&repo, &first.tip);
    let seeded = seed(&repo, "after-clean", CheckpointScope::Committed).await;
    let opened = push(&repo, &http, &fixture.target, &["--new-stream"]);
    let after = config_value(&repo, "libra.traces.mega2.epoch");
    assert_ne!(after, before, "new-stream did not change the durable epoch");
    assert_eq!(opened.epoch, after);
    assert_eq!(opened.tip, seeded.commit);
    assert_ne!(opened.scope, first.scope, "new-stream reused the old scope");
    assert_eq!(
        config_value(&repo, "libra.traces.mega2.receipt.scope"),
        opened.scope
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_old_stream_bytes_readable() {
    let fixture = prepared();
    let root = tempfile::tempdir().expect("root");
    let http = Transport::http(&fixture.token_file);
    let source = init_repo(
        root.path(),
        "source",
        &http.remote_url(&fixture, &fixture.target),
    );
    seed(&source, "bytes-temp", CheckpointScope::Temporary).await;
    let sentinel = seed(&source, "bytes-session", CheckpointScope::Committed).await;
    let old = push(&source, &http, &fixture.target, &[]);
    clean_all(&source, &old.tip);
    seed(&source, "bytes-after", CheckpointScope::Committed).await;
    push(&source, &http, &fixture.target, &["--new-stream"]);

    let reader = init_repo(
        root.path(),
        "reader",
        &http.remote_url(&fixture, &fixture.target),
    );
    assert_eq!(
        config_value(&reader, "core.objectformat"),
        "sha1",
        "reader must use the archive hash kind"
    );
    assert!(
        !reader.join(".libra/objects/info/alternates").exists(),
        "reader must not borrow objects"
    );
    assert!(
        !run(&reader, &["cat-file", "-e", &sentinel.transcript_blob], &[])
            .status
            .success(),
        "the sentinel exists before fetch"
    );
    let fetched = run_with(
        &reader,
        &[
            "--json", "mega2", "traces", "fetch", "--remote", "origin", "--stream", &old.scope,
        ],
        &http,
    );
    ok(&fetched, "old-stream fetch into an empty ODB");
    assert_eq!(json(&fetched)["data"]["body"]["tip"], old.tip.as_str());
    let shown = run(&reader, &["cat-file", "-p", &sentinel.transcript_blob], &[]);
    ok(&shown, "read the fetched sentinel");
    assert_eq!(shown.stdout, sentinel.transcript, "fetched bytes differ");
    assert_eq!(git_blob_sha1(&shown.stdout), sentinel.transcript_blob);
}

/// Push one real stream and return its scope for the read-side cases.
async fn pushed_stream(root: &Path, fixture: &LiveFixture) -> Pushed {
    let http = Transport::http(&fixture.token_file);
    let repo = init_repo(root, "source", &http.remote_url(fixture, &fixture.target));
    seed(&repo, "read-session", CheckpointScope::Committed).await;
    push(&repo, &http, &fixture.target, &[])
}

fn query(cwd: &Path, args: &[&str], fixture: &LiveFixture) -> Output {
    let token_file = fixture.token_file.display().to_string();
    let mut full = vec!["--json", "mega2", "traces"];
    full.extend_from_slice(args);
    full.extend_from_slice(&[
        "--server",
        fixture.http_url.as_str(),
        "--token-file",
        token_file.as_str(),
    ]);
    let output = run(cwd, &full, &[]);
    let text = stderr(&output);
    assert!(output.status.success(), "live query failed: {text}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"ok\""), "missing JSON envelope: {stdout}");
    output
}

fn repo() -> tempfile::TempDir {
    tempfile::tempdir().expect("live repo")
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_explicit_fetch_cli() {
    let fixture = prepared();
    let root = tempfile::tempdir().expect("root");
    let stream = pushed_stream(root.path(), &fixture).await;
    let http = Transport::http(&fixture.token_file);
    let reader = init_repo(
        root.path(),
        "reader",
        &http.remote_url(&fixture, &fixture.target),
    );
    let output = run_with(
        &reader,
        &[
            "--json",
            "mega2",
            "traces",
            "fetch",
            "--remote",
            "origin",
            "--stream",
            &stream.scope,
        ],
        &http,
    );
    ok(&output, "live fetch");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains(&format!("refs/remotes/origin/traces/{}/sha1", stream.scope)),
        "fetch did not publish an archive ref: {text}"
    );
    assert!(!text.contains("refs/libra/traces"));
    assert!(
        !run(&reader, &["rev-parse", "--verify", TRACES_BRANCH], &[])
            .status
            .success(),
        "fetch must not create the local traces branch"
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_archive_locator_binding() {
    let fixture = prepared();
    let root = tempfile::tempdir().expect("root");
    let stream = pushed_stream(root.path(), &fixture).await;
    let ssh = Transport::ssh(root.path(), &fixture.ssh_key_file, "primary");
    let reader = init_repo(
        root.path(),
        "reader",
        &ssh.remote_url(&fixture, &fixture.target),
    );
    let output = run_with(
        &reader,
        &[
            "--json",
            "mega2",
            "traces",
            "fetch",
            "--remote",
            "origin",
            "--stream",
            &stream.scope,
        ],
        &ssh,
    );
    ok(&output, "SSH archive fetch");
    assert_eq!(
        json(&output)["data"]["body"]["server_id"],
        fixture.server_id.as_str()
    );
    let fingerprint = config_value(&reader, "libra.traces.remote.origin.fingerprint");
    assert_eq!(fingerprint.len(), 64);
    // The same remote name pointed at the HTTP endpoint is another authority.
    let http = Transport::http(&fixture.token_file);
    set_remote(&reader, &http.remote_url(&fixture, &fixture.target));
    let conflict = run_with(
        &reader,
        &[
            "mega2",
            "traces",
            "fetch",
            "--remote",
            "origin",
            "--stream",
            &stream.scope,
        ],
        &http,
    );
    assert_refused_by_service(&conflict, "fetch through another authority");
    assert!(
        stderr(&conflict).contains("does not match"),
        "{}",
        stderr(&conflict)
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_archive_pack_integrity() {
    let fixture = prepared();
    let root = tempfile::tempdir().expect("root");
    let http = Transport::http(&fixture.token_file);
    let reader = init_repo(
        root.path(),
        "reader",
        &http.remote_url(&fixture, &fixture.target),
    );
    let unknown = uuid::Uuid::new_v4().to_string();
    let output = run_with(
        &reader,
        &[
            "mega2", "traces", "fetch", "--remote", "origin", "--stream", &unknown,
        ],
        &http,
    );
    assert_refused_by_service(&output, "unknown stream fetch");
    assert!(
        !run(
            &reader,
            &[
                "config",
                "get",
                &format!("libra.traces.archive.{unknown}.ref")
            ],
            &[]
        )
        .status
        .success(),
        "a refused fetch recorded an archive ref"
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_capabilities_cli() {
    let fixture = prepared();
    let root = repo();
    let output = query(root.path(), &["capabilities"], &fixture);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("libra-traces/v1"), "{text}");
    assert!(text.contains(&fixture.server_id), "{text}");
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_search_readiness_preflight() {
    let fixture = prepared();
    let root = repo();
    let output = query(root.path(), &["capabilities", "--require-search"], &fixture);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("search_ready"), "{text}");
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_target_stream_navigation() {
    let fixture = prepared();
    let root = repo();
    query(
        root.path(),
        &["targets", "--path", fixture.target.as_str()],
        &fixture,
    );
    query(
        root.path(),
        &["streams", "--target", "pending-from-resolve"],
        &fixture,
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_session_navigation() {
    let fixture = prepared();
    let root = repo();
    query(
        root.path(),
        &[
            "sessions",
            "--stream",
            "00000000-0000-4000-8000-000000000000",
        ],
        &fixture,
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_session_time_filter() {
    let fixture = prepared();
    let root = repo();
    query(
        root.path(),
        &[
            "sessions",
            "--stream",
            "00000000-0000-4000-8000-000000000000",
            "--since",
            "2026-11-02T00:00:00Z",
            "--until",
            "2026-11-03T00:00:00Z",
        ],
        &fixture,
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_metadata_query_cli() {
    let fixture = prepared();
    let root = repo();
    query(
        root.path(),
        &[
            "checkpoints",
            "--stream",
            "00000000-0000-4000-8000-000000000000",
            "--session",
            "live",
        ],
        &fixture,
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_search_local_admission() {
    let fixture = prepared();
    let root = tempfile::tempdir().expect("root");
    let stream = pushed_stream(root.path(), &fixture).await;
    let search = |query: &str| {
        run(
            root.path(),
            &[
                "--json",
                "mega2",
                "traces",
                "search",
                "--server",
                fixture.http_url.as_str(),
                "--token-file",
                fixture.token_file.to_str().expect("token path"),
                "--target",
                fixture.target.as_str(),
                "--stream",
                stream.scope.as_str(),
                "--query",
                query,
            ],
            &[],
        )
    };
    // Invalid bounds are refused before the network. The in-process observer
    // that counts requests lives in the local contract test; this live case
    // proves the refusal text and exit status with the real endpoint args.
    for query in ["", &"b".repeat(257)] {
        let refused = search(query);
        assert!(!refused.status.success(), "{}", query.len());
        assert!(
            stderr(&refused).contains("was not sent"),
            "{}",
            stderr(&refused)
        );
    }
    // Valid bounds reach the real service.
    for query in ["a", &"c".repeat(256)] {
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        loop {
            let output = search(query);
            if output.status.success() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "valid {} byte query never reached the service: {}",
                query.len(),
                stderr(&output)
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_keyword_search_cli() {
    let fixture = prepared();
    let root = tempfile::tempdir().expect("root");
    let stream = pushed_stream(root.path(), &fixture).await;
    // The fixture indexes asynchronously; poll the mounted search until the
    // seeded transcript is hit (bounded).
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let mut hit = None;
    while std::time::Instant::now() < deadline {
        let output = query(
            root.path(),
            &[
                "search",
                "--target",
                fixture.target.as_str(),
                "--stream",
                stream.scope.as_str(),
                "--query",
                "live traces sentinel",
            ],
            &fixture,
        );
        let body: Json = serde_json::from_slice(&output.stdout).expect("search JSON");
        if let Some(items) = body
            .get("data")
            .and_then(|data| data.get("body"))
            .and_then(|body| body.get("items"))
            .and_then(Json::as_array)
            .filter(|items| !items.is_empty())
        {
            let item = &items[0];
            assert!(item.get("checkpoint_id").is_some(), "{item}");
            assert!(
                item.get("occurrence_count")
                    .and_then(Json::as_u64)
                    .is_some_and(|count| count >= 1),
                "{item}"
            );
            hit = Some(body);
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let body = hit.expect("the seeded transcript must be searchable");
    let coverage = body["data"]["body"]["coverage"].clone();
    assert!(coverage.is_object(), "{coverage}");
    // The mounted route needs a stream; the local refusal runs before network.
    let refused = run(
        root.path(),
        &[
            "--json",
            "mega2",
            "traces",
            "search",
            "--server",
            fixture.http_url.as_str(),
            "--token-file",
            fixture.token_file.to_str().expect("token path"),
            "--target",
            fixture.target.as_str(),
            "--query",
            "live",
        ],
        &[],
    );
    assert!(!refused.status.success());
    // A filter the mounted route does not implement is refused locally too.
    let unsupported = run(
        root.path(),
        &[
            "--json",
            "mega2",
            "traces",
            "search",
            "--server",
            fixture.http_url.as_str(),
            "--token-file",
            fixture.token_file.to_str().expect("token path"),
            "--target",
            fixture.target.as_str(),
            "--stream",
            stream.scope.as_str(),
            "--agent",
            "codex",
            "--query",
            "live",
        ],
        &[],
    );
    assert!(!unsupported.status.success());
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_bounded_transcript_cli() {
    // Blocked on the fixture: the bounded transcript route reads the metadata
    // projection, and this fixture does not ingest freshly pushed scopes
    // (search indexes the object store directly, the DB projection lags).
    // The bounded-read cursor contract is covered by the local contract tests;
    // this selector stays ignored until a fixture with live ingestion exists.
    let fixture = prepared();
    let root = repo();
    query(
        root.path(),
        &[
            "transcript",
            "--stream",
            "00000000-0000-4000-8000-000000000000",
            "--checkpoint",
            "live",
        ],
        &fixture,
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_installed_binary_read_smoke() {
    let fixture = prepared();
    let installed = std::env::var("LIBRA_TRACES_LIVE_INSTALLED_BINARY").unwrap_or_else(|_| {
        panic!(
            "LIBRA_TRACES_LIVE_INSTALLED_BINARY is missing or empty; this live case cannot skip or use cargo run"
        )
    });
    assert!(
        !installed.is_empty() && Path::new(&installed).is_file(),
        "installed binary {installed} is not a file"
    );
    assert_ne!(
        installed,
        env!("CARGO_BIN_EXE_libra"),
        "the smoke must not fall back to the cargo test binary"
    );
    let bytes = std::fs::read(&installed).expect("read installed binary");
    assert_eq!(
        sha256_hex(&bytes),
        fixture.client_binary_sha256,
        "installed binary hash does not match the frozen candidate"
    );
    let root = repo();
    let output = Command::new(&installed)
        .args([
            "--json",
            "mega2",
            "traces",
            "capabilities",
            "--server",
            fixture.http_url.as_str(),
            "--token-file",
            fixture.token_file.to_str().expect("token path"),
        ])
        .current_dir(root.path())
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", root.path())
        .output()
        .expect("run installed binary");
    assert!(
        output.status.success(),
        "installed binary smoke failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
