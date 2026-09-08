//! `tests/agent_bridge_vcs_test.rs` — the bridge's real VCS wiring
//! (plan-20260818 LB-04 `diff.get`; LB-05 `commit.create` / `review.run` /
//! `checkpoint.restore`).
//!
//! Unlike the other `agent_bridge_*` targets, these drive a **real repository**
//! on disk: the bridge's durable tables stay in an in-memory store (the scope
//! the request is bound to), while diff / commit / restore run against a
//! temporary Libra repository that the process cwd points at. That is exactly
//! the split the production bridge has, so a regression that fabricated a
//! result instead of touching the repository would fail here.
//!
//! Every test is `#[serial]`: `ChangeDirGuard` mutates the process cwd.

use std::{fs, io::Write, path::Path};

use libra::{
    command::{
        add::{self, AddArgs},
        commit::{self, CommitArgs},
    },
    internal::{
        ai::agent_bridge::{
            ingress::{BridgeContext, dispatch as ingress_dispatch},
            methods::dispatch as read_dispatch,
            mutations::dispatch as mutation_dispatch,
            protocol::{BridgeRequest, parse_request_line},
            storage::insert_checkpoint,
        },
        config::ConfigKv,
        db::migration::run_builtin_migrations,
        head::Head,
    },
    utils::{
        output::OutputConfig,
        test::{ChangeDirGuard, setup_with_new_libra_in},
    },
};
use sea_orm::Database;
use serde_json::Value;
use serial_test::serial;

const REPO_ID: &str = "repo-vcs";

/// A bridge context whose durable tables are in memory. The repository the VCS
/// calls reach is the process cwd (parked by `ChangeDirGuard`), never anything
/// the request body names.
async fn bridge_ctx() -> BridgeContext {
    let conn = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory bridge store");
    run_builtin_migrations(&conn)
        .await
        .expect("apply migrations");
    BridgeContext::new(conn, REPO_ID, None)
}

fn request(line: &str) -> BridgeRequest {
    parse_request_line(line).expect("valid request frame")
}

async fn open_session(ctx: &BridgeContext, session_id: &str) {
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"session.open","params":{{"session_id":"{session_id}"}},"id":1}}"#
    );
    ingress_dispatch(ctx, &request(&frame))
        .await
        .expect("session.open");
}

fn write_file(root: &Path, name: &str, body: &str) {
    let mut file = fs::File::create(root.join(name)).expect("create file");
    file.write_all(body.as_bytes()).expect("write file");
}

async fn stage(paths: &[&str]) {
    add::execute(AddArgs {
        pathspec: paths.iter().map(|p| p.to_string()).collect(),
        all: false,
        update: false,
        refresh: false,
        force: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;
}

/// Commit through the ordinary CLI path (test fixture setup, not the code
/// under test).
async fn cli_commit(message: &str) {
    commit::execute_safe(
        CommitArgs {
            message: Some(message.to_string()),
            disable_pre: true,
            no_verify: true,
            ..CommitArgs::default()
        },
        &OutputConfig::default(),
    )
    .await
    .expect("fixture commit");
}

/// Initialize a repository with one committed file.
///
/// Returns the [`ChangeDirGuard`] that parks the process cwd inside it: the
/// fixture's own `add`/`commit` calls need that cwd just as much as the bridge
/// calls under test do, so the guard is created here and handed to the caller
/// to hold for the rest of the test.
async fn repo_with_initial_commit(root: &Path) -> ChangeDirGuard {
    setup_with_new_libra_in(root).await;
    let guard = ChangeDirGuard::new(root);
    ConfigKv::set("user.name", "Bridge VCS Test", false)
        .await
        .expect("set user.name");
    ConfigKv::set("user.email", "bridge-vcs@example.com", false)
        .await
        .expect("set user.email");
    write_file(root, "tracked.txt", "one\n");
    stage(&["tracked.txt"]).await;
    cli_commit("initial commit").await;
    guard
}

/// Pull `result.data` out of a read-method envelope.
fn envelope_data(value: &Value) -> &Value {
    &value["data"]
}

// ---------------------------------------------------------------------------
// LB-04 diff.get
// ---------------------------------------------------------------------------

/// `diff.get` reads the real working-tree diff, carries the patch body, and
/// scopes to validated repository-relative paths.
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn diff_get_reports_real_worktree_changes_and_is_path_scoped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;

    write_file(dir.path(), "tracked.txt", "one\ntwo\n");
    write_file(dir.path(), "second.txt", "second\n");
    stage(&["second.txt"]).await;

    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"diff.get","params":{"mode":"worktree"},"id":1}"#),
    )
    .await
    .expect("diff.get")
    .expect("response");
    let result = response.result.expect("result envelope");
    let data = envelope_data(&result);
    assert_eq!(data["mode"], "worktree");
    let files = data["files"].as_array().expect("files array");
    assert!(
        files.iter().any(|f| f["path"] == "tracked.txt"),
        "the modified tracked file must appear in the worktree diff: {files:?}"
    );
    let tracked = files
        .iter()
        .find(|f| f["path"] == "tracked.txt")
        .expect("tracked.txt entry");
    let patch = tracked["patch"].as_str().expect("patch body");
    assert!(
        patch.contains("+two"),
        "the patch body must be the real diff, got: {patch}"
    );

    // Path scoping: asking for a different file must not return tracked.txt.
    let response = read_dispatch(
        &ctx,
        &request(
            r#"{"jsonrpc":"2.0","method":"diff.get","params":{"mode":"worktree","paths":["second.txt"]},"id":2}"#,
        ),
    )
    .await
    .expect("scoped diff.get")
    .expect("response");
    let result = response.result.expect("result envelope");
    let files = envelope_data(&result)["files"]
        .as_array()
        .expect("files array")
        .clone();
    assert!(
        !files.iter().any(|f| f["path"] == "tracked.txt"),
        "path scoping must exclude files outside the selector: {files:?}"
    );

    // The staged side is a different comparison and reports the newly added file.
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"diff.get","params":{"mode":"staged"},"id":3}"#),
    )
    .await
    .expect("staged diff.get")
    .expect("response");
    let result = response.result.expect("result envelope");
    let data = envelope_data(&result);
    assert_eq!(data["mode"], "staged");
    let files = data["files"].as_array().expect("files array");
    assert!(
        files.iter().any(|f| f["path"] == "second.txt"),
        "the staged diff must report the newly staged file: {files:?}"
    );
}

/// `diff.get` selectors are typed: an unknown mode and an escaping path are
/// both refused with `invalid_params`, so a request can never become an
/// arbitrary revision or pathspec (GC-LB-03).
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn diff_get_rejects_untyped_selectors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;

    for frame in [
        r#"{"jsonrpc":"2.0","method":"diff.get","params":{"mode":"HEAD~3"},"id":1}"#,
        r#"{"jsonrpc":"2.0","method":"diff.get","params":{"paths":["../escape"]},"id":2}"#,
        r#"{"jsonrpc":"2.0","method":"diff.get","params":{"paths":[":(exclude)src"]},"id":3}"#,
        r#"{"jsonrpc":"2.0","method":"diff.get","params":{"mode":"checkpoint"},"id":4}"#,
    ] {
        let err = read_dispatch(&ctx, &request(frame))
            .await
            .expect_err("untyped selector must be refused");
        assert_eq!(
            err.stable_code, "LBR-AGENT-027",
            "frame {frame} must be an invalid-params refusal, got {}",
            err.message
        );
    }
}

// ---------------------------------------------------------------------------
// LB-05 commit.create
// ---------------------------------------------------------------------------

/// `commit.create` creates a real commit from the index, moves HEAD, and a
/// replay of the same `operation_id` reports the ORIGINAL commit instead of
/// creating a second one.
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn commit_create_commits_the_index_and_replays_idempotently() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;

    let before = Head::current_commit().await.expect("initial HEAD");
    write_file(dir.path(), "bridge.txt", "from the bridge\n");
    stage(&["bridge.txt"]).await;

    let frame = r#"{"jsonrpc":"2.0","method":"commit.create","params":{"operation_id":"op-commit","session_id":"s1","message":"feat: bridge commit","approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    let response = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("commit.create")
        .expect("response");
    let result = response.result.expect("result");
    let commit = result["commit"].as_str().expect("commit id").to_string();
    assert_eq!(result["subject"], "feat: bridge commit");
    assert_ne!(commit, before.to_string(), "a new commit must be created");

    let after = Head::current_commit().await.expect("HEAD after commit");
    assert_eq!(
        after.to_string(),
        commit,
        "HEAD must point at the new commit"
    );

    // The association graph is durable and does not live in the commit message.
    assert_eq!(result["provenance"]["operation_id"], "op-commit");
    assert_eq!(result["provenance"]["session_id"], "s1");
    assert!(
        !result["subject"]
            .as_str()
            .expect("subject")
            .contains("operation_id"),
        "bridge metadata must never be spliced into the commit message"
    );

    // Replay: same operation id and params -> the recorded commit, no second
    // commit (the mutation is NOT idempotent, so the replay must short-circuit).
    let response = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("replay")
        .expect("response");
    let replay = response.result.expect("result");
    assert_eq!(replay["commit"], commit.as_str());
    assert_eq!(replay["replayed"], true);
    assert_eq!(
        Head::current_commit().await.expect("HEAD").to_string(),
        commit,
        "a replayed commit.create must not create a second commit"
    );
}

/// A stated-but-stale `expected_head` refuses the commit before it is created
/// (LB-05 AC5 fence).
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn commit_create_refuses_on_head_drift() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;

    let before = Head::current_commit().await.expect("initial HEAD");
    write_file(dir.path(), "drift.txt", "drift\n");
    stage(&["drift.txt"]).await;

    let err = mutation_dispatch(
        &ctx,
        &request(
            r#"{"jsonrpc":"2.0","method":"commit.create","params":{"operation_id":"op-drift","session_id":"s1","message":"feat: drift","expected_head":"0000000000000000000000000000000000000000","approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#,
        ),
    )
    .await
    .expect_err("a stale head fence must refuse the commit");
    assert_eq!(err.stable_code, "LBR-AGENT-038");
    assert_eq!(
        Head::current_commit().await.expect("HEAD").to_string(),
        before.to_string(),
        "a refused commit must not move HEAD"
    );
}

// ---------------------------------------------------------------------------
// LB-05 checkpoint.restore
// ---------------------------------------------------------------------------

/// `checkpoint.restore` restores the working tree to the commit a bridge
/// checkpoint pins, and refuses first on a dirty worktree (nothing is
/// destroyed) — HEAD is never moved either way.
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn checkpoint_restore_honours_the_fence_and_restores_the_worktree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;

    let base = Head::current_commit()
        .await
        .expect("base commit")
        .to_string();

    // A bridge checkpoint pinning the base commit.
    insert_checkpoint(&ctx.conn, "cp-base", "s1", None, Some(&base), 1)
        .await
        .expect("record bridge checkpoint");

    // A second commit moves the file forward.
    write_file(dir.path(), "tracked.txt", "one\ntwo\n");
    stage(&["tracked.txt"]).await;
    cli_commit("second commit").await;
    let head = Head::current_commit().await.expect("head").to_string();

    // Dirty worktree -> the restore is refused before it overwrites anything.
    write_file(dir.path(), "tracked.txt", "one\ntwo\nuncommitted\n");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"checkpoint.restore","params":{{"operation_id":"op-restore","session_id":"s1","checkpoint_id":"cp-base","expected_head":"{head}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#
    );
    let err = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("a dirty worktree must refuse the restore");
    assert_eq!(err.stable_code, "LBR-AGENT-038");
    assert_eq!(
        fs::read_to_string(dir.path().join("tracked.txt")).expect("read"),
        "one\ntwo\nuncommitted\n",
        "a refused restore must not touch the working tree"
    );

    // Clean the worktree and retry with a fresh operation id.
    write_file(dir.path(), "tracked.txt", "one\ntwo\n");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"checkpoint.restore","params":{{"operation_id":"op-restore-2","session_id":"s1","checkpoint_id":"cp-base","expected_head":"{head}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#
    );
    let response = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("checkpoint.restore")
        .expect("response");
    let result = response.result.expect("result");
    assert_eq!(result["target_commit"], base.as_str());
    assert_eq!(result["head_moved"], false);
    assert_eq!(
        fs::read_to_string(dir.path().join("tracked.txt")).expect("read"),
        "one\n",
        "the working tree must be restored to the checkpoint's commit"
    );
    assert_eq!(
        Head::current_commit().await.expect("head").to_string(),
        head,
        "checkpoint.restore must never move HEAD"
    );
}

/// A bridge checkpoint that pins no commit cannot be a restore or diff target:
/// it fails closed rather than restoring "nothing".
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn checkpoint_without_a_commit_cannot_be_restored() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;

    insert_checkpoint(&ctx.conn, "cp-empty", "s1", None, None, 1)
        .await
        .expect("record bridge checkpoint");
    let head = Head::current_commit().await.expect("head").to_string();
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"checkpoint.restore","params":{{"operation_id":"op-empty","session_id":"s1","checkpoint_id":"cp-empty","expected_head":"{head}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#
    );
    let err = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("a checkpoint pinning no commit must fail closed");
    assert_eq!(err.stable_code, "LBR-AGENT-027");
    assert!(
        err.message.contains("pins no commit"),
        "the error must explain why: {}",
        err.message
    );
}

// ---------------------------------------------------------------------------
// LB-05 review.run
// ---------------------------------------------------------------------------

/// `review.run` validates the reviewer roster against the launchable
/// capability matrix before any run directory exists, and the bridge
/// supervises nothing after a refusal (GC-LB-10).
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn review_run_refuses_unlaunchable_reviewers_without_residue() {
    use libra::internal::ai::agent_bridge::vcs::supervisor;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;

    let err = mutation_dispatch(
        &ctx,
        &request(
            r#"{"jsonrpc":"2.0","method":"review.run","params":{"operation_id":"op-review","session_id":"s1","agents":["not-a-reviewer"],"approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#,
        ),
    )
    .await
    .expect_err("an unlaunchable reviewer must be refused");
    assert_eq!(err.stable_code, "LBR-AGENT-027");
    assert_eq!(
        supervisor::live_count(),
        0,
        "a refused review.run must leave no supervised run behind"
    );
    assert!(
        !dir.path().join(".libra/sessions/agent-runs").exists(),
        "a refused review.run must not create a run directory"
    );
}

// ---------------------------------------------------------------------------
// LB-05 publisher-vs-deleter
// ---------------------------------------------------------------------------

/// Kill a spawned child when the test leaves, however it leaves.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `commit.create` publishes under the shared maintenance hold (§C.4.3).
///
/// The bridge is the one writer that reaches the repository IN-PROCESS while
/// living outside the CLI's command-level hold:
/// `command_holds_shared_maintenance_lock` (`src/cli.rs`) excludes the whole
/// `agent` surface because an agent's VCS mutations are supposed to spawn
/// `libra` as a subprocess, and the child takes the hold. LB-05 made
/// `commit.create` call `run_commit` directly, so without an explicit hold a
/// concurrent `gc` / `prune` / `repack -d` could unlink payloads between
/// writing an object and publishing the reference that pins it.
///
/// A deletion phase in ANOTHER process holds the lock exclusively (same
/// process would join its own hold instead of blocking). The
/// `publication-barrier` sentinel is written from inside the acquisition only
/// when a shared take would block, so its existence proves the bridge waited
/// rather than merely being slow — there is no sleep here to be wrong about.
#[test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
#[cfg(unix)]
fn commit_create_waits_for_a_deletion_phase_before_publishing() {
    use std::io::{BufRead, BufReader, Write as _};

    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();

    // Fixture setup runs on its own runtime so the cwd guard is dropped before
    // the bridge subprocess starts: the child gets the repository through
    // `current_dir`, not through this process's cwd.
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        let _guard = repo_with_initial_commit(&root).await;
        write_file(&root, "staged.txt", "to be committed\n");
        stage(&["staged.txt"]).await;
    });

    // A deletion phase, in another process, holding the lock exclusively.
    let lock_path = root.join(".libra").join("maintenance.lock");
    let script = format!(
        "import fcntl, sys, time\n\
         f = open({path:?}, 'a+')\n\
         fcntl.flock(f, fcntl.LOCK_EX)\n\
         sys.stdout.write('locked\\n')\n\
         sys.stdout.flush()\n\
         time.sleep(600)\n",
        path = lock_path.to_string_lossy().to_string()
    );
    let mut deleter = ChildGuard(
        std::process::Command::new("python3")
            .args(["-c", &script])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn the deleter holding the lock"),
    );
    let mut ready = String::new();
    BufReader::new(deleter.0.stdout.take().expect("stdout"))
        .read_line(&mut ready)
        .expect("deleter ready");
    assert_eq!(ready.trim(), "locked");

    // Drive a real bridge session up to `commit.create`.
    let frames = concat!(
        r#"{"jsonrpc":"2.0","method":"initialize","params":{"protocol":{"major":1,"minor":0}},"id":1}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"session.open","params":{"session_id":"s-lock"},"id":2}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"commit.create","params":{"operation_id":"op-lock","session_id":"s-lock","message":"feat: publish under the hold","approval":{"decision":"approved","approver":"reviewer-1"}},"id":3}"#,
        "\n",
    );
    let mut bridge = ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
            .args(["agent", "bridge", "--stdio"])
            .current_dir(&root)
            .env("LIBRA_TEST", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the bridge"),
    );
    bridge
        .0
        .stdin
        .take()
        .expect("bridge stdin")
        .write_all(frames.as_bytes())
        .expect("feed bridge frames");

    let barrier = root.join(".libra").join("publication-barrier");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !barrier.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "commit.create never reached the publication barrier — it published \
             without taking the shared maintenance hold"
        );
        assert!(
            bridge.0.try_wait().expect("poll").is_none(),
            "the bridge exited before reaching the publication barrier"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // The sentinel is written only after the bridge observed that a shared
    // acquisition would block, so reaching here already proves the wait.
    assert!(
        bridge.0.try_wait().expect("poll").is_none(),
        "commit.create must wait for the deletion phase, not publish underneath it"
    );
}
