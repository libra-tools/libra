//! CLI-level tests for the `cloud` command.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use super::*;

/// Plant one capture session per kind. The fixture drops the local
/// `agent_session` CHECK first so a kind the repository schema does not know
/// yet (a future kind) can be planted too.
fn plant_capture_kinds(repo: &std::path::Path, kinds: &[&str]) {
    let db = repo.join(".libra").join("libra.db");
    let script = r#"
import sqlite3, sys
db, kinds = sys.argv[1], sys.argv[2:]
conn = sqlite3.connect(db)
conn.execute("PRAGMA foreign_keys=OFF")
conn.executescript(
    "CREATE TABLE agent_session_hm02 AS SELECT * FROM agent_session WHERE 0;"
    "DROP TABLE agent_session;"
    "ALTER TABLE agent_session_hm02 RENAME TO agent_session;"
)
for kind in kinds:
    conn.execute(
        "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, working_dir, metadata_json, redaction_report, started_at, last_event_at) VALUES (?, ?, 'hm02-provider', 'stopped', '/work', '{}', '{}', 1, 1)",
        (kind + "-session", kind),
    )
conn.commit()
"#;
    let output = std::process::Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(&db)
        .args(kinds)
        .output()
        .expect("python3 sqlite fixture");
    assert!(
        output.status.success(),
        "fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn plant_unsupported_capture_kind(repo: &std::path::Path) {
    plant_capture_kinds(repo, &["hermes"]);
}

fn assert_unsupported_capture_kind_refused(output: &std::process::Output) {
    assert_capture_kind_refused(output, "hermes");
}

fn assert_capture_kind_refused(output: &std::process::Output, kind: &str) {
    assert_eq!(
        output.status.code(),
        Some(128),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LBR-CONFLICT-002"),
        "missing conflict code: {stderr}"
    );
    assert!(
        stderr.contains("outside the cloud capture allowlist"),
        "missing allowlist refusal: {stderr}"
    );
    assert!(
        stderr.contains(kind),
        "refusal should name the rejected kind: {stderr}"
    );
    assert!(
        stderr.contains(
            "while this repository holds local-only capture sessions of a kind the cloud mirror does not support, `libra cloud sync` keeps refusing until a later Libra release adds complete cloud support for that kind; `--force` does not bypass it"
        ),
        "hint must say the refusal lasts while local-only capture exists: {stderr}"
    );
    assert!(
        stderr.contains("back up the whole repository, including its `.libra` directory, with an external backup tool of your choice"),
        "hint must name the interim external full-repository backup: {stderr}"
    );
    assert!(
        !stderr.contains("missing cloud configuration"),
        "kind refusal ran after credential preflight: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("\"ok\":true")
            && !stdout.contains("Starting cloud sync")
            && !stdout.contains("Sync complete"),
        "refusal rendered success or progress: stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stdout.contains("cloud_sync.") && !stderr.contains("cloud_sync."),
        "refusal emitted a cloud_sync progress event: stdout={stdout} stderr={stderr}"
    );
}

/// A local `hermes` capture row stops sync before D1/R2, including `--force`.
#[test]
fn unsupported_capture_kind_refuses_before_remote_write() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    plant_unsupported_capture_kind(repo.path());

    let output = run_libra_command(&["cloud", "sync"], repo.path());
    assert_unsupported_capture_kind_refused(&output);

    let forced = run_libra_command(&["cloud", "sync", "--force"], repo.path());
    assert_unsupported_capture_kind_refused(&forced);
}

/// JSON and progress mode use the same pre-write refusal and do not emit success.
#[test]
fn unsupported_capture_kind_json_and_progress_do_not_succeed() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    plant_unsupported_capture_kind(repo.path());

    let json = run_libra_command(&["--json", "cloud", "sync"], repo.path());
    assert_unsupported_capture_kind_refused(&json);

    let progress = run_libra_command(&["cloud", "sync", "--progress=json"], repo.path());
    assert_unsupported_capture_kind_refused(&progress);
}

/// PI-05: the fence allowlist is exactly the `agent_kind` set of the D1 DDL
/// `cloud sync` creates, for both capture tables, and every kind in it
/// passes the fence (the run stops later, at the credential preflight).
#[test]
fn unsupported_capture_kind_allowlist_matches_d1_ddl() {
    use libra::utils::d1_client::{
        D1_CAPTURE_AGENT_KINDS, agent_capture_session_v2_table_ddl, agent_session_table_ddl,
        parse_agent_kind_check,
    };

    // The eight kinds today's D1 tables accept, pinned independently of the
    // constant that both the fence and the DDL read.
    const REQUIRED: [&str; 8] = [
        "claude_code",
        "cursor",
        "codex",
        "gemini",
        "opencode",
        "copilot",
        "factory_ai",
        "grok",
    ];
    assert_eq!(D1_CAPTURE_AGENT_KINDS, REQUIRED);
    let expected: Vec<String> = REQUIRED.iter().map(|kind| kind.to_string()).collect();
    for ddl in [
        agent_session_table_ddl(),
        agent_capture_session_v2_table_ddl(),
    ] {
        assert_eq!(
            parse_agent_kind_check(&ddl),
            Some(expected.clone()),
            "{ddl}"
        );
    }

    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    plant_capture_kinds(repo.path(), &REQUIRED);
    let output = run_libra_command(&["cloud", "sync"], repo.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("outside the cloud capture allowlist"),
        "a D1 DDL kind was refused by the fence: {stderr}"
    );
    assert!(
        stderr.contains("missing cloud configuration for sync"),
        "allowed kinds should reach the credential preflight: {stderr}"
    );
}

/// PI-05: an unknown future kind is refused like `hermes`, in every mode.
/// PI-01 registers `pi` as a local-only identity; it stays refused too.
#[test]
fn unsupported_capture_kind_future_kind_is_refused() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    plant_capture_kinds(repo.path(), &["claude_code", "pi", "vogon"]);

    for args in [
        &["cloud", "sync"][..],
        &["cloud", "sync", "--force"][..],
        &["--json", "cloud", "sync"][..],
        &["cloud", "sync", "--progress=json"][..],
    ] {
        let output = run_libra_command(args, repo.path());
        assert_capture_kind_refused(&output, "pi");
        assert_capture_kind_refused(&output, "vogon");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("claude_code"),
            "only the kinds outside the allowlist are named: {stderr}"
        );
    }
}

/// Count TCP connections to a loopback listener that closes each one at once.
struct ConnectionCounter {
    url: String,
    count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl ConnectionCounter {
    fn start() -> Self {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind counter");
        listener.set_nonblocking(true).expect("nonblocking counter");
        let url = format!("http://{}", listener.local_addr().expect("counter addr"));
        let count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let count = count.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            count.fetch_add(1, Ordering::SeqCst);
                            drop(stream);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        Self {
            url,
            count,
            stop,
            worker: Some(worker),
        }
    }

    fn connections(&self) -> usize {
        self.count.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for ConnectionCounter {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// PI-05: with full (fake) cloud credentials and every HTTP(S) request routed
/// to a local counter, the refusal makes no remote request at all; the
/// control run with an allowed kind shows the counter does see the first
/// D1 request, so the zero is meaningful.
#[test]
fn unsupported_capture_kind_makes_no_remote_request() {
    let counter = ConnectionCounter::start();
    let env = [
        ("LIBRA_D1_ACCOUNT_ID", "pi05-account"),
        ("LIBRA_D1_API_TOKEN", "pi05-token"),
        ("LIBRA_D1_DATABASE_ID", "pi05-database"),
        ("LIBRA_STORAGE_ENDPOINT", counter.url.as_str()),
        ("LIBRA_STORAGE_ALLOW_HTTP", "true"),
        ("LIBRA_STORAGE_BUCKET", "pi05-bucket"),
        ("LIBRA_STORAGE_ACCESS_KEY", "pi05-access"),
        ("LIBRA_STORAGE_SECRET_KEY", "pi05-secret"),
        ("HTTPS_PROXY", counter.url.as_str()),
        ("HTTP_PROXY", counter.url.as_str()),
        ("ALL_PROXY", counter.url.as_str()),
    ];

    let refused = tempdir().unwrap();
    init_repo_via_cli(refused.path());
    plant_unsupported_capture_kind(refused.path());
    for args in [&["cloud", "sync"][..], &["cloud", "sync", "--force"][..]] {
        let output = run_libra_command_with_env(args, refused.path(), &env);
        assert_unsupported_capture_kind_refused(&output);
    }
    assert_eq!(
        counter.connections(),
        0,
        "the refusal must precede every remote request"
    );

    let allowed = tempdir().unwrap();
    init_repo_via_cli(allowed.path());
    plant_capture_kinds(allowed.path(), &["claude_code"]);
    let control = run_libra_command_with_env(&["cloud", "sync"], allowed.path(), &env);
    assert!(
        !control.status.success(),
        "the control run cannot succeed against the counter"
    );
    assert!(
        counter.connections() > 0,
        "control run made no request through the counter: stderr={}",
        String::from_utf8_lossy(&control.stderr)
    );
}

/// Running `cloud sync` outside a repository should return exit code 128.
#[test]
fn test_cloud_cli_outside_repository_returns_fatal_128() {
    let temp = tempdir().unwrap();

    let output = run_libra_command(&["cloud", "sync"], temp.path());
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal: not a libra repository"),
        "unexpected stderr: {stderr}"
    );
}

/// `cloud status` is local-only and supports structured output without requiring
/// Cloudflare credentials.
#[test]
fn test_cloud_status_json_output_empty_repo() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["--json", "cloud", "status"], repo.path());
    assert_cli_success(&output, "cloud status --json failed");
    assert!(output.stderr.is_empty());

    let json = parse_json_stdout(&output);
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "cloud.status");
    assert!(
        json["data"]["repo_id"]
            .as_str()
            .is_some_and(|repo_id| !repo_id.is_empty()),
        "repo_id should be populated: {json}"
    );
    assert_eq!(json["data"]["total_objects"], 0);
    assert_eq!(json["data"]["synced"], 0);
    assert_eq!(json["data"]["pending"], 0);
    assert_eq!(json["data"]["synced_percent"], 0);
    assert_eq!(json["data"]["by_type"].as_array().unwrap().len(), 0);
    assert!(json["data"].get("unsynced_objects").is_none());
}

/// `--machine` emits the same status envelope as one NDJSON record.
#[test]
fn test_cloud_status_machine_output_empty_repo() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["--machine", "cloud", "status"], repo.path());
    assert_cli_success(&output, "cloud status --machine failed");
    assert!(output.stderr.is_empty());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.lines().count(), 1, "unexpected stdout: {stdout}");
    let json: serde_json::Value =
        serde_json::from_str(stdout.trim_end()).expect("machine stdout should be JSON");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "cloud.status");
    assert_eq!(json["data"]["total_objects"], 0);
}

/// `cloud sync --quiet` should not emit legacy stdout progress even when
/// preflight env validation fails.
#[test]
fn test_cloud_sync_quiet_preflight_failure_has_no_stdout_progress() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["--quiet", "cloud", "sync"], repo.path());
    assert_ne!(output.status.code(), Some(0));
    assert!(
        output.stdout.is_empty(),
        "quiet sync should not print legacy progress to stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        !human.contains("Starting cloud sync..."),
        "quiet sync must not leak legacy start message: {human}"
    );
    assert_eq!(
        report.details.get("operation"),
        Some(&serde_json::json!("sync"))
    );
    assert_eq!(
        report.details.get("component"),
        Some(&serde_json::json!("cloud"))
    );
}

/// `cloud sync --json` failure path should avoid emitting legacy human progress.
#[test]
fn test_cloud_sync_json_preflight_failure_has_no_human_progress() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["--json=compact", "cloud", "sync"], repo.path());
    assert_ne!(output.status.code(), Some(0));
    assert!(
        output.stdout.is_empty(),
        "failed json sync should not emit success envelope: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        !human.contains("Starting cloud sync..."),
        "json sync must not leak legacy start message: {human}"
    );
    assert_eq!(
        report.details.get("operation"),
        Some(&serde_json::json!("sync"))
    );
    assert_eq!(
        report.details.get("component"),
        Some(&serde_json::json!("cloud"))
    );
}

/// `cloud sync --json --progress=json` should emit NDJSON progress events on
/// stderr without leaking legacy human progress lines.
#[test]
fn test_cloud_sync_json_progress_emits_start_event() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let output = run_libra_command(
        &["--json=compact", "--progress=json", "cloud", "sync"],
        repo.path(),
    );
    assert_ne!(output.status.code(), Some(0));
    assert!(
        output.stdout.is_empty(),
        "failed json sync should not emit success envelope: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("\"event\":\"cloud_sync.start\""),
        "expected cloud sync progress start event in stderr: {stderr}"
    );
    assert!(
        !stderr.contains("Starting cloud sync..."),
        "json progress mode must not leak legacy start message: {stderr}"
    );
}

/// `cloud sync --progress=json` in human mode should switch from legacy stdout
/// progress lines to structured stderr events.
#[test]
fn test_cloud_sync_human_progress_json_emits_event_without_stdout_progress() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["--progress=json", "cloud", "sync"], repo.path());
    assert_ne!(output.status.code(), Some(0));
    assert!(
        output.stdout.is_empty(),
        "progress=json should suppress legacy stdout progress: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("\"event\":\"cloud_sync.start\""),
        "expected cloud sync progress start event in stderr: {stderr}"
    );
    assert!(
        !stderr.contains("Starting cloud sync..."),
        "progress=json must not leak legacy start message: {stderr}"
    );
}

/// A zero batch cannot enter the object-sync loop or mark objects as synced.
/// The CLI helper clears cloud credentials, so this also pins validation ahead
/// of any remote preflight or D1/R2 side effect.
#[test]
fn test_cloud_sync_zero_batch_rejects_before_index_or_remote_write() {
    let repo = create_committed_repo_via_cli();
    let status_before = run_libra_command(&["--json", "cloud", "status"], repo.path());
    assert_cli_success(&status_before, "cloud status before rejected sync failed");
    let status_before = parse_json_stdout(&status_before);
    assert!(
        status_before["data"]["pending"]
            .as_u64()
            .is_some_and(|n| n > 0),
        "committed fixture should contain unsynced objects: {status_before}"
    );

    let sync = run_libra_command(
        &["--json=compact", "cloud", "sync", "--batch-size", "0"],
        repo.path(),
    );
    assert_ne!(sync.status.code(), Some(0));
    assert!(sync.stdout.is_empty(), "failed sync emitted success output");
    let (human, report) = parse_cli_error_stderr(&sync.stderr);
    assert!(
        !human.contains("Starting cloud sync..."),
        "rejected sync should not emit start progress: {human}"
    );
    assert!(
        report.message.contains("Batch size must be at least 1"),
        "unexpected error: {}",
        report.message
    );
    assert_eq!(
        report.details.get("operation"),
        Some(&serde_json::json!("sync"))
    );

    let status_after = run_libra_command(&["--json", "cloud", "status"], repo.path());
    assert_cli_success(&status_after, "cloud status after rejected sync failed");
    let status_after = parse_json_stdout(&status_after);
    assert_eq!(
        status_after["data"]["repo_id"],
        status_before["data"]["repo_id"]
    );
    assert_eq!(status_before["data"]["synced"], 0);
    assert_eq!(
        status_after["data"]["synced"], 0,
        "rejected sync must not mark any local object as uploaded"
    );
    assert_eq!(
        status_after["data"]["total_objects"], status_before["data"]["total_objects"],
        "rejected sync must not create operation-log snapshot objects"
    );
    assert_eq!(
        status_after["data"]["pending"], status_before["data"]["pending"],
        "rejected sync must leave pending object count unchanged"
    );
}

/// Regression: `libra cloud sync` must not write Operation Log v2 snapshot
/// objects into `.libra/objects` / `object_index` before running; otherwise
/// every repeat `cloud sync` creates new unsynced blobs and can never become
/// an idempotent no-op.
#[test]
fn test_cloud_sync_does_not_create_operation_snapshot_objects() {
    let repo = create_committed_repo_via_cli();

    let status_before = run_libra_command(&["--json", "cloud", "status"], repo.path());
    assert_cli_success(&status_before, "cloud status before sync failed");
    let before = parse_json_stdout(&status_before);

    // Fails at missing-credentials preflight after CLI dispatch + operation
    // classification, so any central operation-log pre-snapshot would have
    // already written new blobs into `object_index`.
    let sync = run_libra_command(&["--json", "cloud", "sync"], repo.path());
    assert_ne!(sync.status.code(), Some(0));

    let status_after = run_libra_command(&["--json", "cloud", "status"], repo.path());
    assert_cli_success(&status_after, "cloud status after sync failed");
    let after = parse_json_stdout(&status_after);

    assert_eq!(
        after["data"]["total_objects"], before["data"]["total_objects"],
        "cloud sync must not mint Operation Log snapshot objects: before={before} after={after}"
    );
    assert_eq!(
        after["data"]["pending"], before["data"]["pending"],
        "cloud sync must not inflate pending object count: before={before} after={after}"
    );
}

/// `libra cloud --help` surfaces the EXAMPLES banner so users see the
/// canonical invocation per sub-command (`status`, `sync`, `restore`)
/// plus force-sync and JSON variants without reading the design doc.
/// Cross-cutting `--help` EXAMPLES rollout per
/// `docs/development/commands/_general.md` item B.
#[test]
fn test_cloud_help_lists_examples_banner() {
    let repo = tempdir().expect("tempdir for cloud --help");
    let output = run_libra_command(&["cloud", "--help"], repo.path());
    assert!(
        output.status.success(),
        "cloud --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXAMPLES:"),
        "cloud --help should include EXAMPLES banner, stdout: {stdout}"
    );
    for invocation in [
        "libra cloud status",
        "libra cloud sync",
        "libra cloud sync --force",
        "libra cloud restore --name my-project",
        "libra cloud restore --repo-id",
        "libra cloud --json sync",
        "libra cloud sync --progress=json",
    ] {
        assert!(
            stdout.contains(invocation),
            "cloud --help should include `{invocation}`, stdout: {stdout}"
        );
    }
}
