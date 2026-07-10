//! AG-22 read-only agent review workflow tests
//! (`docs/development/tracing/plan.md` Task A7; `agent.md` E8 +
//! 落地执行补充规格 §5 / test-matrix pins).
//!
//! **Layer:** L1 — deterministic. Reviewers are fake `/bin/sh` scripts
//! from `tests/fixtures/agent_workflows/` (see the provenance README
//! there), driven through the `ReviewerSource::Custom` /
//! `ReviewerCommand` test seam — no network, no credentials, no real
//! agent CLIs. CLI-surface scenarios (`--fix`, `list` pagination,
//! `cancel` idempotency) go through the real binary
//! (`CARGO_BIN_EXE_libra`) with an isolated `HOME`, matching the
//! `tests/command/mod.rs` helper shape.
//!
//! Pinned scenario names (agent.md test matrix, AG-22 / E8 row):
//! - `fake_reviewers_cover_success_error_cancel_and_slow_output`
//! - `review_sink_is_not_blocked_by_high_frequency_reviewer`
//! - `review_read_only_emits_findings_manifest_and_manual_attach`
//! - `review_fix_returns_unsupported_until_bridge_ready`
//! - `cancel_releases_reviewer_processes_and_locks`
//!
//! plus the plan.md:961 stress case (cancel during pending output) and
//! the 强制补强项 #5 keyset-pagination envelope through the real CLI.
//! (`review_fix_bridge_enters_agent_runtime_mutating_path` is the
//! matrix's fix-bridge alternative; it only lands once the internal fix
//! bridge has a source anchor — until then the unsupported pin below is
//! the mandatory one.)
//!
//! No test changes the process working directory or process
//! environment, so none of them need `#[serial]`.

#![cfg(unix)]

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{Duration, Instant},
};

use libra::internal::ai::review::{
    REVIEW_SINK_BUFFER_BYTES, REVIEW_SINK_TRUNCATION_MARKER, ReviewCancelHandle, ReviewRunOutcome,
    ReviewRunRequest, ReviewRunStore, ReviewTerminalState, ReviewerCommand, ReviewerOutcome,
    ReviewerSource, UNTRUSTED_FINDINGS_CLOSE, UNTRUSTED_FINDINGS_OPEN_PREFIX, process_start_ticks,
    run_review,
};

/// The fake `sk-` credential `reviewer-success.sh` assembles at run time
/// (never a literal in the fixture); it must never survive redaction.
const FAKE_CREDENTIAL: &str = "sk-abcdefghijklmnopqrstuvwx123456";

// ---------------------------------------------------------------------------
// Helpers: real-CLI invocation (isolated HOME), repo + fixture staging
// ---------------------------------------------------------------------------

/// Run the Libra binary with an isolated HOME so host config never leaks
/// into tests (`tests/command/mod.rs::base_libra_command` shape).
fn run_libra(args: &[&str], cwd: &Path, extra_env: &[(&str, &str)]) -> Output {
    let home = cwd.parent().unwrap_or(cwd).join(".libra-test-home");
    let config_home = home.join(".config");
    let global_db = home.join(".libra").join("config.db");
    std::fs::create_dir_all(&config_home).expect("create isolated config dir");

    let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("LIBRA_CONFIG_GLOBAL_DB", &global_db)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env(libra::utils::pager::LIBRA_TEST_ENV, "1");
    if let Some(llvm_profile_file) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", llvm_profile_file);
    }
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command.output().expect("failed to execute libra binary")
}

fn assert_cli_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `libra init` a fresh repository under `<root>/repo`.
fn init_repo(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).expect("create repo dir");
    assert_cli_success(&run_libra(&["init"], &repo, &[]), "libra init");
    repo
}

/// A real Libra repository with one commit, so `repo_root` has a valid
/// HEAD and the mandatory isolated-workspace copy mirrors real content.
fn init_committed_repo(root: &Path) -> PathBuf {
    let repo = init_repo(root);
    assert_cli_success(
        &run_libra(&["config", "user.name", "Libra Review Test"], &repo, &[]),
        "config user.name",
    );
    assert_cli_success(
        &run_libra(
            &["config", "user.email", "review-test@example.com"],
            &repo,
            &[],
        ),
        "config user.email",
    );
    std::fs::write(repo.join("tracked.txt"), "tracked content\n").expect("seed tracked file");
    assert_cli_success(&run_libra(&["add", "tracked.txt"], &repo, &[]), "libra add");
    assert_cli_success(
        &run_libra(&["commit", "-m", "base", "--no-verify"], &repo, &[]),
        "libra commit",
    );
    repo
}

/// The run store the engine and the CLI share for a repo
/// (`<repo>/.libra/sessions/agent-runs/<run_id>/`).
fn store_for(repo: &Path) -> ReviewRunStore {
    ReviewRunStore::new(repo.join(".libra").join("sessions"))
}

/// Copy a fixture reviewer script into `dir` and (re-)apply `0o755` so a
/// checkout that dropped file modes cannot break the suite.
fn stage_fixture(dir: &Path, name: &str) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("agent_workflows")
        .join(name);
    std::fs::create_dir_all(dir).expect("create fixture staging dir");
    let target = dir.join(name);
    std::fs::copy(&source, &target)
        .unwrap_or_else(|error| panic!("failed to stage fixture {name}: {error}"));
    let mut perms = std::fs::metadata(&target)
        .expect("fixture metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&target, perms).expect("fixture chmod");
    target
}

/// Directly constructed `ReviewerCommand` — the documented test seam.
/// The environment is deliberately EMPTY: fixtures must survive the
/// production `env_clear()` spawn contract. There is no cwd knob:
/// the engine unconditionally runs every reviewer inside the isolated
/// workspace (plan.md:947).
fn fake_reviewer(slug: &str, program: &Path, args: &[&str], timeout: Duration) -> ReviewerSource {
    ReviewerSource::Custom(ReviewerCommand {
        slug: slug.to_string(),
        program: program.to_path_buf(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
        env: Vec::new(),
        timeout,
    })
}

/// Drive one run to a terminal state under a firm test deadline —
/// a hang is a test failure, never a stuck CI job.
async fn run_bounded(
    store: &ReviewRunStore,
    request: ReviewRunRequest,
    cancel: ReviewCancelHandle,
    deadline_secs: u64,
) -> ReviewRunOutcome {
    tokio::time::timeout(
        Duration::from_secs(deadline_secs),
        run_review(store, request, cancel),
    )
    .await
    .expect("review run must finish within the test deadline")
    .expect("review run reaches a terminal state")
}

/// Assert no `libra-task-worktree-*` workspace is left under the repo's
/// task-worktree base after a run — the workspace lease was released.
fn assert_no_leaked_workspace(repo: &Path) {
    let tasks_dir = repo.join(".libra").join("worktrees").join("tasks");
    if !tasks_dir.exists() {
        return;
    }
    let leaked: Vec<String> = std::fs::read_dir(&tasks_dir)
        .expect("read task worktree dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("libra-task-worktree-"))
        .collect();
    assert!(
        leaked.is_empty(),
        "review run leaked isolated workspaces: {leaked:?}"
    );
}

// ---------------------------------------------------------------------------
// Pinned scenario 1: success / error / cancel / slow output
// ---------------------------------------------------------------------------

/// One mixed run (success + error + slow reviewer) reaches `partial`
/// with per-reviewer outcomes and captured late output; a second run
/// with a long-sleeping reviewer is cancelled through the shared handle
/// and terminates promptly as `cancelled`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fake_reviewers_cover_success_error_cancel_and_slow_output() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = init_committed_repo(temp.path());
    let store = store_for(&repo);
    let fixtures = temp.path().join("fixtures");
    let success = stage_fixture(&fixtures, "reviewer-success.sh");
    let error = stage_fixture(&fixtures, "reviewer-error.sh");
    let slow = stage_fixture(&fixtures, "reviewer-slow.sh");

    // ---- Run A: success + error + slow(1s) → partial. ----
    let request = ReviewRunRequest::new(
        &repo,
        "review the changes",
        "HEAD~1..HEAD",
        "sha-workflow-a",
        vec![
            fake_reviewer("fake-success", &success, &[], Duration::from_secs(60)),
            fake_reviewer("fake-error", &error, &[], Duration::from_secs(60)),
            fake_reviewer("fake-slow", &slow, &["1"], Duration::from_secs(60)),
        ],
    );
    let outcome = run_bounded(&store, request, ReviewCancelHandle::new(), 120).await;
    assert_eq!(outcome.terminal_state, ReviewTerminalState::Partial);
    assert_eq!(outcome.reviewers.len(), 3);
    assert_eq!(outcome.reviewers[0].outcome, ReviewerOutcome::Ok);
    assert_eq!(outcome.reviewers[0].exit_code, Some(0));
    assert_eq!(outcome.reviewers[1].outcome, ReviewerOutcome::Failed);
    assert_eq!(outcome.reviewers[1].exit_code, Some(3));
    assert_eq!(
        outcome.reviewers[2].outcome,
        ReviewerOutcome::Ok,
        "slow reviewer output after its sleep must still count as success"
    );

    let findings = store
        .read_findings(&outcome.run_id)
        .expect("read findings")
        .expect("findings exist");
    assert!(findings.contains("looks-good"), "{findings}");
    assert!(
        findings.contains("slow-finding-after-sleep"),
        "late output must be captured: {findings}"
    );
    let stderr_log = store
        .reviewer_stderr_log_path(&outcome.run_id, "fake-error")
        .expect("stderr log path");
    let stderr_text = std::fs::read_to_string(stderr_log).expect("stderr log");
    assert!(
        stderr_text.contains("reviewer exploded"),
        "failed reviewer's stderr must persist: {stderr_text}"
    );
    let state = store
        .load_state(&outcome.run_id)
        .expect("load state")
        .expect("state exists");
    assert_eq!(state.terminal_state, Some(ReviewTerminalState::Partial));
    assert_no_leaked_workspace(&repo);

    // ---- Run B: long sleeper cancelled through the shared handle. ----
    let request = ReviewRunRequest::new(
        &repo,
        "review the changes",
        "HEAD~1..HEAD",
        "sha-workflow-b",
        vec![fake_reviewer(
            "fake-sleeper",
            &slow,
            &["30"],
            Duration::from_secs(120),
        )],
    );
    let cancel = ReviewCancelHandle::new();
    let started = Instant::now();
    let run = tokio::spawn({
        let store = store.clone();
        let cancel = cancel.clone();
        async move { run_review(&store, request, cancel).await }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    cancel.cancel();
    let outcome = tokio::time::timeout(Duration::from_secs(30), run)
        .await
        .expect("cancelled run must finish promptly")
        .expect("join run task")
        .expect("run reaches a terminal state");
    assert_eq!(outcome.terminal_state, ReviewTerminalState::Cancelled);
    assert_eq!(outcome.reviewers[0].outcome, ReviewerOutcome::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(25),
        "cancel must not wait out the 30s sleeper (took {:?})",
        started.elapsed()
    );
    let state = store
        .load_state(&outcome.run_id)
        .expect("load state")
        .expect("state exists");
    assert_eq!(state.terminal_state, Some(ReviewTerminalState::Cancelled));
    assert_no_leaked_workspace(&repo);
}

// ---------------------------------------------------------------------------
// Pinned scenario 2: sink is not blocked by a flooding reviewer
// ---------------------------------------------------------------------------

/// A reviewer flooding ~1 MiB of stdout never starves a quiet sibling:
/// the quiet reviewer's full output persists verbatim, the flood log is
/// truncated at the 64 KiB per-sink cap with the marker appended, and
/// the whole run completes well inside its deadline
/// (`agent.md` perf row: review sink).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_sink_is_not_blocked_by_high_frequency_reviewer() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = init_committed_repo(temp.path());
    let store = store_for(&repo);
    let fixtures = temp.path().join("fixtures");
    let flood = stage_fixture(&fixtures, "reviewer-flood.sh");
    let quiet = stage_fixture(&fixtures, "reviewer-quiet.sh");

    let request = ReviewRunRequest::new(
        &repo,
        "review the changes",
        "HEAD~1..HEAD",
        "sha-sink-flood",
        vec![
            fake_reviewer("fake-flood", &flood, &[], Duration::from_secs(120)),
            fake_reviewer("fake-quiet", &quiet, &[], Duration::from_secs(120)),
        ],
    );
    let started = Instant::now();
    let outcome = run_bounded(&store, request, ReviewCancelHandle::new(), 120).await;
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "flooded run must complete promptly (took {:?})",
        started.elapsed()
    );
    assert_eq!(outcome.terminal_state, ReviewTerminalState::Success);
    assert!(
        outcome.reviewers[0].stdout_truncated,
        "the flooding reviewer must be truncated"
    );
    assert!(
        !outcome.reviewers[1].stdout_truncated,
        "the quiet reviewer must not be truncated"
    );

    // Quiet reviewer: FULL output persisted, in order.
    let quiet_log = store
        .reviewer_stdout_log_path(&outcome.run_id, "fake-quiet")
        .expect("quiet log path");
    let quiet_text = std::fs::read_to_string(quiet_log).expect("quiet log");
    assert!(
        quiet_text.contains("quiet-finding-alpha\nquiet-finding-beta\n"),
        "the flooder must not starve or corrupt the quiet reviewer's output: {quiet_text}"
    );

    // Flood reviewer: capped at 64 KiB (+ marker slack) with the marker.
    let flood_log = store
        .reviewer_stdout_log_path(&outcome.run_id, "fake-flood")
        .expect("flood log path");
    let flood_text = std::fs::read_to_string(flood_log).expect("flood log");
    assert!(
        flood_text.contains(REVIEW_SINK_TRUNCATION_MARKER),
        "truncated flood log must carry the truncation marker"
    );
    assert!(
        flood_text.len() <= REVIEW_SINK_BUFFER_BYTES + 256,
        "flood log must stay within the per-sink cap plus marker slack (got {} bytes)",
        flood_text.len()
    );
    assert_no_leaked_workspace(&repo);
}

// ---------------------------------------------------------------------------
// Pinned scenario 3: E8 findings/manifest/manual_attach wire
// ---------------------------------------------------------------------------

/// A read-only run persists the E8-libra run wire exactly:
/// `manifest.json` with EXACTLY the 12 E8 keys and the empty
/// `manual_attach` placeholder, `findings.md` with spotlighting
/// delimiters around redacted reviewer text, and both
/// `reviewers/<slug>.{stdout,stderr}.redacted.log` files.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_read_only_emits_findings_manifest_and_manual_attach() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = init_committed_repo(temp.path());
    let store = store_for(&repo);
    let fixtures = temp.path().join("fixtures");
    let success = stage_fixture(&fixtures, "reviewer-success.sh");

    let request = ReviewRunRequest::new(
        &repo,
        "review the changes",
        "HEAD~1..HEAD",
        "sha-e8-wire",
        vec![fake_reviewer(
            "fake-success",
            &success,
            &[],
            Duration::from_secs(60),
        )],
    );
    let outcome = run_bounded(&store, request, ReviewCancelHandle::new(), 120).await;
    assert_eq!(outcome.terminal_state, ReviewTerminalState::Success);

    // ---- manifest.json: exactly the 12 E8 keys (agent.md:876/:1321). ----
    let manifest_path = store.manifest_path(&outcome.run_id).expect("manifest path");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(manifest_path).expect("read manifest"))
            .expect("manifest parses");
    let mut keys: Vec<&str> = manifest
        .as_object()
        .expect("manifest is a JSON object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    let mut expected = vec![
        "schema_version",
        "run_id",
        "kind",
        "agents",
        "starting_sha",
        "target_scope",
        "terminal_state",
        "created_at",
        "updated_at",
        "findings_oid",
        "redaction_report",
        "manual_attach",
    ];
    expected.sort_unstable();
    assert_eq!(
        keys, expected,
        "manifest.json must carry exactly the E8 keys"
    );
    assert_eq!(manifest["schema_version"], 1);
    assert_eq!(manifest["run_id"], outcome.run_id.as_str());
    assert_eq!(manifest["kind"], "review");
    assert_eq!(manifest["agents"], serde_json::json!(["fake-success"]));
    assert_eq!(manifest["starting_sha"], "sha-e8-wire");
    assert_eq!(manifest["target_scope"], "HEAD~1..HEAD");
    assert_eq!(manifest["terminal_state"], "success");
    // A0-06: findings.md is objectized at finalize; findings_oid is a real
    // git object id (verified against the content hash after findings.md is
    // read below).
    let findings_oid = manifest["findings_oid"]
        .as_str()
        .expect("A0-06 populates findings_oid with a real object id");
    assert!(
        findings_oid.len() == 40 || findings_oid.len() == 64,
        "findings_oid must be a git object id: {findings_oid}"
    );
    // No `review attach` was invoked, so manual_attach stays empty (populated
    // only by the attach command surface — see review_artifacts_objectized).
    assert_eq!(manifest["manual_attach"], serde_json::json!([]));
    assert!(
        manifest["redaction_report"]["matches"]
            .as_u64()
            .expect("matches is a number")
            >= 1,
        "the fake credential must have hit at least one redaction rule: {manifest}"
    );

    // ---- findings.md: spotlighting delimiters + redacted text. ----
    let findings = store
        .read_findings(&outcome.run_id)
        .expect("read findings")
        .expect("findings exist");
    // A0-06: the objectized blob at findings_oid must decode to findings.md.
    let obj_path = repo
        .join(".libra")
        .join("objects")
        .join(&findings_oid[..2])
        .join(&findings_oid[2..]);
    let raw = std::fs::read(&obj_path).expect("findings object exists on disk");
    let mut decoder = flate2::read::ZlibDecoder::new(&raw[..]);
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decoded).expect("zlib decode findings object");
    let header_end = decoded.iter().position(|&b| b == 0).expect("object header");
    assert_eq!(
        &decoded[header_end + 1..],
        findings.as_bytes(),
        "the findings object must match findings.md bytes"
    );
    assert!(
        findings.contains(&format!(
            "{UNTRUSTED_FINDINGS_OPEN_PREFIX} slug=\"fake-success\">>>"
        )),
        "findings must open a spotlighting block per reviewer: {findings}"
    );
    assert!(
        findings.contains(UNTRUSTED_FINDINGS_CLOSE),
        "findings must close the spotlighting block: {findings}"
    );
    assert!(findings.contains("looks-good"), "{findings}");
    assert!(
        !findings.contains(FAKE_CREDENTIAL),
        "the fake credential must never survive redaction into findings.md"
    );

    // ---- reviewers/<slug>.{stdout,stderr}.redacted.log exist. ----
    let stdout_log = store
        .reviewer_stdout_log_path(&outcome.run_id, "fake-success")
        .expect("stdout log path");
    let stderr_log = store
        .reviewer_stderr_log_path(&outcome.run_id, "fake-success")
        .expect("stderr log path");
    assert!(stdout_log.is_file(), "stdout redacted log must exist");
    assert!(stderr_log.is_file(), "stderr redacted log must exist");
    let stdout_text = std::fs::read_to_string(stdout_log).expect("stdout log");
    assert!(stdout_text.contains("looks-good"), "{stdout_text}");
    assert!(
        !stdout_text.contains(FAKE_CREDENTIAL),
        "the fake credential must never survive redaction into the log"
    );
    assert!(
        !stdout_text.contains('\u{1b}'),
        "persisted logs must be control-scrubbed (no raw ESC): {stdout_text:?}"
    );
    assert_no_leaked_workspace(&repo);
}

// ---------------------------------------------------------------------------
// Pinned scenario 4: --fix fails closed with LBR-AGENT-010
// ---------------------------------------------------------------------------

/// `libra review --agent codex --fix` fails closed with the stable
/// `LBR-AGENT-010` code (exit 128) until the internal AgentRuntime fix
/// bridge lands — never fake success (plan.md:950).
#[test]
fn review_fix_returns_unsupported_until_bridge_ready() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = init_repo(temp.path());

    // Human/default surface.
    let output = run_libra(&["review", "--agent", "codex", "--fix"], &repo, &[]);
    assert_eq!(
        output.status.code(),
        Some(128),
        "--fix must be a fatal (128) refusal: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        stderr.contains("LBR-AGENT-010"),
        "stderr must carry the stable code: {stderr}"
    );
    assert!(
        stderr.contains("fix bridge"),
        "the refusal must name the missing fix bridge: {stderr}"
    );

    // Structured JSON error surface.
    let output = run_libra(
        &["review", "--agent", "codex", "--fix"],
        &repo,
        &[("LIBRA_ERROR_JSON", "1")],
    );
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let json_start = stderr
        .rfind("\n{")
        .map(|index| index + 1)
        .or_else(|| stderr.find('{'))
        .expect("structured stderr must carry a JSON error report");
    let report: serde_json::Value =
        serde_json::from_str(stderr[json_start..].trim()).expect("JSON error report parses");
    assert_eq!(report["error_code"], "LBR-AGENT-010");
    assert_eq!(report["exit_code"], 128);
}

// ---------------------------------------------------------------------------
// Pinned scenario 5: cancel releases processes, workspace, and marker
// ---------------------------------------------------------------------------

/// The cross-process cancel marker (the same file `review cancel`
/// writes) terminates a live run: the reviewer process is no longer
/// alive afterwards (`kill -0` fails), the isolated workspace is
/// released, the run is stamped `cancelled`, and a second cancel — both
/// store-level and through the real CLI — is idempotent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_releases_reviewer_processes_and_locks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = init_committed_repo(temp.path());
    let store = store_for(&repo);
    let fixtures = temp.path().join("fixtures");
    let pidfile_script = stage_fixture(&fixtures, "reviewer-pidfile.sh");
    let pidfile = temp.path().join("reviewer.pid");
    let pidfile_arg = pidfile.display().to_string();

    let request = ReviewRunRequest::new(
        &repo,
        "review the changes",
        "HEAD~1..HEAD",
        "sha-cancel-pid",
        vec![fake_reviewer(
            "fake-pidfile",
            &pidfile_script,
            &[pidfile_arg.as_str()],
            Duration::from_secs(120),
        )],
    );
    let run = tokio::spawn({
        let store = store.clone();
        async move { run_review(&store, request, ReviewCancelHandle::new()).await }
    });

    // Wait until the reviewer recorded its PID and the run dir exists.
    let deadline = Instant::now() + Duration::from_secs(30);
    let (run_id, pid) = loop {
        assert!(
            Instant::now() < deadline,
            "reviewer never started (pidfile missing or run not listed)"
        );
        if pidfile.is_file()
            && let Ok(text) = std::fs::read_to_string(&pidfile)
            && let Ok(pid) = text.trim().parse::<i32>()
        {
            let runs = store.list_runs().expect("list runs");
            if let Some(run) = runs.first() {
                break (run.run_id.clone(), pid);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        // SAFETY: signal 0 only probes liveness of a PID we spawned.
        unsafe { libc::kill(pid, 0) },
        0,
        "reviewer must be alive before cancel"
    );

    // Cross-process cancel: drop the marker the runner polls.
    assert!(
        store.mark_cancel_requested(&run_id).expect("mark cancel"),
        "the run directory must accept the cancel marker"
    );
    let outcome = tokio::time::timeout(Duration::from_secs(30), run)
        .await
        .expect("cancelled run must finish promptly")
        .expect("join run task")
        .expect("run reaches a terminal state");
    assert_eq!(outcome.run_id, run_id);
    assert_eq!(outcome.terminal_state, ReviewTerminalState::Cancelled);
    assert_eq!(outcome.reviewers[0].outcome, ReviewerOutcome::Cancelled);

    // The reviewer process (the exec'd sleep) must be gone.
    let dead_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        // SAFETY: signal 0 only probes liveness of a PID we spawned.
        if unsafe { libc::kill(pid, 0) } == -1 {
            break;
        }
        assert!(
            Instant::now() < dead_deadline,
            "reviewer PID {pid} still alive after cancel"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Workspace lease released; terminal state persisted.
    assert_no_leaked_workspace(&repo);
    let state = store
        .load_state(&run_id)
        .expect("load state")
        .expect("state exists");
    assert_eq!(state.terminal_state, Some(ReviewTerminalState::Cancelled));

    // Second cancel is idempotent: store-level…
    assert!(
        !store.mark_cancelled(&run_id).expect("second cancel"),
        "a terminal run must not transition again"
    );
    // …and through the real CLI.
    let output = run_libra(&["review", "cancel", &run_id], &repo, &[]);
    assert_cli_success(&output, "second `review cancel` must succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("already terminal"),
        "second cancel must report the terminal state: {stdout}"
    );

    // ---- Orphaned-run path: the runner is gone (simulated: a seeded
    // non-terminal run with a live decoy process group and a recorded
    // fake workspace, and no process polling the marker). `review
    // cancel` must NOT merely stamp the state: it must kill the
    // recorded pgid (only under verified start-time provenance — pid
    // reuse must never murder an unrelated process), remove the
    // recorded workspace, and report exactly what it did. ----
    let orphan_id = "orphan-run-decoy";
    store
        .create_run(orphan_id, &["decoy".to_string()], "sha-orphan", "scope")
        .expect("create orphan run");
    // Live decoy sleeper in its own process group — stands in for a
    // reviewer left behind by a crashed runner.
    let mut decoy_cmd = Command::new("/bin/sleep");
    decoy_cmd.arg("300");
    std::os::unix::process::CommandExt::process_group(&mut decoy_cmd, 0);
    let mut decoy = decoy_cmd.spawn().expect("spawn decoy sleeper");
    let decoy_pid = decoy.id();
    // Process provenance, exactly as the runner records it at spawn
    // (`/proc/<pid>/stat` field 22; `None` on non-Linux platforms).
    let decoy_ticks = process_start_ticks(decoy_pid);
    // Fake recorded workspace with the task-worktree naming, INSIDE the
    // repo's own worktrees/tasks base (the only location the orphan
    // cancel will agree to remove).
    let orphan_cleanup_root = repo
        .join(".libra")
        .join("worktrees")
        .join("tasks")
        .join("libra-task-worktree-orphan-decoy");
    let orphan_workspace = orphan_cleanup_root.join("workspace");
    std::fs::create_dir_all(&orphan_workspace).expect("fake orphan workspace");
    store
        .update_state(orphan_id, |state| {
            state.agents[0].pid = Some(decoy_pid);
            state.agents[0].pgid = Some(decoy_pid);
            state.agents[0].proc_start_ticks = decoy_ticks;
            state.workspace_root = Some(orphan_workspace.display().to_string());
        })
        .expect("seed orphan state");

    let output = run_libra(&["review", "cancel", orphan_id, "--json"], &repo, &[]);
    assert_cli_success(&output, "orphaned `review cancel` must succeed");
    let payload: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cancel --json emits JSON");
    assert_eq!(payload["data"]["cancelled"], true);
    assert_eq!(payload["data"]["mode"], "orphaned");
    let released = &payload["data"]["released"];
    assert_eq!(released["had_recorded_processes"], true);
    if decoy_ticks.is_some() {
        // Provenance verifiable (Linux): the live decoy is provably the
        // recorded reviewer incarnation → killed.
        assert_eq!(
            released["killed_pgids"],
            serde_json::json!([decoy_pid]),
            "the provenance-verified decoy pgid must be killed: {payload}"
        );

        // The decoy is our direct child: reap it (bounded) and confirm
        // it was killed — a zombie would still answer kill(pid, 0), so
        // the reaped exit status is the honest liveness proof.
        let reap_deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = decoy.try_wait().expect("try_wait decoy") {
                break status;
            }
            assert!(
                Instant::now() < reap_deadline,
                "decoy sleeper (pid {decoy_pid}) was not killed by the orphaned cancel"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert!(!status.success(), "the decoy must have been SIGKILLed");
    } else {
        // No /proc provenance on this platform: the engine must refuse
        // the kill and report the pgid as unverifiable.
        assert_eq!(
            released["stale_unsafe_pgids"],
            serde_json::json!([decoy_pid]),
            "an unverifiable live pgid must be reported, never killed: {payload}"
        );
        assert!(
            decoy.try_wait().expect("try_wait decoy").is_none(),
            "the unverifiable decoy must still be alive"
        );
        decoy.kill().expect("kill decoy ourselves");
        decoy.wait().expect("reap decoy");
    }
    assert_eq!(released["workspace_action"], "removed");
    assert!(
        !orphan_cleanup_root.exists(),
        "the recorded orphan workspace must be removed"
    );
    let state = store
        .load_state(orphan_id)
        .expect("load orphan state")
        .expect("orphan state exists");
    assert_eq!(state.terminal_state, Some(ReviewTerminalState::Cancelled));
}

// ---------------------------------------------------------------------------
// Stress: cancel during pending output (plan.md:961)
// ---------------------------------------------------------------------------

/// Cancelling while reviewers are mid-flood (megabytes of pending
/// output) neither deadlocks nor drags: the run terminates `cancelled`
/// within a firm bound and the terminal state is persisted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_during_pending_output_is_bounded_and_deadlock_free() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = init_committed_repo(temp.path());
    let store = store_for(&repo);
    let fixtures = temp.path().join("fixtures");
    let flood = stage_fixture(&fixtures, "reviewer-flood.sh");

    // Two concurrent floods of ~130 MiB each (2M lines) — several
    // seconds of sustained output, so a cancel at ~500ms lands squarely
    // mid-flood with both pipes full.
    let request = ReviewRunRequest::new(
        &repo,
        "review the changes",
        "HEAD~1..HEAD",
        "sha-cancel-flood",
        vec![
            fake_reviewer(
                "fake-flood-a",
                &flood,
                &["2000000"],
                Duration::from_secs(120),
            ),
            fake_reviewer(
                "fake-flood-b",
                &flood,
                &["2000000"],
                Duration::from_secs(120),
            ),
        ],
    );
    let cancel = ReviewCancelHandle::new();
    let started = Instant::now();
    let run = tokio::spawn({
        let store = store.clone();
        let cancel = cancel.clone();
        async move { run_review(&store, request, cancel).await }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    cancel.cancel();
    let outcome = tokio::time::timeout(Duration::from_secs(60), run)
        .await
        .expect("cancel during pending output must not deadlock")
        .expect("join run task")
        .expect("run reaches a terminal state");
    assert_eq!(outcome.terminal_state, ReviewTerminalState::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(45),
        "cancel with pending output must stay bounded (took {:?})",
        started.elapsed()
    );
    let state = store
        .load_state(&outcome.run_id)
        .expect("load state")
        .expect("state exists");
    assert_eq!(state.terminal_state, Some(ReviewTerminalState::Cancelled));
    assert_no_leaked_workspace(&repo);
}

// ---------------------------------------------------------------------------
// CLI pagination: unified keyset page envelope (agent.md 强制补强项 #5)
// ---------------------------------------------------------------------------

fn list_page(repo: &Path, extra: &[&str]) -> serde_json::Value {
    let mut args = vec!["review", "list", "--json"];
    args.extend_from_slice(extra);
    let output = run_libra(&args, repo, &[]);
    assert_cli_success(&output, "review list --json");
    serde_json::from_slice(&output.stdout).expect("review list stdout is JSON")
}

fn page_ids(page: &serde_json::Value) -> Vec<String> {
    page["data"]["items"]
        .as_array()
        .expect("items is an array")
        .iter()
        .map(|item| {
            item["run_id"]
                .as_str()
                .expect("run_id is a string")
                .to_string()
        })
        .collect()
}

/// `review list --json --limit --cursor` through the real CLI: the
/// unified `{schema_version, items, next_cursor, has_more}` envelope,
/// a full no-duplicate/no-loss cursor walk over more runs than one page
/// (including a same-timestamp `run_id DESC` tiebreak), and a malformed
/// cursor failing closed as one usage error.
#[test]
fn review_list_cli_paginates_with_keyset_cursor_envelope() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = init_repo(temp.path());
    let store = store_for(&repo);

    // Seed N=7 > limit=3 runs with controlled created_at values
    // (fixed-width RFC 3339 micros — the keyset contract). run-5/run-6
    // share a timestamp to exercise the run_id DESC tiebreak.
    for (run_id, created_at) in [
        ("run-1", "2026-07-01T00:00:00.000000Z"),
        ("run-2", "2026-07-02T00:00:00.000000Z"),
        ("run-3", "2026-07-03T00:00:00.000000Z"),
        ("run-4", "2026-07-04T00:00:00.000000Z"),
        ("run-5", "2026-07-05T00:00:00.000000Z"),
        ("run-6", "2026-07-05T00:00:00.000000Z"),
        ("run-7", "2026-07-06T00:00:00.000000Z"),
    ] {
        let mut state = store
            .create_run(run_id, &["fake-success".to_string()], "sha-seed", "seed")
            .expect("seed run");
        state.created_at = created_at.to_string();
        state.updated_at = created_at.to_string();
        state.terminal_state = Some(ReviewTerminalState::Success);
        store.write_state(&state).expect("write seeded state");
    }
    let expected_order = [
        "run-7", "run-6", "run-5", "run-4", "run-3", "run-2", "run-1",
    ];

    // ---- Page 1: envelope shape + newest-first order. ----
    let page1 = list_page(&repo, &["--limit", "3"]);
    assert_eq!(page1["ok"], true);
    assert_eq!(page1["command"], "review_list");
    let data = page1["data"].as_object().expect("data is an object");
    let mut envelope_keys: Vec<&str> = data.keys().map(String::as_str).collect();
    envelope_keys.sort_unstable();
    assert_eq!(
        envelope_keys,
        ["has_more", "items", "next_cursor", "schema_version"],
        "the page envelope must carry exactly the unified keys"
    );
    assert_eq!(page1["data"]["schema_version"], 1);
    assert_eq!(page1["data"]["has_more"], true);
    assert_eq!(page_ids(&page1), &expected_order[0..3]);
    let cursor1 = page1["data"]["next_cursor"]
        .as_str()
        .expect("next_cursor is an opaque string")
        .to_string();

    // ---- Page 2 via the opaque cursor (round-trip). ----
    let page2 = list_page(&repo, &["--limit", "3", "--cursor", &cursor1]);
    assert_eq!(page2["data"]["has_more"], true);
    assert_eq!(page_ids(&page2), &expected_order[3..6]);
    let cursor2 = page2["data"]["next_cursor"]
        .as_str()
        .expect("second next_cursor")
        .to_string();

    // ---- Final page: remainder, no further cursor. ----
    let page3 = list_page(&repo, &["--limit", "3", "--cursor", &cursor2]);
    assert_eq!(page3["data"]["has_more"], false);
    assert!(page3["data"]["next_cursor"].is_null());
    assert_eq!(page_ids(&page3), &expected_order[6..]);

    // ---- Whole walk: no duplicates, no loss. ----
    let mut walked: Vec<String> = Vec::new();
    walked.extend(page_ids(&page1));
    walked.extend(page_ids(&page2));
    walked.extend(page_ids(&page3));
    assert_eq!(walked, expected_order, "cursor walk must be exact");

    // ---- Default limit (50) returns everything in one page. ----
    let all = list_page(&repo, &[]);
    assert_eq!(all["data"]["has_more"], false);
    assert_eq!(page_ids(&all).len(), 7);

    // ---- Malformed cursor fails closed as one usage error. ----
    let output = run_libra(
        &["review", "list", "--cursor", "not-a-cursor!!"],
        &repo,
        &[],
    );
    assert_eq!(
        output.status.code(),
        Some(129),
        "a malformed cursor is a usage error: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid --cursor"),
        "the refusal must be actionable: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// A0-04: run-level admission / queue enforcement
// ---------------------------------------------------------------------------

/// Seed the shared run-admission directory with `slots` occupied-slot tickets
/// and `queued` waiting tickets, each owned by this (live) test process so the
/// spawned CLI's stale-reclaim keeps them.
fn seed_admission(repo: &Path, slots: usize, queued: usize) {
    let admission = repo
        .join(".libra")
        .join("sessions")
        .join("agent-runs")
        .join(".admission");
    let pid = std::process::id().to_string();
    for (dir, n) in [("slots", slots), ("queue", queued)] {
        let d = admission.join(dir);
        std::fs::create_dir_all(&d).expect("create admission subdir");
        for i in 0..n {
            std::fs::write(d.join(format!("seed-{dir}-{i:03}")), &pid).expect("write ticket");
        }
    }
}

/// A0-04: with the run-level concurrency budget saturated (2 active) AND the
/// wait queue at its cap (10), a fresh `libra review` run is refused
/// fail-closed with the stable `LBR-AGENT-014` code (exit 128) — never
/// silently overrunning the budget — on both the human and JSON surfaces.
#[test]
fn run_level_concurrency_rejects_when_queue_full() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = init_committed_repo(temp.path());
    // 2 active (default max_concurrent_runs) + 10 queued (cap) → full.
    seed_admission(&repo, 2, 10);

    // Human surface.
    let output = run_libra(&["review", "--agent", "codex"], &repo, &[]);
    assert_eq!(
        output.status.code(),
        Some(128),
        "a full queue must refuse fatally (128): stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        stderr.contains("LBR-AGENT-014"),
        "stderr must carry the run-queue-full code: {stderr}"
    );
    assert!(
        stderr.contains("concurrent"),
        "the refusal must explain the concurrency limit: {stderr}"
    );

    // Structured JSON error surface.
    let output = run_libra(
        &["review", "--agent", "codex"],
        &repo,
        &[("LIBRA_ERROR_JSON", "1")],
    );
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let json_start = stderr
        .rfind("\n{")
        .map(|index| index + 1)
        .or_else(|| stderr.find('{'))
        .expect("structured stderr must carry a JSON error report");
    let report: serde_json::Value =
        serde_json::from_str(stderr[json_start..].trim()).expect("JSON error report parses");
    assert_eq!(report["error_code"], "LBR-AGENT-014");
    assert_eq!(report["exit_code"], 128);
}

// ---------------------------------------------------------------------------
// A0-06: manual attach command surface objectizes external files
// ---------------------------------------------------------------------------

/// `libra review attach <run_id> <file>` redacts the external file, writes it
/// to the object store, and records a `manual_attach` manifest entry
/// (`{oid, name, provenance:"manual", size, attached_at}`).
#[test]
fn review_artifacts_objectized() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = init_committed_repo(temp.path());
    let store = store_for(&repo);
    let run = store
        .create_run(
            "attach-run",
            &["codex".to_string()],
            "sha-attach",
            "HEAD~1..HEAD",
        )
        .expect("create run");

    // An external file carrying a secret that must be redacted on attach.
    let attach_file = temp.path().join("external-finding.md");
    std::fs::write(
        &attach_file,
        format!("external finding body\ncredential {FAKE_CREDENTIAL}\n"),
    )
    .expect("write attach file");

    let out = run_libra(
        &[
            "review",
            "attach",
            &run.run_id,
            attach_file.to_str().unwrap(),
        ],
        &repo,
        &[],
    );
    assert!(
        out.status.success(),
        "review attach must succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let manifest = store
        .load_manifest(&run.run_id)
        .expect("load manifest")
        .expect("manifest exists");
    assert_eq!(manifest.manual_attach.len(), 1, "one attachment recorded");
    let entry = &manifest.manual_attach[0];
    assert_eq!(entry["provenance"], "manual");
    assert_eq!(entry["name"], "external-finding.md");
    let oid = entry["oid"].as_str().expect("attachment oid");
    assert!(oid.len() == 40 || oid.len() == 64, "oid: {oid}");

    // The attachment object is readable, preserves content, and is redacted.
    let obj_path = repo
        .join(".libra")
        .join("objects")
        .join(&oid[..2])
        .join(&oid[2..]);
    let raw = std::fs::read(&obj_path).expect("attachment object on disk");
    let mut decoder = flate2::read::ZlibDecoder::new(&raw[..]);
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decoded).expect("zlib decode attachment");
    let header_end = decoded.iter().position(|&b| b == 0).expect("object header");
    let text = String::from_utf8_lossy(&decoded[header_end + 1..]).to_string();
    assert!(
        text.contains("external finding body"),
        "attachment content preserved: {text}"
    );
    assert!(
        !text.contains(FAKE_CREDENTIAL),
        "the credential must be redacted out of the attachment object: {text}"
    );

    // A second attach appends AND redacts/scrubs an untrusted basename: a
    // filename can itself carry a secret, a newline, a tab, or ANSI.
    let leaky_name = format!("leak-{FAKE_CREDENTIAL}-\u{1b}\n\tx.md");
    let leaky_file = temp.path().join(&leaky_name);
    std::fs::write(&leaky_file, "second attachment body\n").expect("write leaky-name file");
    let out = run_libra(
        &[
            "review",
            "attach",
            &run.run_id,
            leaky_file.to_str().unwrap(),
        ],
        &repo,
        &[],
    );
    assert!(
        out.status.success(),
        "second attach: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let manifest = store.load_manifest(&run.run_id).unwrap().unwrap();
    assert_eq!(manifest.manual_attach.len(), 2, "second attach appends");
    let name2 = manifest.manual_attach[1]["name"]
        .as_str()
        .expect("attachment name");
    assert!(
        !name2.contains(FAKE_CREDENTIAL),
        "the basename must be redacted before persist: {name2}"
    );
    assert!(
        !name2.chars().any(|c| c.is_control()),
        "the basename must have every control char (incl \\n/\\t/ESC) stripped: {name2:?}"
    );

    // A failed read of a hostile path must not leak the secret/path either.
    let missing = temp.path().join(format!("missing-{FAKE_CREDENTIAL}.md"));
    let out = run_libra(
        &["review", "attach", &run.run_id, missing.to_str().unwrap()],
        &repo,
        &[],
    );
    assert!(!out.status.success(), "attaching a missing file must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains(FAKE_CREDENTIAL),
        "the read-error must not leak the secret from the path: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// A0-09: findings retention GC removes an expired terminal run
// ---------------------------------------------------------------------------

/// A real finalized review run's manifest carries the retention fields
/// (`terminal_state`, `updated_at`, `findings_oid`); once backdated past the
/// `agent.retention.findings_days` window, `libra agent clean --gc` removes the
/// whole run directory (the objectized blob is content-addressed and kept for a
/// future repo-wide object GC).
#[test]
fn findings_retention_manifest() {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = init_committed_repo(temp.path());
    let store = store_for(&repo);
    store
        .create_run("gc-run", &["codex".to_string()], "sha", "HEAD~1..HEAD")
        .expect("create run");
    store
        .write_findings("gc-run", "review finding body\n")
        .expect("write findings");
    store
        .finalize_run(
            "gc-run",
            ReviewTerminalState::Success,
            &[],
            libra::internal::ai::review::store::RedactionReportSummary::default(),
        )
        .expect("finalize objectizes findings");

    let run_dir = repo.join(".libra/sessions/agent-runs/gc-run");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(run_dir.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["terminal_state"], "success");
    let findings_oid = manifest["findings_oid"]
        .as_str()
        .expect("A0-06 findings_oid")
        .to_string();
    let blob = repo
        .join(".libra/objects")
        .join(&findings_oid[..2])
        .join(&findings_oid[2..]);
    assert!(blob.exists(), "findings blob objectized");

    // Backdate updated_at past the retention window, then GC via the binary.
    let mut backdated = manifest.clone();
    backdated["updated_at"] = serde_json::json!("2000-01-01T00:00:00.000000Z");
    std::fs::write(
        run_dir.join("manifest.json"),
        serde_json::to_vec(&backdated).unwrap(),
    )
    .unwrap();

    let out = run_libra(&["agent", "clean", "--gc"], &repo, &[]);
    assert!(
        out.status.success(),
        "clean --gc: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!run_dir.exists(), "the expired terminal run dir is GC'd");
    // The objectized blob is content-addressed and left for a future repo-wide
    // object GC — per-run retention never deletes it.
    assert!(
        blob.exists(),
        "the objectized findings blob is not deleted here"
    );
}
