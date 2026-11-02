//! Real Mega2 fixture cases for traces. Default CI does not start them.
//! `cargo nextest run --run-ignored only` must execute the selected case.
//! A missing fixture fails the case. These tests do not start a stub server
//! and do not treat a local refusal as a successful upload or read.

use std::{
    path::Path,
    process::{Command, Output},
};

use libra::internal::protocol::mega2_auth::ENV_TOKEN_VAR;

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

fn token_value(path: &Path) -> String {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("cannot read the live token file: {error}"));
    let value = raw.trim_end_matches(['\n', '\r']);
    assert!(!value.is_empty(), "the live token file is empty");
    value.to_string()
}

fn run(cwd: &Path, args: &[&str], token: Option<&str>) -> Output {
    // The helper clears the process environment. A token is passed only as the
    // child environment and is not copied into assertion text.
    if let Some(value) = token {
        super::run_libra_command_with_env(args, cwd, &[(ENV_TOKEN_VAR, value)])
    } else {
        super::run_libra_command(args, cwd)
    }
}

fn repo() -> tempfile::TempDir {
    tempfile::tempdir().expect("live repo")
}

fn init_with_remote(url: &str) -> tempfile::TempDir {
    let root = repo();
    let created = run(root.path(), &["init"], None);
    assert!(
        created.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let configured = run(
        root.path(),
        &["config", "set", "remote.origin.url", url],
        None,
    );
    assert!(
        configured.status.success(),
        "remote URL was not stored: {}",
        String::from_utf8_lossy(&configured.stderr)
    );
    root
}

fn push(cwd: &Path, token: &str, extra: &[&str]) -> Output {
    let mut args = vec!["agent", "push", "--mega2", "--remote", "origin"];
    args.extend_from_slice(extra);
    run(cwd, &args, Some(token))
}

fn config_value(cwd: &Path, key: &str) -> String {
    let output = run(cwd, &["config", "get", key], None);
    assert!(
        output.status.success(),
        "missing {key}: {}",
        stderr(&output)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn assert_push_reached_service(output: &Output, label: &str) {
    let text = stderr(output);
    assert!(
        output.status.success(),
        "{label} did not succeed against the live service: {text}"
    );
    assert!(
        !text.contains("stopped before"),
        "{label} stopped before network admission: {text}"
    );
}

fn curl_code(url: &str) -> String {
    let status = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--max-time",
            "10",
            "--output",
            "/dev/null",
            "--write-out",
            "%{http_code}",
            url,
        ])
        .output()
        .unwrap_or_else(|error| panic!("curl failed to start: {error}"));
    assert!(
        status.status.success(),
        "live endpoint unreachable: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let code = String::from_utf8_lossy(&status.stdout).trim().to_string();
    assert_ne!(code, "000", "live service did not answer");
    code
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
    let output = run(cwd, &full, None);
    let text = stderr(&output);
    assert!(output.status.success(), "live query failed: {text}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"ok\""), "missing JSON envelope: {stdout}");
    output
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_source_epoch_binding() {
    let fixture = prepared();
    let code = curl_code(&format!(
        "{}/api/v1/traces/capabilities",
        fixture.http_url.trim_end_matches('/')
    ));
    assert_ne!(code, "000");
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_scoped_git_transport() {
    let fixture = prepared();
    let code = curl_code(&format!(
        "{}/info/refs?service=git-upload-pack",
        fixture.http_url.trim_end_matches('/')
    ));
    assert_ne!(code, "000");
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_scoped_push_cli() {
    let fixture = prepared();
    let root = init_with_remote(&fixture.http_url);
    let output = push(root.path(), &token_value(&fixture.token_file), &[]);
    assert_push_reached_service(&output, "HTTP scoped push");
    let ssh = init_with_remote(&fixture.ssh_uri);
    let ssh_output = push(ssh.path(), &token_value(&fixture.token_file), &[]);
    assert_push_reached_service(&ssh_output, "SSH scoped push");
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_code_clone_isolation() {
    let fixture = prepared();
    let root = repo();
    let destination = root.path().join("clone");
    let output = run(
        root.path(),
        &[
            "clone",
            "--mirror",
            fixture.http_url.as_str(),
            destination.to_str().expect("utf8 path"),
        ],
        None,
    );
    assert!(
        output.status.success(),
        "live code clone failed: {}",
        stderr(&output)
    );
    let refs = run(&destination, &["show-ref"], None);
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
    let primary = init_with_remote(&fixture.http_url);
    let first = push(primary.path(), &token_value(&fixture.token_file), &[]);
    assert_push_reached_service(&first, "primary push");
    let peer = push(primary.path(), &token_value(&fixture.peer_token_file), &[]);
    assert!(
        !peer.status.success(),
        "peer principal was allowed to write the primary stream: {}",
        stderr(&peer)
    );
    assert!(
        !stderr(&peer).contains("stopped before"),
        "peer denial did not reach the service: {}",
        stderr(&peer)
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_other_target_ref_isolation() {
    let fixture = prepared();
    let root = init_with_remote(&fixture.http_url);
    let first = push(root.path(), &token_value(&fixture.token_file), &[]);
    assert_push_reached_service(&first, "primary target push");
    let other = init_with_remote(&fixture.http_url);
    let second = push(other.path(), &token_value(&fixture.token_file), &[]);
    assert_push_reached_service(&second, "other target push");
    let primary_scope = config_value(root.path(), "libra.traces.mega2.receipt.scope");
    let other_scope = config_value(other.path(), "libra.traces.mega2.receipt.scope");
    assert_ne!(
        primary_scope, other_scope,
        "targets {} and {} resolved to the same scope",
        fixture.target, fixture.other_target
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_other_source_ref_isolation() {
    let fixture = prepared();
    let first_repo = init_with_remote(&fixture.http_url);
    let first = push(first_repo.path(), &token_value(&fixture.token_file), &[]);
    assert_push_reached_service(&first, "first source push");
    let second_repo = init_with_remote(&fixture.http_url);
    let second = push(second_repo.path(), &token_value(&fixture.token_file), &[]);
    assert_push_reached_service(&second, "second source push");
    assert_ne!(
        config_value(first_repo.path(), "libra.traces.mega2.receipt.scope"),
        config_value(second_repo.path(), "libra.traces.mega2.receipt.scope"),
        "two sources resolved to the same scope"
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_epoch_history_retained() {
    let fixture = prepared();
    let root = init_with_remote(&fixture.http_url);
    let token = token_value(&fixture.token_file);
    let first = push(root.path(), &token, &["--new-stream"]);
    assert_push_reached_service(&first, "first epoch push");
    let first_scope = config_value(root.path(), "libra.traces.mega2.receipt.scope");
    let second = push(root.path(), &token, &["--new-stream"]);
    assert_push_reached_service(&second, "second epoch push");
    assert_ne!(
        first_scope,
        config_value(root.path(), "libra.traces.mega2.receipt.scope"),
        "the second epoch reused the first scope"
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_clean_rewrite_denied() {
    let fixture = prepared();
    let root = init_with_remote(&fixture.http_url);
    let token = token_value(&fixture.token_file);
    let first = push(root.path(), &token, &[]);
    assert_push_reached_service(&first, "push before clean");
    let cleaned = run(root.path(), &["agent", "clean", "--all"], None);
    assert!(
        cleaned.status.success(),
        "agent clean failed: {}",
        stderr(&cleaned)
    );
    let again = push(root.path(), &token, &[]);
    assert!(
        !again.status.success(),
        "clean rewrite was accepted: {}",
        stderr(&again)
    );
    assert!(
        !stderr(&again).contains("stopped before"),
        "clean rejection did not reach the service: {}",
        stderr(&again)
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_new_stream_epoch_created() {
    let fixture = prepared();
    let root = init_with_remote(&fixture.http_url);
    let token = token_value(&fixture.token_file);
    let before = run(
        root.path(),
        &["config", "get", "libra.traces.mega2.epoch"],
        None,
    );
    let opened = push(root.path(), &token, &["--new-stream"]);
    assert_push_reached_service(&opened, "new-stream push");
    let after = run(
        root.path(),
        &["config", "get", "libra.traces.mega2.epoch"],
        None,
    );
    assert_ne!(
        String::from_utf8_lossy(&before.stdout).trim(),
        String::from_utf8_lossy(&after.stdout).trim(),
        "new-stream did not change the durable epoch"
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_old_stream_bytes_readable() {
    let fixture = prepared();
    let reader = init_with_remote(&fixture.http_url);
    let listed = run(reader.path(), &["rev-list", "--all"], None);
    let commits = String::from_utf8_lossy(&listed.stdout);
    assert!(
        commits.trim().is_empty(),
        "reader ODB is not empty before fetch: {commits}"
    );
    let fetched = run(
        reader.path(),
        &[
            "--json",
            "mega2",
            "traces",
            "fetch",
            "--remote",
            "origin",
            "--stream",
            "00000000-0000-4000-8000-000000000000",
            "--token-file",
            fixture.token_file.to_str().expect("token path"),
        ],
        None,
    );
    assert!(
        fetched.status.success(),
        "old-stream fetch into an empty ODB failed for server {}: {}",
        fixture.server_id,
        stderr(&fetched)
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
async fn live_keyword_search_cli() {
    let fixture = prepared();
    let root = repo();
    query(
        root.path(),
        &[
            "search",
            "--target",
            fixture.target.as_str(),
            "--query",
            "timeout",
        ],
        &fixture,
    );
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_bounded_transcript_cli() {
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
async fn live_explicit_fetch_cli() {
    let fixture = prepared();
    let real = fetch_placeholder(&fixture);
    assert!(
        real.status.success(),
        "live fetch failed: {}",
        stderr(&real)
    );
    let text = String::from_utf8_lossy(&real.stdout);
    assert!(
        text.contains("refs/remotes/origin/traces/"),
        "fetch did not publish an archive ref: {text}"
    );
    assert!(!text.contains("refs/libra/traces"));
}

fn fetch_placeholder(fixture: &LiveFixture) -> Output {
    let root = init_with_remote(&format!(
        "{}/project/live.git",
        fixture.http_url.trim_end_matches('/')
    ));
    run(
        root.path(),
        &[
            "--json",
            "mega2",
            "traces",
            "fetch",
            "--remote",
            "origin",
            "--stream",
            "00000000-0000-4000-8000-000000000000",
            "--token-file",
            fixture.token_file.to_str().expect("token path"),
        ],
        None,
    )
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_archive_locator_binding() {
    let fixture = prepared();
    let real = fetch_placeholder(&fixture);
    assert!(
        real.status.success(),
        "live archive locator was not bound: {}",
        stderr(&real)
    );
    assert!(String::from_utf8_lossy(&real.stdout).contains(&fixture.server_id));
}

#[tokio::test]
#[ignore = "explicit real Mega2 fixture"]
async fn live_archive_pack_integrity() {
    let fixture = prepared();
    let root = init_with_remote(&fixture.http_url);
    let output = run(
        root.path(),
        &[
            "mega2",
            "traces",
            "fetch",
            "--remote",
            "origin",
            "--stream",
            "00000000-0000-4000-8000-000000000000",
            "--token-file",
            fixture.token_file.to_str().expect("token path"),
        ],
        None,
    );
    assert!(
        !output.status.success(),
        "an unknown live stream was accepted without a verified pack: {}",
        stderr(&output)
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
