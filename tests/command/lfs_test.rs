//! Tests LFS subcommands covering upload/download negotiation, locks, and tracking detection.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::{fs, path::Path, process::Command};

use axum::{
    Json, Router,
    http::StatusCode,
    routing::{get, post},
};
use serde_json::json;
use tempfile::TempDir;

/// Build a `Command` for the Libra binary with an isolated HOME.
///
/// Clears `LIBRA_READ_POLICY` so an ambient `offline`/`local` from the host
/// shell (common when debugging tiered storage) cannot skip `lfs.lockEnforce`
/// verification — that produced false T-1 failures under BRL-05 that focused
/// runs without the polluted env did not reproduce.
fn libra_command(cwd: &Path) -> Command {
    let home = cwd.join(".libra-test-home");
    let config_home = home.join(".config");
    fs::create_dir_all(&config_home).expect("failed to create isolated HOME");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
    cmd.current_dir(cwd)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("USERPROFILE", &home)
        .env_remove("LIBRA_READ_POLICY");
    cmd
}

/// Spawn an axum-based mock LFS server on a free port and return the bound address.
/// The returned `JoinHandle` is dropped by the caller when the test finishes, which
/// aborts the server task.
async fn spawn_mock_lfs_server(app: Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind mock LFS listener");
    let addr = listener
        .local_addr()
        .expect("failed to read mock LFS bound address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

/// Initialize an isolated libra repo wired to the given LFS server URL via the `origin`
/// remote, so `LFSClient::new()` resolves through `branch.main.remote=origin` at runtime.
fn init_repo_with_mock_remote(remote_url: &str) -> TempDir {
    let repo = init_temp_repo();
    let repo_path = repo.path();

    let add_remote = libra_command(repo_path)
        .args(["remote", "add", "origin", remote_url])
        .output()
        .expect("failed to add mock remote");
    assert!(
        add_remote.status.success(),
        "remote add failed: {}",
        String::from_utf8_lossy(&add_remote.stderr)
    );

    let set_upstream = libra_command(repo_path)
        .args(["config", "branch.main.remote", "origin"])
        .output()
        .expect("failed to set branch upstream remote");
    assert!(
        set_upstream.status.success(),
        "config set failed: {}",
        String::from_utf8_lossy(&set_upstream.stderr)
    );

    repo
}

/// Helper function: Initialize a temporary Libra repository
fn init_temp_repo() -> TempDir {
    let temp_dir = tempfile::tempdir().expect("Failed to create temporary directory");
    let temp_path = temp_dir.path();

    let output = libra_command(temp_path)
        .args(["init"])
        .output()
        .expect("Failed to execute libra binary");

    if !output.status.success() {
        panic!(
            "Failed to initialize libra repository: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    temp_dir
}

#[tokio::test]
/// Test track/untrack path rule management
async fn test_lfs_track_untrack() {
    let temp_repo = init_temp_repo();
    let temp_path = temp_repo.path();

    let track_output = libra_command(temp_path)
        .args(["lfs", "track", "*.txt"])
        .output()
        .expect("Failed to track path");
    assert!(
        track_output.status.success(),
        "Failed to track path: {}",
        String::from_utf8_lossy(&track_output.stderr)
    );

    let untrack_output = libra_command(temp_path)
        .args(["lfs", "untrack", "*.txt"])
        .output()
        .expect("Failed to untrack path");
    assert!(
        untrack_output.status.success(),
        "Failed to untrack path: {}",
        String::from_utf8_lossy(&untrack_output.stderr)
    );
}

#[tokio::test]
/// Pre-v0.17.1065 `libra lfs track` (list mode) printed nothing at all
/// on a fresh repo with no tracked patterns — the user could not tell
/// whether the command had run or hung. Pin the new behavior: the
/// "Listing tracked patterns" header is always emitted so empty is a
/// confirmed-empty, not a silent no-op.
async fn test_lfs_track_list_prints_header_on_empty_repo() {
    let temp_repo = init_temp_repo();
    let temp_path = temp_repo.path();

    let output = libra_command(temp_path)
        .args(["lfs", "track"])
        .output()
        .expect("failed to run lfs track");
    assert!(
        output.status.success(),
        "lfs track (list) should succeed on empty repo: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Listing tracked patterns"),
        "empty-repo lfs track should still print the header, stdout={stdout:?}"
    );
}

#[tokio::test]
/// Test JSON output for local LFS tracking operations.
async fn test_lfs_track_and_untrack_json_output() {
    let temp_repo = init_temp_repo();
    let temp_path = temp_repo.path();

    let track_output = libra_command(temp_path)
        .args(["--json", "lfs", "track", "*.txt"])
        .output()
        .expect("Failed to track path");
    assert!(
        track_output.status.success(),
        "Failed to track path: {}",
        String::from_utf8_lossy(&track_output.stderr)
    );
    assert!(track_output.stderr.is_empty());
    let json: serde_json::Value =
        serde_json::from_slice(&track_output.stdout).expect("track stdout should be JSON");
    assert_eq!(json["command"], "lfs");
    assert_eq!(json["data"]["action"], "track");
    assert_eq!(json["data"]["patterns"][0], "*.txt");

    let list_output = libra_command(temp_path)
        .args(["--json", "lfs", "track"])
        .output()
        .expect("Failed to list tracked patterns");
    assert!(
        list_output.status.success(),
        "Failed to list tracked patterns: {}",
        String::from_utf8_lossy(&list_output.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&list_output.stdout).expect("track list stdout should be JSON");
    assert_eq!(json["data"]["action"], "track-list");
    assert_eq!(json["data"]["patterns"][0], "*.txt");

    let untrack_output = libra_command(temp_path)
        .args(["--json", "lfs", "untrack", "*.txt"])
        .output()
        .expect("Failed to untrack path");
    assert!(
        untrack_output.status.success(),
        "Failed to untrack path: {}",
        String::from_utf8_lossy(&untrack_output.stderr)
    );
    assert!(untrack_output.stderr.is_empty());
    let json: serde_json::Value =
        serde_json::from_slice(&untrack_output.stdout).expect("untrack stdout should be JSON");
    assert_eq!(json["data"]["action"], "untrack");
    assert_eq!(json["data"]["patterns"][0], "*.txt");
}

#[tokio::test]
/// Test file status viewing
async fn test_lfs_ls_files() {
    let temp_repo = init_temp_repo();
    let temp_path = temp_repo.path();

    // Create a test file and add it to LFS
    let file_path = temp_path.join("tracked_file.txt");
    std::fs::write(&file_path, "Tracked content").expect("Failed to create tracked file");

    libra_command(temp_path)
        .args(["lfs", "track", "*.txt"])
        .output()
        .expect("Failed to track file");

    libra_command(temp_path)
        .args(["add", "tracked_file.txt"])
        .output()
        .expect("Failed to add file to LFS");

    let ls_files_output = libra_command(temp_path)
        .args(["lfs", "ls-files"])
        .output()
        .expect("Failed to list LFS files");
    assert!(
        ls_files_output.status.success(),
        "Failed to list LFS files: {}",
        String::from_utf8_lossy(&ls_files_output.stderr)
    );

    let stdout = String::from_utf8_lossy(&ls_files_output.stdout);
    assert!(
        stdout.contains("tracked_file.txt"),
        "LFS file list does not contain expected file: {stdout}",
    );
}

#[tokio::test]
/// Test JSON output for LFS file listing.
async fn test_lfs_ls_files_json_output() {
    let temp_repo = init_temp_repo();
    let temp_path = temp_repo.path();

    let file_path = temp_path.join("tracked_file.txt");
    std::fs::write(&file_path, "Tracked content").expect("Failed to create tracked file");

    libra_command(temp_path)
        .args(["lfs", "track", "*.txt"])
        .output()
        .expect("Failed to track file");

    libra_command(temp_path)
        .args(["add", "tracked_file.txt"])
        .output()
        .expect("Failed to add file to LFS");

    let output = libra_command(temp_path)
        .args(["--json", "lfs", "ls-files", "--size"])
        .output()
        .expect("Failed to list LFS files");
    assert!(
        output.status.success(),
        "Failed to list LFS files: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("ls-files stdout should be JSON");
    assert_eq!(json["command"], "lfs");
    assert_eq!(json["data"]["action"], "ls-files");
    assert_eq!(json["data"]["show_size"], true);
    let file = &json["data"]["files"][0];
    assert_eq!(file["path"], "tracked_file.txt");
    assert!(file["size"].as_u64().is_some());
    // `oid` is the display oid (10-char prefix by default), `full_oid` always carries
    // the canonical 64-char hash so `--json` consumers don't have to pass `--long`.
    let display_oid = file["oid"].as_str().expect("oid should be a string");
    let full_oid = file["full_oid"]
        .as_str()
        .expect("full_oid should be a string");
    assert_eq!(display_oid.len(), 10);
    assert_eq!(full_oid.len(), 64);
    assert!(full_oid.starts_with(display_oid));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// `lfs locks --json` against a mock server that returns one lock; verifies the JSON
/// envelope surfaces the locks list and matches the `LfsOutput` schema.
async fn test_lfs_locks_cli_returns_locks_from_mock_server() {
    let app = Router::new().route(
        "/locks",
        get(|| async {
            Json(json!({
                "locks": [{
                    "id": "lock-1",
                    "path": "tracked.txt",
                    "locked_at": "2026-01-01T00:00:00Z",
                    "owner": { "name": "tester" }
                }],
                "next_cursor": ""
            }))
        }),
    );
    let addr = spawn_mock_lfs_server(app).await;
    let repo = init_repo_with_mock_remote(&format!("http://{addr}"));
    let repo_path = repo.path().to_path_buf();

    let output = tokio::task::spawn_blocking(move || {
        libra_command(&repo_path)
            .args(["--json", "lfs", "locks"])
            .output()
            .expect("failed to run lfs locks")
    })
    .await
    .expect("spawn_blocking join failed");

    assert!(
        output.status.success(),
        "lfs locks should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("locks stdout should be JSON");
    assert_eq!(stdout["command"], "lfs");
    assert_eq!(stdout["data"]["action"], "locks");
    assert_eq!(stdout["data"]["locks"][0]["path"], "tracked.txt");
    assert_eq!(stdout["data"]["locks"][0]["id"], "lock-1");
}

#[tokio::test]
/// Pre-v0.17.1067 `libra lfs track "*.txt"` ran twice in a row would
/// produce zero stdout on the second invocation — the dedup fix in
/// v0.17.1057 returned an empty `added` Vec and the human renderer
/// silently no-op'd. Pin the confirmed-already-tracked notice so the
/// command never looks like a hang.
async fn test_lfs_track_prints_notice_when_all_patterns_already_tracked() {
    let temp_repo = init_temp_repo();
    let temp_path = temp_repo.path();

    // First track adds the pattern; second track has nothing new to add.
    libra_command(temp_path)
        .args(["lfs", "track", "*.txt"])
        .output()
        .expect("first track should succeed");

    let output = libra_command(temp_path)
        .args(["lfs", "track", "*.txt"])
        .output()
        .expect("second track should succeed");
    assert!(
        output.status.success(),
        "second track should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No new patterns added (already tracked)"),
        "duplicate-track should print the already-tracked notice, stdout={stdout:?}"
    );
}

#[tokio::test]
/// Pre-v0.17.1067 `libra lfs untrack "*.txt"` on a pattern that was
/// never tracked produced zero stdout — the human renderer silently
/// no-op'd. Pin the confirmed-no-match notice.
async fn test_lfs_untrack_prints_notice_when_no_match() {
    let temp_repo = init_temp_repo();
    let temp_path = temp_repo.path();

    let output = libra_command(temp_path)
        .args(["lfs", "untrack", "*.never-tracked"])
        .output()
        .expect("untrack should succeed");
    assert!(
        output.status.success(),
        "untrack of an untracked pattern should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No matching LFS patterns to untrack"),
        "no-match untrack should print the no-op notice, stdout={stdout:?}"
    );
}

#[tokio::test]
/// Pre-v0.17.1067 `libra lfs ls-files` on a repo with no LFS-tracked
/// files printed zero stdout. Pin the confirmed-empty notice for the
/// default human path while preserving silence under `--name-only`
/// (which shell pipelines rely on).
async fn test_lfs_ls_files_prints_notice_when_empty_but_silent_with_name_only() {
    let temp_repo = init_temp_repo();
    let temp_path = temp_repo.path();

    // Default human mode → notice present.
    let output = libra_command(temp_path)
        .args(["lfs", "ls-files"])
        .output()
        .expect("ls-files should succeed");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No LFS files"),
        "empty ls-files should print the no-op notice, stdout={stdout:?}"
    );

    // --name-only → silent (pipeline consumers).
    let output = libra_command(temp_path)
        .args(["lfs", "ls-files", "--name-only"])
        .output()
        .expect("ls-files --name-only should succeed");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().is_empty(),
        "empty ls-files --name-only should stay silent, stdout={stdout:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// Pre-v0.17.1066 `libra lfs locks` (human mode) printed nothing when
/// the server returned an empty list — same silent-no-op UX class as
/// the `track-list` fix in v0.17.1065. Pin the new "No locks on the
/// current branch" notice so users always see a confirmed-empty signal.
async fn test_lfs_locks_human_prints_notice_when_empty() {
    let app = Router::new().route(
        "/locks",
        get(|| async { Json(json!({ "locks": [], "next_cursor": "" })) }),
    );
    let addr = spawn_mock_lfs_server(app).await;
    let repo = init_repo_with_mock_remote(&format!("http://{addr}"));
    let repo_path = repo.path().to_path_buf();

    let output = tokio::task::spawn_blocking(move || {
        libra_command(&repo_path)
            .args(["lfs", "locks"])
            .output()
            .expect("failed to run lfs locks")
    })
    .await
    .expect("spawn_blocking join failed");

    assert!(
        output.status.success(),
        "lfs locks should succeed on empty server response; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No locks"),
        "empty `lfs locks` should still print a notice, stdout={stdout:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// `lfs locks --json` against a mock server that returns 403; verifies the CLI surfaces
/// stable error code `LBR-AUTH-002` and exits non-zero.
async fn test_lfs_locks_cli_forbidden_returns_auth_permission_denied() {
    let app = Router::new().route("/locks", get(|| async { StatusCode::FORBIDDEN }));
    let addr = spawn_mock_lfs_server(app).await;
    let repo = init_repo_with_mock_remote(&format!("http://{addr}"));
    let repo_path = repo.path().to_path_buf();

    let output = tokio::task::spawn_blocking(move || {
        libra_command(&repo_path)
            .args(["--json", "lfs", "locks"])
            .output()
            .expect("failed to run lfs locks")
    })
    .await
    .expect("spawn_blocking join failed");

    assert!(
        !output.status.success(),
        "lfs locks should fail when server returns 403"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let envelope: serde_json::Value = serde_json::from_str(stderr.trim())
        .unwrap_or_else(|err| panic!("error envelope should be JSON: {err}; stderr={stderr}"));
    assert_eq!(envelope["error_code"], "LBR-AUTH-002");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// `lfs lock --json` against a mock server that accepts the request; verifies the JSON
/// envelope reports the locked path and action.
async fn test_lfs_lock_cli_success_with_mock_server() {
    let app = Router::new().route(
        "/locks",
        post(|| async {
            (
                StatusCode::CREATED,
                Json(json!({
                    "lock": {
                        "id": "lock-1",
                        "path": "tracked.txt",
                        "locked_at": "2026-01-01T00:00:00Z",
                        "owner": { "name": "tester" }
                    }
                })),
            )
        }),
    );
    let addr = spawn_mock_lfs_server(app).await;
    let repo = init_repo_with_mock_remote(&format!("http://{addr}"));
    let repo_path = repo.path().to_path_buf();
    fs::write(repo_path.join("tracked.txt"), "content").expect("failed to create tracked file");

    let output = tokio::task::spawn_blocking(move || {
        libra_command(&repo_path)
            .args(["--json", "lfs", "lock", "tracked.txt"])
            .output()
            .expect("failed to run lfs lock")
    })
    .await
    .expect("spawn_blocking join failed");

    assert!(
        output.status.success(),
        "lfs lock should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("lock stdout should be JSON");
    assert_eq!(stdout["command"], "lfs");
    assert_eq!(stdout["data"]["action"], "lock");
    assert_eq!(stdout["data"]["path"], "tracked.txt");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// `lfs lock --json` against a mock server that returns 409; verifies the CLI surfaces
/// stable error code `LBR-CONFLICT-002` and exits non-zero.
async fn test_lfs_lock_cli_conflict_returns_conflict_blocked() {
    let app = Router::new().route("/locks", post(|| async { StatusCode::CONFLICT }));
    let addr = spawn_mock_lfs_server(app).await;
    let repo = init_repo_with_mock_remote(&format!("http://{addr}"));
    let repo_path = repo.path().to_path_buf();
    fs::write(repo_path.join("tracked.txt"), "content").expect("failed to create tracked file");

    let output = tokio::task::spawn_blocking(move || {
        libra_command(&repo_path)
            .args(["--json", "lfs", "lock", "tracked.txt"])
            .output()
            .expect("failed to run lfs lock")
    })
    .await
    .expect("spawn_blocking join failed");

    assert!(
        !output.status.success(),
        "lfs lock should fail when server returns 409"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let envelope: serde_json::Value = serde_json::from_str(stderr.trim())
        .unwrap_or_else(|err| panic!("error envelope should be JSON: {err}; stderr={stderr}"));
    assert_eq!(envelope["error_code"], "LBR-CONFLICT-002");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// `lfs unlock --json --force --id <id>` against a mock server that accepts the request;
/// verifies the JSON envelope reports the unlocked path.
async fn test_lfs_unlock_cli_success_with_force_and_id() {
    let app = Router::new().route(
        "/locks/{id}/unlock",
        post(|| async {
            Json(json!({
                "lock": {
                    "id": "lock-1",
                    "path": "tracked.txt",
                    "locked_at": "2026-01-01T00:00:00Z",
                    "owner": { "name": "tester" }
                }
            }))
        }),
    );
    let addr = spawn_mock_lfs_server(app).await;
    let repo = init_repo_with_mock_remote(&format!("http://{addr}"));
    let repo_path = repo.path().to_path_buf();

    let output = tokio::task::spawn_blocking(move || {
        libra_command(&repo_path)
            .args([
                "--json",
                "lfs",
                "unlock",
                "tracked.txt",
                "--force",
                "--id",
                "lock-1",
            ])
            .output()
            .expect("failed to run lfs unlock")
    })
    .await
    .expect("spawn_blocking join failed");

    assert!(
        output.status.success(),
        "lfs unlock should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("unlock stdout should be JSON");
    assert_eq!(stdout["command"], "lfs");
    assert_eq!(stdout["data"]["action"], "unlock");
    assert_eq!(stdout["data"]["path"], "tracked.txt");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// `lfs unlock <path>` (no `--id`) exercises the path → id lookup
/// branch in `LfsCmds::Unlock`: the CLI first calls `GET /locks?path=...`
/// to resolve the lock id, then issues `POST /locks/<id>/unlock`. No
/// prior CLI test covered this branch — only the `--id`-supplied paths.
async fn test_lfs_unlock_by_path_resolves_lock_id_via_get_locks() {
    let app = Router::new()
        .route(
            "/locks",
            get(|| async {
                Json(json!({
                    "locks": [{
                        "id": "lock-by-path",
                        "path": "tracked.bin",
                        "locked_at": "2026-01-01T00:00:00Z",
                        "owner": { "name": "tester" }
                    }],
                    "next_cursor": ""
                }))
            }),
        )
        .route(
            "/locks/{id}/unlock",
            post(|| async {
                Json(json!({
                    "lock": {
                        "id": "lock-by-path",
                        "path": "tracked.bin",
                        "locked_at": "2026-01-01T00:00:00Z",
                        "owner": { "name": "tester" }
                    }
                }))
            }),
        );
    let addr = spawn_mock_lfs_server(app).await;
    let repo = init_repo_with_mock_remote(&format!("http://{addr}"));
    let repo_path = repo.path().to_path_buf();

    // Use `--force` to bypass the path-existence + clean-tree pre-checks.
    // `force` short-circuits the pre-check guard in `LfsCmds::Unlock` but
    // does *not* skip the `id.is_none()` lookup branch in the unlock body,
    // which is exactly what this test exercises: the path → id resolution
    // via `get_locks`. We assert the resolved id came from the server
    // response, proving we went through the path branch and not a `--id`
    // arg.
    let output = tokio::task::spawn_blocking(move || {
        libra_command(&repo_path)
            .args(["--json", "lfs", "unlock", "tracked.bin", "--force"])
            .output()
            .expect("failed to run lfs unlock")
    })
    .await
    .expect("spawn_blocking join failed");

    assert!(
        output.status.success(),
        "lfs unlock by path should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("unlock stdout should be JSON");
    assert_eq!(stdout["data"]["action"], "unlock");
    assert_eq!(stdout["data"]["path"], "tracked.bin");
    // The id must come from the get_locks response, not from a --id arg.
    assert_eq!(stdout["data"]["id"], "lock-by-path");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// Pre-v0.17.1071 `current_refspec` printed
/// `"fatal: HEAD is detached"` via `emit_legacy_stderr` then returned
/// `None`. Every caller wrapped the `None` in a typed error and
/// reported it again through the normal `OutputConfig` error renderer.
/// Net effect: detached-HEAD users (especially `--json` consumers) saw
/// two stderr lines for a single failure — the legacy text plus the
/// typed envelope.
///
/// Pin the deduplicated behavior by running `lfs locks --json` on a
/// detached HEAD and asserting stderr parses as exactly one JSON
/// envelope (no leading legacy line, no trailing duplicate).
async fn test_lfs_locks_on_detached_head_emits_single_error_envelope() {
    let temp_repo = init_temp_repo();
    let temp_path = temp_repo.path();

    // Need at least one commit so HEAD can be detached to it.
    for (k, v) in [
        ("user.name", "tester"),
        ("user.email", "tester@example.com"),
    ] {
        let cfg = libra_command(temp_path)
            .args(["config", k, v])
            .output()
            .unwrap();
        assert!(
            cfg.status.success(),
            "config {k}: {}",
            String::from_utf8_lossy(&cfg.stderr)
        );
    }
    fs::write(temp_path.join("seed.txt"), b"hi").unwrap();
    let add = libra_command(temp_path)
        .args(["add", "seed.txt"])
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "add: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    let commit = libra_command(temp_path)
        .args(["commit", "-m", "seed"])
        .output()
        .unwrap();
    assert!(
        commit.status.success(),
        "commit: {}",
        String::from_utf8_lossy(&commit.stderr)
    );

    // Detach HEAD by checking out the commit hash directly.
    let head = libra_command(temp_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let head_hash = String::from_utf8_lossy(&head.stdout).trim().to_string();
    assert!(
        !head_hash.is_empty(),
        "rev-parse HEAD returned empty; stderr={}",
        String::from_utf8_lossy(&head.stderr)
    );
    let detach = libra_command(temp_path)
        .args(["switch", "--detach", &head_hash])
        .output()
        .unwrap();
    assert!(
        detach.status.success(),
        "switch --detach {head_hash}: {}",
        String::from_utf8_lossy(&detach.stderr)
    );

    let output = libra_command(temp_path)
        .args(["--json", "lfs", "locks"])
        .output()
        .expect("lfs locks should run");
    assert!(
        !output.status.success(),
        "lfs locks on detached HEAD should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let trimmed = stderr.trim();

    // The whole stderr must parse as exactly one JSON envelope; no
    // unwrapped "fatal: HEAD is detached" line leaking before it.
    let envelope: serde_json::Value = serde_json::from_str(trimmed).unwrap_or_else(|err| {
        panic!("stderr should parse as a single JSON envelope: {err}; stderr={trimmed:?}")
    });
    assert_eq!(envelope["error_code"], "LBR-REPO-003");

    // Defensive: the legacy text "fatal: HEAD is detached" must NOT
    // appear as a standalone line before the JSON envelope.
    assert!(
        !trimmed.starts_with("fatal: HEAD is detached"),
        "stderr should not begin with the legacy plain-text error; stderr={trimmed:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// `lfs unlock <path>` (no `--id`) must surface a typed error when
/// `get_locks?path=...` returns an empty list — there is no id to
/// unlock by. Asserts the fatal error envelope carries
/// `LBR-REPO-001` (`RepoStateInvalid`) and a hint-bearing message
/// rather than a generic 500 from the unlock leg or, worse, a panic.
async fn test_lfs_unlock_by_path_returns_typed_error_when_no_lock_found() {
    let app = Router::new().route(
        "/locks",
        get(|| async { Json(json!({ "locks": [], "next_cursor": "" })) }),
    );
    let addr = spawn_mock_lfs_server(app).await;
    let repo = init_repo_with_mock_remote(&format!("http://{addr}"));
    let repo_path = repo.path().to_path_buf();

    let output = tokio::task::spawn_blocking(move || {
        libra_command(&repo_path)
            .args(["--json", "lfs", "unlock", "absent.bin", "--force"])
            .output()
            .expect("failed to run lfs unlock")
    })
    .await
    .expect("spawn_blocking join failed");

    assert!(
        !output.status.success(),
        "lfs unlock without a lock should fail; stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let envelope: serde_json::Value = serde_json::from_str(stderr.trim())
        .unwrap_or_else(|err| panic!("error envelope should be JSON: {err}; stderr={stderr}"));
    assert_eq!(envelope["error_code"], "LBR-REPO-003");
    assert!(
        envelope["message"]
            .as_str()
            .is_some_and(|m| m.contains("no lock found for path 'absent.bin'")),
        "message should mention the offending path; envelope={envelope}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// `lfs unlock --id <id> <path>` should succeed when the path does not
/// exist locally — `--id` makes the path purely a label (the id is the
/// lookup key on the server). Prior to the fix, this case required
/// `--force`, which has stronger semantics (force-release a lock you do
/// not own).
async fn test_lfs_unlock_with_id_skips_path_existence_check() {
    let app = Router::new().route(
        "/locks/{id}/unlock",
        post(|| async {
            Json(json!({
                "lock": {
                    "id": "lock-99",
                    "path": "deleted.bin",
                    "locked_at": "2026-01-01T00:00:00Z",
                    "owner": { "name": "tester" }
                }
            }))
        }),
    );
    let addr = spawn_mock_lfs_server(app).await;
    let repo = init_repo_with_mock_remote(&format!("http://{addr}"));
    let repo_path = repo.path().to_path_buf();

    // Note: no `--force`, and `deleted.bin` does not exist in the repo.
    let output = tokio::task::spawn_blocking(move || {
        libra_command(&repo_path)
            .args(["--json", "lfs", "unlock", "deleted.bin", "--id", "lock-99"])
            .output()
            .expect("failed to run lfs unlock")
    })
    .await
    .expect("spawn_blocking join failed");

    assert!(
        output.status.success(),
        "lfs unlock --id should bypass path check; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("unlock stdout should be JSON");
    assert_eq!(stdout["data"]["action"], "unlock");
    assert_eq!(stdout["data"]["id"], "lock-99");
    assert_eq!(stdout["data"]["path"], "deleted.bin");
}

/// `libra lfs --help` surfaces the EXAMPLES banner so users see the
/// canonical invocation per sub-command (`track`, `untrack`, `ls-files`,
/// `locks`, `lock`, `unlock`) plus a JSON variant without reading the
/// design doc. Cross-cutting `--help` EXAMPLES rollout per
/// `docs/development/commands/_general.md` item B.
#[test]
fn test_lfs_help_lists_examples_banner() {
    let repo = tempfile::tempdir().expect("tempdir for lfs --help");
    let output = libra_command(repo.path())
        .args(["lfs", "--help"])
        .output()
        .expect("failed to run libra lfs --help");
    assert!(
        output.status.success(),
        "lfs --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXAMPLES:"),
        "lfs --help should include EXAMPLES banner, stdout: {stdout}"
    );
    assert!(
        stdout.contains(".libra_attributes"),
        "lfs --help should name the real Libra attributes file, stdout: {stdout}"
    );
    assert!(
        !stdout.contains(".libraattributes"),
        "lfs --help should not mention the old misspelled attributes file, stdout: {stdout}"
    );
    for invocation in [
        "libra lfs track",
        "libra lfs untrack",
        "libra lfs ls-files",
        "libra lfs locks",
        "libra lfs lock build/output.bin",
        "libra lfs unlock build/output.bin",
        "libra lfs unlock --force",
        "libra lfs --json ls-files",
    ] {
        assert!(
            stdout.contains(invocation),
            "lfs --help should include `{invocation}`, stdout: {stdout}"
        );
    }
}

// ── lore.md 2.8: lfs.lockEnforce warn|block gate ────────────────────────────

/// Mock `POST /locks/verify` returning a canned ours/theirs split, with a
/// hit counter so the zero-overhead default is assertable.
fn locks_verify_router(
    hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    theirs_path: &str,
) -> Router {
    let theirs = theirs_path.to_string();
    Router::new().route(
        "/locks/verify",
        axum::routing::post(move || {
            hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = serde_json::json!({
                "ours": [],
                "theirs": [{
                    "id": "lock-1",
                    "path": theirs,
                    "locked_at": "2026-01-01T00:00:00Z",
                    "owner": { "name": "alice" }
                }],
                "next_cursor": ""
            });
            async move { axum::Json(body) }
        }),
    )
}

/// Scaffold: repo with a mock locks server, `*.bin` LFS-tracked, and one
/// LFS-tracked file plus one plain file in the working tree.
async fn lock_enforce_setup(
    theirs_path: &str,
) -> (TempDir, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let addr = spawn_mock_lfs_server(locks_verify_router(hits.clone(), theirs_path)).await;
    let repo = init_repo_with_mock_remote(&format!("http://{addr}"));
    let track = libra_command(repo.path())
        .args(["lfs", "track", "*.bin"])
        .output()
        .expect("lfs track");
    assert!(track.status.success());
    std::fs::write(repo.path().join("asset.bin"), b"payload").expect("write asset");
    std::fs::write(repo.path().join("plain.txt"), b"text").expect("write plain");
    (repo, hits)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lock_enforce_off_by_default_zero_overhead() {
    let (repo, hits) = lock_enforce_setup("asset.bin").await;
    let add = libra_command(repo.path())
        .args(["add", "asset.bin", "plain.txt"])
        .output()
        .expect("add");
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    assert_eq!(
        hits.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "unset policy must perform zero verify requests"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lock_enforce_warn_proceeds_with_warning() {
    let (repo, hits) = lock_enforce_setup("asset.bin").await;
    assert!(
        libra_command(repo.path())
            .args(["config", "lfs.lockEnforce", "warn"])
            .output()
            .expect("config")
            .status
            .success()
    );
    let add = libra_command(repo.path())
        .args(["add", "asset.bin"])
        .output()
        .expect("add");
    assert!(add.status.success(), "warn must proceed");
    let stderr = String::from_utf8_lossy(&add.stderr);
    assert!(
        stderr.contains("locked by alice") && stderr.contains("lock-1"),
        "warning names owner and lock id: {stderr}"
    );
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    // Non-LFS files never trigger verification.
    let add_plain = libra_command(repo.path())
        .args(["add", "plain.txt"])
        .output()
        .expect("add plain");
    assert!(add_plain.status.success());
    assert_eq!(
        hits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "plain file adds perform no verify request"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lock_enforce_block_aborts_atomically_and_commit_gate_fires() {
    let (repo, hits) = lock_enforce_setup("asset.bin").await;
    assert!(
        libra_command(repo.path())
            .args(["config", "lfs.lockEnforce", "block"])
            .output()
            .expect("config")
            .status
            .success()
    );
    let add = libra_command(repo.path())
        .args(["add", "asset.bin"])
        .output()
        .expect("add");
    assert!(!add.status.success(), "block must refuse");
    let stderr = String::from_utf8_lossy(&add.stderr);
    assert!(
        stderr.contains("locked by alice") && stderr.contains("LBR-CONFLICT-002"),
        "typed conflict with detail: {stderr}"
    );
    // Atomic: nothing staged.
    let status = libra_command(repo.path())
        .args(["status", "--porcelain"])
        .output()
        .expect("status");
    assert!(
        !String::from_utf8_lossy(&status.stdout)
            .lines()
            .any(|line| line.starts_with('A')),
        "nothing staged after a blocked add"
    );
    // --dry-run never hits the network.
    let before = hits.load(std::sync::atomic::Ordering::SeqCst);
    let dry = libra_command(repo.path())
        .args(["add", "--dry-run", "asset.bin"])
        .output()
        .expect("dry add");
    assert!(dry.status.success(), "dry-run is a preview");
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), before);
    // Commit gate: stage under warn, then block the commit itself.
    assert!(
        libra_command(repo.path())
            .args(["config", "lfs.lockEnforce", "warn"])
            .output()
            .expect("config")
            .status
            .success()
    );
    assert!(
        libra_command(repo.path())
            .args(["add", "asset.bin"])
            .output()
            .expect("add under warn")
            .status
            .success()
    );
    assert!(
        libra_command(repo.path())
            .args(["config", "lfs.lockEnforce", "block"])
            .output()
            .expect("config")
            .status
            .success()
    );
    let commit = libra_command(repo.path())
        .args(["commit", "-m", "blocked"])
        .output()
        .expect("commit");
    assert!(!commit.status.success(), "commit gate fires");
    assert!(
        String::from_utf8_lossy(&commit.stderr).contains("locked by alice"),
        "{}",
        String::from_utf8_lossy(&commit.stderr)
    );
    // Invalid value is a hard usage error (never silently off) — surfaced
    // when an LFS-tracked path is actually staged (the gate reads config
    // only after finding LFS candidates, preserving zero-overhead default).
    // Modify the file so `add` has a real candidate to gate.
    std::fs::write(repo.path().join("asset.bin"), b"changed payload").expect("modify asset");
    assert!(
        libra_command(repo.path())
            .args(["config", "lfs.lockEnforce", "blocc"])
            .output()
            .expect("config")
            .status
            .success()
    );
    let bad = libra_command(repo.path())
        .args(["add", "asset.bin"])
        .output()
        .expect("add bad policy");
    assert!(
        !bad.status.success(),
        "invalid config value must be a hard error"
    );
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("invalid lfs.lockEnforce"),
        "{}",
        String::from_utf8_lossy(&bad.stderr)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lock_enforce_block_fails_closed_on_unreachable_server() {
    // A realistic "no locking API" server returns 404 WITH a JSON error
    // body (git-lfs spec). verify_locks returns Ok((404, empty)) without
    // decoding, and both modes treat 404 as a clean no-op.
    let addr = spawn_mock_lfs_server(Router::new().route(
        "/locks/verify",
        axum::routing::post(|| async {
            (
                axum::http::StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({ "message": "not found" })),
            )
        }),
    ))
    .await;
    let repo = init_repo_with_mock_remote(&format!("http://{addr}"));
    assert!(
        libra_command(repo.path())
            .args(["lfs", "track", "*.bin"])
            .output()
            .expect("track")
            .status
            .success()
    );
    std::fs::write(repo.path().join("asset.bin"), b"payload").expect("write");
    assert!(
        libra_command(repo.path())
            .args(["config", "lfs.lockEnforce", "block"])
            .output()
            .expect("config")
            .status
            .success()
    );
    // 404 = server has no locking API → clean no-op (mirrors push).
    let add = libra_command(repo.path())
        .args(["add", "asset.bin"])
        .output()
        .expect("add");
    assert!(
        add.status.success(),
        "404 no-locking-API is a documented no-op: {}",
        String::from_utf8_lossy(&add.stderr)
    );
}

/// block + a 5xx server → FAIL CLOSED (an opted-in hard guarantee must not
/// silently degrade); the same server under warn proceeds with a warning.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lock_enforce_block_fails_closed_on_server_error() {
    let addr = spawn_mock_lfs_server(Router::new().route(
        "/locks/verify",
        axum::routing::post(|| async { axum::http::StatusCode::INTERNAL_SERVER_ERROR }),
    ))
    .await;
    let repo = init_repo_with_mock_remote(&format!("http://{addr}"));
    assert!(
        libra_command(repo.path())
            .args(["lfs", "track", "*.bin"])
            .output()
            .expect("track")
            .status
            .success()
    );
    std::fs::write(repo.path().join("asset.bin"), b"payload").expect("write");
    // block: 5xx = unverified → fail closed.
    assert!(
        libra_command(repo.path())
            .args(["config", "lfs.lockEnforce", "block"])
            .output()
            .expect("config")
            .status
            .success()
    );
    let blocked = libra_command(repo.path())
        .args(["add", "asset.bin"])
        .output()
        .expect("add block");
    assert!(!blocked.status.success(), "block must fail closed on 5xx");
    assert!(
        String::from_utf8_lossy(&blocked.stderr).contains("LBR-NET-001"),
        "{}",
        String::from_utf8_lossy(&blocked.stderr)
    );
    // warn: advisory mode proceeds despite the 5xx.
    assert!(
        libra_command(repo.path())
            .args(["config", "lfs.lockEnforce", "warn"])
            .output()
            .expect("config")
            .status
            .success()
    );
    let warned = libra_command(repo.path())
        .args(["add", "asset.bin"])
        .output()
        .expect("add warn");
    assert!(
        warned.status.success(),
        "warn proceeds despite 5xx: {}",
        String::from_utf8_lossy(&warned.stderr)
    );
}

/// A lock on a file in a SUBDIRECTORY must be enforced even when `add` is
/// invoked from that subdirectory (candidates are repo-root-relative and
/// slash-normalized before matching the server's `sub/asset.bin` lock path).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lock_enforce_matches_subdir_lock_from_subdir_cwd() {
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let addr = spawn_mock_lfs_server(locks_verify_router(hits.clone(), "sub/asset.bin")).await;
    let repo = init_repo_with_mock_remote(&format!("http://{addr}"));
    assert!(
        libra_command(repo.path())
            .args(["lfs", "track", "*.bin"])
            .output()
            .expect("track")
            .status
            .success()
    );
    let subdir = repo.path().join("sub");
    std::fs::create_dir_all(&subdir).expect("mkdir sub");
    std::fs::write(subdir.join("asset.bin"), b"payload").expect("write");
    assert!(
        libra_command(repo.path())
            .args(["config", "lfs.lockEnforce", "block"])
            .output()
            .expect("config")
            .status
            .success()
    );
    // Invoke `add asset.bin` FROM the subdirectory — the candidate must still
    // resolve to the repo-root-relative `sub/asset.bin` lock path.
    let add = libra_command(&subdir)
        .args(["add", "asset.bin"])
        .output()
        .expect("add from subdir");
    assert!(
        !add.status.success(),
        "subdir lock must be enforced: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    assert!(
        String::from_utf8_lossy(&add.stderr).contains("sub/asset.bin"),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
}
