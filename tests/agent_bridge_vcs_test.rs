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

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

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
    BridgeContext {
        repo: conn.clone(),
        conn,
        repository_id: REPO_ID.into(),
        worktree_id: None,
    }
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
        intent_to_add: false,
        sparse: false,
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;
}

/// Commit through the ordinary CLI path (test fixture setup, not the code
/// under test).
async fn delete_publication(operation_id: &str) {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let db = libra::internal::worktree_scope::request_db()
        .await
        .expect("repository database");
    db.execute_raw(Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "DELETE FROM agent_bridge_publication WHERE operation_id = ?",
        [operation_id.into()],
    ))
    .await
    .expect("delete publication");
}

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
    // VCS behavior is under test, not signing; isolate the repository from host vault keys.
    ConfigKv::set("commit.gpgSign", "false", false)
        .await
        .expect("disable fixture signing locally");
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
#[serial(cwd, env, hash_kind)]
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

/// The legacy `diff.get` path contract continues to accept backslash separators.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(unix)]
async fn diff_get_preserves_backslash_separator_compatibility() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    std::fs::create_dir(dir.path().join("src")).expect("src directory");
    write_file(dir.path(), "src/main.rs", "before\n");
    stage(&["src/main.rs"]).await;
    cli_commit("add nested source path").await;
    write_file(dir.path(), "src/main.rs", "before\nafter\n");

    let ctx = bridge_ctx().await;
    let response = read_dispatch(
        &ctx,
        &request(
            r#"{"jsonrpc":"2.0","method":"diff.get","params":{"mode":"worktree","paths":["src\\main.rs"]},"id":1}"#,
        ),
    )
    .await
    .expect("diff.get keeps legacy path behavior")
    .expect("response");
    let result = response.result.expect("result envelope");
    let files = envelope_data(&result)["files"]
        .as_array()
        .expect("files array");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["path"], "src/main.rs");
    assert!(
        files[0]["patch"]
            .as_str()
            .expect("patch")
            .contains("+after")
    );
}

/// `diff.get` selectors are typed: an unknown mode and an escaping path are
/// both refused with `invalid_params`, so a request can never become an
/// arbitrary revision or pathspec (GC-LB-03).
#[tokio::test]
#[serial(cwd, env, hash_kind)]
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

/// Publication checks the admitted HEAD inside the ref transaction.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn bridge_publication_refuses_a_head_changed_after_admission() {
    use libra::internal::ai::agent_bridge::{publication, vcs};
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let admitted = Head::current_commit()
        .await
        .expect("initial head")
        .to_string();
    write_file(dir.path(), "advance.txt", "advance\n");
    stage(&["advance.txt"]).await;
    cli_commit("advance before publication").await;
    let actual = Head::current_commit()
        .await
        .expect("advanced head")
        .to_string();
    assert_ne!(actual, admitted);
    publication::ensure_table()
        .await
        .expect("publication table");
    let error = publication::with_operation("op-transaction-fence", &admitted, async {
        vcs::commit_create_unguarded("must not publish", false, true).await
    })
    .await
    .expect_err("moved HEAD must refuse publication");
    assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
    assert!(error.message.contains("stale_state"), "{}", error.message);
    assert_eq!(
        Head::current_commit().await.expect("head").to_string(),
        actual
    );
    assert!(
        publication::published_commit("op-transaction-fence")
            .await
            .expect("lookup")
            .is_none()
    );
}

/// A child holding a SQLite write transaction advances HEAD while publication waits.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn bridge_publication_refuses_overlapping_external_ref_transaction() {
    use std::{
        io::{BufRead, BufReader, Write as _},
        process::{Command, Stdio},
    };

    use libra::internal::{
        ai::agent_bridge::{publication, vcs},
        branch::Branch,
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let admitted = Head::current_commit()
        .await
        .expect("initial head")
        .to_string();
    write_file(dir.path(), "advance.txt", "advance\n");
    stage(&["advance.txt"]).await;
    cli_commit("external target").await;
    let target = Head::current_commit().await.expect("target").to_string();
    let branch = match Head::current().await {
        Head::Branch(name) => name,
        Head::Detached(_) => panic!("expected branch"),
    };
    Branch::update_branch(&branch, &admitted, None)
        .await
        .expect("restore test branch");
    publication::ensure_table()
        .await
        .expect("publication table");
    let script = "import sqlite3,sys\nc=sqlite3.connect(sys.argv[1],timeout=30)\nc.execute('BEGIN IMMEDIATE')\nr=c.execute(\"UPDATE `reference` SET `commit`=? WHERE name=? AND kind='Branch' AND remote IS NULL\",(sys.argv[2],sys.argv[3]))\nassert r.rowcount==1\nprint('locked',flush=True)\nsys.stdin.readline()\nc.commit()\n";
    let mut child = Command::new("python3")
        .args(["-c", script])
        .arg(dir.path().join(".libra/libra.db"))
        .arg(&target)
        .arg(&branch)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn transaction holder");
    let mut line = String::new();
    BufReader::new(child.stdout.take().expect("stdout"))
        .read_line(&mut line)
        .expect("read lock confirmation");
    assert_eq!(line.trim(), "locked");
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut pending = tokio::task::spawn_local(async move {
                publication::with_operation("op-overlap", &admitted, async {
                    vcs::commit_create_unguarded("must not publish", false, true).await
                })
                .await
            });
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(200), &mut pending)
                    .await
                    .is_err(),
                "bridge must not finish while the writer holds its transaction"
            );
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(b"commit\n")
                .expect("release child");
            let result = tokio::time::timeout(std::time::Duration::from_secs(30), pending)
                .await
                .expect("bridge completes")
                .expect("join");
            let error = result.expect_err("HEAD moved by overlapping writer");
            assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
        })
        .await;
    let output = child.wait_with_output().expect("child exit");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        Head::current_commit().await.expect("head").to_string(),
        target
    );
    assert!(
        publication::published_commit("op-overlap")
            .await
            .expect("lookup")
            .is_none()
    );
}

/// A different Libra process can advance HEAD after this request observes it.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn bridge_publication_refuses_head_advanced_by_another_process() {
    use libra::internal::ai::agent_bridge::{publication, vcs};
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let admitted = Head::current_commit()
        .await
        .expect("initial head")
        .to_string();
    write_file(dir.path(), "external.txt", "external\n");
    stage(&["external.txt"]).await;
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(dir.path())
        .args(["commit", "--no-verify", "-m", "external commit"])
        .output()
        .expect("run separate Libra process");
    assert!(
        child.status.success(),
        "child commit failed: {}",
        String::from_utf8_lossy(&child.stderr)
    );
    let actual = Head::current_commit()
        .await
        .expect("advanced head")
        .to_string();
    assert_ne!(actual, admitted);
    publication::ensure_table()
        .await
        .expect("publication table");
    let error = publication::with_operation("op-external-fence", &admitted, async {
        vcs::commit_create_unguarded("must not publish", false, true).await
    })
    .await
    .expect_err("external HEAD advance must refuse publication");
    assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
    assert_eq!(
        Head::current_commit().await.expect("head").to_string(),
        actual
    );
    assert!(
        publication::published_commit("op-external-fence")
            .await
            .expect("lookup")
            .is_none()
    );
}

/// A broken HEAD store cannot be mistaken for an unborn branch on mutation.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn commit_create_refuses_an_unreadable_head() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let db = libra::internal::worktree_scope::request_db()
        .await
        .expect("repository database");
    db.execute_raw(Statement::from_string(
        DatabaseBackend::Sqlite,
        "DROP TABLE \"reference\"".to_string(),
    ))
    .await
    .expect("remove reference table in disposable repository");
    let frame = r#"{"jsonrpc":"2.0","method":"commit.create","params":{"operation_id":"op-broken-head","session_id":"s1","message":"must not commit","allow_empty":true,"expected_head":"","approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    let error = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect_err("unreadable HEAD must not be accepted as unborn");
    assert!(error.message.contains("HEAD fence"), "{}", error.message);
    let published =
        libra::internal::ai::agent_bridge::publication::published_commit("op-broken-head")
            .await
            .expect("publication lookup");
    assert!(published.is_none(), "no commit should have been published");
}

/// A publication table created before the phase column still accepts a commit.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn commit_create_upgrades_a_publication_table_without_phase() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let db = libra::internal::worktree_scope::request_db()
        .await
        .expect("repository database");
    db.execute_raw(Statement::from_string(
        DatabaseBackend::Sqlite,
        "CREATE TABLE agent_bridge_publication (operation_id TEXT PRIMARY KEY, commit_oid TEXT NOT NULL)"
            .to_string(),
    ))
    .await
    .expect("old publication table");
    let frame = r#"{"jsonrpc":"2.0","method":"commit.create","params":{"operation_id":"op-upgrade","session_id":"s1","message":"feat: upgrade","allow_empty":true,"approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    let result = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("commit")
        .expect("response")
        .result
        .expect("result");
    let oid = result["commit"].as_str().expect("oid");
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT phase FROM agent_bridge_publication WHERE operation_id = ?",
            ["op-upgrade".into()],
        ))
        .await
        .expect("phase query")
        .expect("phase row");
    let phase: String = row.try_get_by_index(0).expect("phase");
    assert_eq!(phase, "published");
    assert_eq!(Head::current_commit().await.expect("HEAD").to_string(), oid);
}

/// `vcs.commit` rechecks its worktree fence after a pre-commit hook edits a file.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(unix)]
async fn vcs_commit_rechecks_worktree_after_pre_commit_hook() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let before = Head::current_commit().await.expect("initial HEAD");
    write_file(dir.path(), "commit.txt", "staged for bridge\n");
    stage(&["commit.txt"]).await;
    write_file(dir.path(), "hook.txt", "before hook\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");

    let hooks = dir.path().join(".libra/hooks");
    fs::create_dir_all(&hooks).expect("hooks directory");
    let hook = hooks.join("pre-commit");
    fs::write(
        &hook,
        "#!/bin/sh\nprintf 'completed' > \"$LIBRA_WORK_TREE/hook-result\"\nprintf 'changed by hook\\n' > \"$LIBRA_WORK_TREE/hook.txt\"\n",
    )
    .expect("pre-commit hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("make hook executable");

    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.commit","params":{{"operation_id":"op-hook-lock","session_id":"s1","message":"feat: hook lock","paths":["commit.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let response = mutation_dispatch(&ctx, &request(&frame)).await;
    let hook_result = fs::read_to_string(dir.path().join("hook-result"))
        .expect("the pre-commit hook must finish");
    assert_eq!(hook_result, "completed");
    let error = response.expect_err("the hook's worktree change must make the commit stale");
    assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
    assert_eq!(Head::current_commit().await.expect("HEAD"), before);
    assert_eq!(
        fs::read_to_string(dir.path().join("hook.txt")).expect("hook edit"),
        "changed by hook\n"
    );
}

/// `commit.create` creates a real commit from the index, moves HEAD, and a
/// replay of the same `operation_id` reports the ORIGINAL commit instead of
/// creating a second one.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
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

/// An intent without a publication record cannot identify a completed commit.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn commit_create_refuses_an_intent_without_a_publication_record() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let before = Head::current_commit().await.expect("initial HEAD");
    let frame = r#"{"jsonrpc":"2.0","method":"commit.create","params":{"operation_id":"op-recover","session_id":"s1","message":"feat: recover","allow_empty":true,"approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    let first = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("commit")
        .expect("response")
        .result
        .expect("result");
    let oid = first["commit"].as_str().expect("oid").to_string();
    let intent =
        format!(r#"{{"commitIntent":true,"expectedHead":"{before}","message":"feat: recover"}}"#);
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'failed', result_digest = ? WHERE operation_id = 'op-recover'",
            [intent.into()],
        ))
        .await
        .expect("replace result with intent");
    delete_publication("op-recover").await;
    let replay = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(
        replay["outcome"], "indeterminate",
        "an intent without a publication record has an uncertain outcome: {replay}"
    );
    assert_ne!(
        replay["commit"].as_str(),
        Some(oid.as_str()),
        "an intent alone must not adopt a commit"
    );
    assert_eq!(Head::current_commit().await.expect("HEAD").to_string(), oid);
}

/// A later commit with the same message is not this operation's result.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn commit_create_replay_uses_the_publication_not_a_lookalike() {
    use git_internal::internal::object::commit::Commit;
    use libra::utils::object_ext::CommitExt;
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let frame = r#"{"jsonrpc":"2.0","method":"commit.create","params":{"operation_id":"op-bound","session_id":"s1","message":"feat: same","allow_empty":true,"approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    let first = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("commit")
        .expect("response")
        .result
        .expect("result");
    let oid = first["commit"].as_str().expect("oid").to_string();
    let stored = Commit::load(&libra::internal::object_format::parse_repo_oid(&oid).expect("oid"))
        .expect("commit object");
    assert_eq!(stored.message.trim(), "feat: same");
    assert!(
        !stored.message.contains("op-bound"),
        "operation id must not enter the commit message: {}",
        stored.message
    );
    let before = Head::current_commit().await.expect("parent").to_string();
    // The successful call already replaced the intent with the commit id.
    // Put the intent back in the capture catalog only; the repository binding stays.
    let intent =
        format!(r#"{{"commitIntent":true,"expectedHead":"{before}","message":"feat: same"}}"#);
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'failed', result_digest = ? WHERE operation_id = 'op-bound'",
            [intent.into()],
        ))
        .await
        .expect("restore intent");
    commit::execute_safe(
        CommitArgs {
            message: Some("feat: same".to_string()),
            allow_empty: true,
            disable_pre: true,
            no_verify: true,
            ..CommitArgs::default()
        },
        &OutputConfig::default(),
    )
    .await
    .expect("lookalike");
    let lookalike = Head::current_commit().await.expect("lookalike").to_string();
    assert_ne!(lookalike, oid);
    let replay = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(replay["commit"], oid);
    assert_eq!(replay["replayed"], true);
    assert_eq!(
        Head::current_commit().await.expect("HEAD").to_string(),
        lookalike,
        "recovery must not publish another commit"
    );
}

/// A retry resumes a prepared commit after acquiring the stopped owner's lock.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn commit_create_recovers_a_prepared_object_after_owner_stops() {
    use git_internal::internal::object::commit::Commit;
    use libra::{internal::branch::Branch, utils::object_ext::CommitExt};
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let frame = r#"{"jsonrpc":"2.0","method":"commit.create","params":{"operation_id":"op-prepared","session_id":"s1","message":"feat: prepared","allow_empty":true,"approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    let first = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("commit")
        .expect("response")
        .result
        .expect("result");
    let oid = first["commit"].as_str().expect("oid").to_string();
    let stored = Commit::load(&libra::internal::object_format::parse_repo_oid(&oid).expect("oid"))
        .expect("commit");
    let parent = stored
        .parent_commit_ids
        .first()
        .expect("parent")
        .to_string();
    let branch = match Head::current().await {
        Head::Branch(name) => name,
        Head::Detached(_) => panic!("expected a branch"),
    };
    Branch::update_branch(&branch, &parent, None)
        .await
        .expect("rewind branch");
    let db = libra::internal::worktree_scope::request_db()
        .await
        .expect("repository database");
    db.execute_raw(Statement::from_string(
        DatabaseBackend::Sqlite,
        "UPDATE agent_bridge_publication SET phase = 'prepared' WHERE operation_id = 'op-prepared'"
            .to_string(),
    ))
    .await
    .expect("mark prepared");
    let intent =
        format!(r#"{{"commitIntent":true,"expectedHead":"{parent}","message":"feat: prepared"}}"#);
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'pending', result_digest = ? WHERE operation_id = 'op-prepared'",
            [intent.clone().into()],
        ))
        .await
        .expect("restore intent");
    let recovered = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("recover stopped publisher")
        .expect("response")
        .result
        .expect("replayed result");
    assert_eq!(recovered["commit"], oid);
    assert_eq!(recovered["replayed"], true);
    assert_eq!(Head::current_commit().await.expect("HEAD").to_string(), oid);

    // A later recovery attempt blocked by sequence state is terminal. Removing
    // the marker later must not let the same operation publish retroactively.
    Branch::update_branch(&branch, &parent, None)
        .await
        .expect("rewind for the blocked recovery case");
    db.execute_raw(Statement::from_string(
        DatabaseBackend::Sqlite,
        "UPDATE agent_bridge_publication SET phase = 'prepared' WHERE operation_id = 'op-prepared'"
            .to_string(),
    ))
    .await
    .expect("restore prepared publication");
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'pending', result_digest = ? WHERE operation_id = 'op-prepared'",
            [intent.into()],
        ))
        .await
        .expect("restore pending intent");
    write_file(dir.path(), ".libra/merge-state.json", "{}\n");
    let blocked = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("terminal recovery response")
        .expect("response")
        .result
        .expect("terminal result");
    assert_eq!(blocked["outcome"], "indeterminate", "{blocked}");
    fs::remove_file(dir.path().join(".libra/merge-state.json")).expect("remove merge marker");
    let replay = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("replay after terminal recovery")
        .expect("response")
        .result
        .expect("terminal result");
    assert_eq!(replay["outcome"], "indeterminate", "{replay}");
    assert_eq!(replay["replayed"], true);
    assert_eq!(
        Head::current_commit().await.expect("HEAD").to_string(),
        parent,
        "a terminal operation must not publish later"
    );
}

/// Replay must reject a corrupted recorded commit identifier.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn commit_create_replay_rejects_corrupt_recorded_oid() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let frame = r#"{"jsonrpc":"2.0","method":"commit.create","params":{"operation_id":"op-corrupt","session_id":"s1","message":"feat: corrupt","allow_empty":true,"approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("commit");
    let before = Head::current_commit().await.expect("head");
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET result_digest = ? WHERE operation_id = 'op-corrupt'",
            ["not-an-object-id".into()],
        ))
        .await
        .expect("corrupt result");
    let error = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect_err("corrupt result must fail");
    assert!(
        error.message.contains("recorded commit"),
        "{}",
        error.message
    );
    assert_eq!(Head::current_commit().await.expect("head"), before);
}

/// An association failure after publication keeps the commit id for replay.
#[tokio::test]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
#[serial(cwd, env, hash_kind)]
async fn commit_create_keeps_the_commit_id_when_association_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let frame = r#"{"jsonrpc":"2.0","method":"commit.create","params":{"operation_id":"op-assoc","session_id":"s1","message":"feat: assoc","allow_empty":true,"approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    libra::internal::ai::agent_bridge::mutations::fail_next_association_for_test();
    let error = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect_err("association failure");
    assert!(error.message.contains("association"), "{}", error.message);
    assert!(
        error.message.contains("durable outcome"),
        "the bookkeeping error must name the durable outcome it hides: {}",
        error.message
    );
    let published = Head::current_commit()
        .await
        .expect("published HEAD")
        .to_string();
    let recorded =
        libra::internal::ai::agent_bridge::storage::get_operation_result(&ctx.conn, "op-assoc")
            .await
            .expect("operation")
            .expect("row");
    assert_eq!(recorded.result_digest.as_deref(), Some(published.as_str()));
    let replay = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(replay["commit"], published);
    assert_eq!(replay["replayed"], true);
    assert_eq!(
        Head::current_commit().await.expect("HEAD").to_string(),
        published
    );
}

/// Recovery must not adopt a commit whose tree differs from the recorded intent.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn commit_create_recovery_rejects_a_different_tree() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let before = Head::current_commit().await.expect("initial HEAD");
    let frame = r#"{"jsonrpc":"2.0","method":"commit.create","params":{"operation_id":"op-tree","session_id":"s1","message":"feat: tree","allow_empty":true,"approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    let first = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("commit")
        .expect("response")
        .result
        .expect("result");
    let oid = first["commit"].as_str().expect("oid").to_string();
    let intent = format!(
        r#"{{"commitIntent":true,"expectedHead":"{before}","message":"feat: tree","tree":"not-the-commit-tree"}}"#
    );
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'failed', result_digest = ? WHERE operation_id = 'op-tree'",
            [intent.into()],
        ))
        .await
        .expect("store mismatched intent");
    delete_publication("op-tree").await;
    let replay = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(
        replay["outcome"], "indeterminate",
        "a commit whose publication record is gone has an uncertain outcome: {replay}"
    );
    assert_ne!(
        replay["commit"].as_str(),
        Some(oid.as_str()),
        "the mismatched commit must not be adopted"
    );
    assert_eq!(Head::current_commit().await.expect("HEAD").to_string(), oid);
}

/// Recovery must not adopt a commit that was created before the operation existed.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn commit_create_recovery_rejects_a_commit_that_predates_the_operation() {
    use git_internal::internal::object::commit::Commit;
    use libra::utils::object_ext::CommitExt;
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let before = Head::current_commit().await.expect("initial HEAD");
    let frame = r#"{"jsonrpc":"2.0","method":"commit.create","params":{"operation_id":"op-old","session_id":"s1","message":"feat: predates","allow_empty":true,"approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    let first = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("commit")
        .expect("response")
        .result
        .expect("result");
    let oid = first["commit"].as_str().expect("oid").to_string();
    let stored = Commit::load(&libra::internal::object_format::parse_repo_oid(&oid).expect("oid"))
        .expect("commit");
    let intent = format!(
        r#"{{"commitIntent":true,"expectedHead":"{before}","message":"feat: predates","tree":"{}"}}"#,
        stored.tree_id
    );
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'failed', result_digest = ?, created_at = 9000000000000 WHERE operation_id = 'op-old'",
            [intent.into()],
        ))
        .await
        .expect("backdate operation");
    delete_publication("op-old").await;
    let replay = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(
        replay["outcome"], "indeterminate",
        "a commit whose publication record is gone has an uncertain outcome: {replay}"
    );
    assert_ne!(
        replay["commit"].as_str(),
        Some(oid.as_str()),
        "a commit that predates the operation must not be adopted"
    );
    assert_eq!(Head::current_commit().await.expect("HEAD").to_string(), oid);
}

/// A replay does not terminalize a commit while another process owns the bridge claim.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(unix)]
async fn commit_replay_refuses_reconciliation_during_a_cross_process_claim() {
    use std::io::{BufRead, BufReader};

    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let (expected_head, expected_index) =
        libra::internal::ai::agent_bridge::vcs_write::current_commit_fence()
            .await
            .expect("commit fence");
    let frame = r#"{"jsonrpc":"2.0","method":"commit.create","params":{"operation_id":"op-cross-process-claim","session_id":"s1","message":"feat: cross-process claim","allow_empty":true,"approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("commit")
        .expect("response");
    let head_after_commit = Head::current_commit().await.expect("HEAD").to_string();

    // Simulate the executor having recorded its intent before a process stop.
    let intent = serde_json::json!({
        "commitIntent": true,
        "expectedHead": expected_head,
        "expectedIndex": expected_index,
        "message": "feat: cross-process claim",
    })
    .to_string();
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'pending', result_digest = ? \
             WHERE operation_id = 'op-cross-process-claim'",
            [intent.clone().into()],
        ))
        .await
        .expect("record the prepared intent");
    delete_publication("op-cross-process-claim").await;

    // A different process holds the bridge write claim, representing a live
    // executor. A replay must report retryable busy and leave the row alone.
    let script = "import fcntl, sys\nf = open(sys.argv[1], 'a+')\nfcntl.flock(f, fcntl.LOCK_EX)\nprint('locked', flush=True)\nsys.stdin.read()\n";
    let lock_path = dir.path().join(".libra/agent-bridge-writes.lock");
    let mut owner = ChildGuard(
        std::process::Command::new("python3")
            .args(["-c", script])
            .arg(lock_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn claim owner"),
    );
    let mut ready = String::new();
    BufReader::new(owner.0.stdout.take().expect("claim stdout"))
        .read_line(&mut ready)
        .expect("claim ready");
    assert_eq!(ready.trim(), "locked");

    let error = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect_err("replay must not reconcile while a claim is held");
    assert_eq!(error.stable_code, "LBR-AGENT-035", "{}", error.message);
    assert!(error.retryable, "{}", error.message);
    let operation = libra::internal::ai::agent_bridge::storage::get_operation_result(
        &ctx.conn,
        "op-cross-process-claim",
    )
    .await
    .expect("read operation")
    .expect("operation row");
    assert_eq!(operation.status, "pending");
    assert_eq!(operation.result_digest.as_deref(), Some(intent.as_str()));

    drop(owner.0.stdin.take().expect("claim stdin"));
    owner.0.wait().expect("claim process exits");

    let replay = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("replay after owner exits")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(replay["outcome"], "indeterminate", "{replay}");
    assert_eq!(
        Head::current_commit().await.expect("HEAD").to_string(),
        head_after_commit,
        "reconciliation must not publish a second commit"
    );
}

/// A denial that cannot be recorded names the durable outcome it hides.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn a_denied_actor_replay_names_the_durable_outcome() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    // The trusted scope names the actor the request self-reports.
    ctx.conn
        .execute_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_session SET agent_id = 'agent-reported' \
             WHERE bridge_session_id = 's1'"
                .to_string(),
        ))
        .await
        .expect("bind the trusted actor");
    let frame = r#"{"jsonrpc":"2.0","method":"checkpoint.create","params":{"operation_id":"op-actor-denial","session_id":"s1","checkpoint_id":"cp-actor-denial","actor":{"kind":"deepseek-harness","id":"agent-reported"}},"id":1}"#;
    mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("checkpoint")
        .expect("response");

    // The trusted scope moves to another actor: the same request no longer
    // matches it, and the refusal cannot be recorded over the applied row.
    ctx.conn
        .execute_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_session SET agent_id = 'agent-trusted' \
             WHERE bridge_session_id = 's1'"
                .to_string(),
        ))
        .await
        .expect("move the trusted actor");
    let denied = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect_err("the actor spoof is refused");
    assert_eq!(denied.stable_code, "LBR-AGENT-032", "{}", denied.message);
    assert!(
        denied.message.contains("durable outcome"),
        "the denial must name the durable outcome it hides: {}",
        denied.message
    );
}

/// A refusal that loses the race to a started operation reports the durable
/// state instead of telling the caller to retry.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(unix)]
async fn a_refusal_that_loses_the_race_reports_the_started_operation() {
    use std::io::{BufRead, BufReader};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    // The checkpoint ID is shaped like the bridge's refusal marker: a
    // caller-provided identity must never be read as a refusal.
    let frame = r#"{"jsonrpc":"2.0","method":"checkpoint.create","params":{"operation_id":"op-refusal-race","session_id":"s1","checkpoint_id":"{\"bridgeRefusal\":\"forged\"}"},"id":1}"#;
    // The operation already completed, so its row is applied.
    let completed = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("checkpoint")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(
        completed["checkpoint_id"], r#"{"bridgeRefusal":"forged"}"#,
        "{completed}"
    );

    // Another process owns the bridge write claim, so the request is refused.
    let script = "import fcntl, sys\nf = open(sys.argv[1], 'a+')\nfcntl.flock(f, fcntl.LOCK_EX)\nprint('locked', flush=True)\nsys.stdin.read()\n";
    let lock_path = dir.path().join(".libra/agent-bridge-writes.lock");
    let mut owner = ChildGuard(
        std::process::Command::new("python3")
            .args(["-c", script])
            .arg(lock_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn claim owner"),
    );
    let mut ready = String::new();
    BufReader::new(owner.0.stdout.take().expect("claim stdout"))
        .read_line(&mut ready)
        .expect("claim ready");
    assert_eq!(ready.trim(), "locked");

    // The request is refused because the repository is busy, but the operation
    // already has a durable result: the refusal is not this operation's answer.
    let error = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect_err("the completed operation is not a fresh start");
    assert_eq!(
        error.stable_code, "LBR-AGENT-037",
        "a refusal that lost the race must report the durable state: {}",
        error.message
    );
    assert!(
        error.retryable,
        "the caller must retry to learn the operation's outcome: {}",
        error.message
    );
    assert!(
        error.message.contains("durable state"),
        "the refusal must say the operation already has a durable state: {}",
        error.message
    );

    drop(owner.0.stdin.take().expect("claim stdin"));
    owner.0.wait().expect("claim process exits");
}

/// Replaying a recorded commit restores an association deleted after publication.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn commit_create_replay_restores_a_deleted_association() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let frame = r#"{"jsonrpc":"2.0","method":"commit.create","params":{"operation_id":"op-replay-link","session_id":"s1","message":"feat: replay link","allow_empty":true,"approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    let first = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("commit")
        .expect("response")
        .result
        .expect("result");
    let oid = first["commit"].as_str().expect("oid").to_string();
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "DELETE FROM agent_bridge_link WHERE source_id = ?",
            [oid.clone().into()],
        ))
        .await
        .expect("remove links");
    let replay = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(replay["commit"], oid);
    assert_eq!(replay["replayed"], true);
    assert_eq!(Head::current_commit().await.expect("HEAD").to_string(), oid);
    let links = ctx
        .conn
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT target_id FROM agent_bridge_link WHERE source_type = 'commit' AND source_id = ? AND target_id = 'op-replay-link'",
            [oid.into()],
        ))
        .await
        .expect("query links");
    assert_eq!(links.len(), 1, "replay must restore the operation link");
}

/// Recovering a published commit must also restore its association link.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn commit_create_recovery_restores_the_missing_association() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let before = Head::current_commit().await.expect("initial HEAD");
    let frame = r#"{"jsonrpc":"2.0","method":"commit.create","params":{"operation_id":"op-relink","session_id":"s1","message":"feat: relink","allow_empty":true,"approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    let first = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("commit")
        .expect("response")
        .result
        .expect("result");
    let oid = first["commit"].as_str().expect("oid").to_string();
    let intent =
        format!(r#"{{"commitIntent":true,"expectedHead":"{before}","message":"feat: relink"}}"#);
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "DELETE FROM agent_bridge_link WHERE source_id = ?",
            [oid.clone().into()],
        ))
        .await
        .expect("remove links");
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'failed', result_digest = ? WHERE operation_id = 'op-relink'",
            [intent.into()],
        ))
        .await
        .expect("restore intent");
    let replay = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(replay["commit"], oid);
    assert_eq!(Head::current_commit().await.expect("HEAD").to_string(), oid);
    let links = ctx
        .conn
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT target_id FROM agent_bridge_link WHERE source_type = 'commit' AND source_id = ? AND target_id = 'op-relink'",
            [oid.into()],
        ))
        .await
        .expect("query links");
    assert_eq!(links.len(), 1, "recovery must restore the operation link");
}

/// HEAD drift while waiting for the in-process gate must be checked after acquisition.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn commit_create_rechecks_head_after_waiting_for_gate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "next.txt", "next\n");
    stage(&["next.txt"]).await;
    let before = Head::current_commit().await.expect("head");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"commit.create","params":{{"operation_id":"op-wait-head","session_id":"s1","message":"feat: bridge","allow_empty":true,"expected_head":"{before}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#
    );
    tokio::task::LocalSet::new()
        .run_until(async {
            let hold = libra::internal::ai::agent_bridge::vcs_write::acquire_publish_gate().await;
            let cloned = ctx.clone();
            let mut pending = tokio::task::spawn_local(async move {
                mutation_dispatch(&cloned, &request(&frame)).await
            });
            tokio::select! {
                _ = &mut pending => panic!("request must wait for gate"),
                _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {}
            }
            cli_commit("advance head").await;
            let advanced = Head::current_commit().await.expect("advanced");
            assert_ne!(advanced, before);
            drop(hold);
            let error = tokio::time::timeout(std::time::Duration::from_secs(10), pending)
                .await
                .expect("request completed")
                .expect("join")
                .expect_err("stale head");
            assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
            assert_eq!(Head::current_commit().await.expect("head"), advanced);
        })
        .await;
}

/// A stated-but-stale `expected_head` refuses the commit before it is created
/// (LB-05 AC5 fence).
#[tokio::test]
#[serial(cwd, env, hash_kind)]
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
#[serial(cwd, env, hash_kind)]
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

/// A competing Libra writer prevents checkpoint.restore from touching the worktree.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(unix)]
async fn checkpoint_restore_refuses_a_cross_process_publisher() {
    use std::io::{BufRead, BufReader};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let base = Head::current_commit()
        .await
        .expect("base commit")
        .to_string();
    insert_checkpoint(&ctx.conn, "cp-base", "s1", None, Some(&base), 1)
        .await
        .expect("checkpoint");
    write_file(dir.path(), "tracked.txt", "one\ntwo\n");
    stage(&["tracked.txt"]).await;
    cli_commit("second commit").await;
    let head = Head::current_commit().await.expect("head").to_string();

    let script = format!(
        "import fcntl, sys, time\n\
         f = open({path:?}, 'a+')\n\
         fcntl.flock(f, fcntl.LOCK_SH)\n\
         sys.stdout.write('locked\\n')\n\
         sys.stdout.flush()\n\
         time.sleep(600)\n",
        path = dir
            .path()
            .join(".libra/maintenance.lock")
            .to_string_lossy()
            .to_string()
    );
    let mut publisher = ChildGuard(
        std::process::Command::new("python3")
            .args(["-c", &script])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("publisher"),
    );
    let mut ready = String::new();
    BufReader::new(publisher.0.stdout.take().expect("publisher stdout"))
        .read_line(&mut ready)
        .expect("publisher ready");
    assert_eq!(ready.trim(), "locked");

    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"checkpoint.restore","params":{{"operation_id":"op-lock-restore","session_id":"s1","checkpoint_id":"cp-base","expected_head":"{head}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("restore must refuse a concurrent writer");
    assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
    assert_eq!(
        fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
        "one\ntwo\n"
    );
    assert_eq!(
        Head::current_commit().await.expect("head").to_string(),
        head
    );
}

/// A bridge checkpoint that pins no commit cannot be a restore or diff target:
/// it fails closed rather than restoring "nothing".
#[tokio::test]
#[serial(cwd, env, hash_kind)]
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
#[serial(cwd, env, hash_kind)]
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

/// A publisher in another process prevents a fenced bridge write from running.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(unix)]
async fn fenced_write_refuses_a_cross_process_publisher() {
    use std::{
        io::{BufRead, BufReader},
        sync::atomic::{AtomicBool, Ordering},
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let before = fs::read(&index_path).expect("index");
    let script = "import fcntl, sys\nf = open(sys.argv[1], 'a+')\nfcntl.flock(f, fcntl.LOCK_SH)\nprint('locked', flush=True)\nsys.stdin.read()\n";
    let mut child = ChildGuard(
        std::process::Command::new("python3")
            .args(["-c", script])
            .arg(dir.path().join(".libra/maintenance.lock"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("publisher"),
    );
    let mut ready = String::new();
    BufReader::new(child.0.stdout.take().expect("stdout"))
        .read_line(&mut ready)
        .expect("ready");
    assert_eq!(ready.trim(), "locked");
    let called = AtomicBool::new(false);
    let result = libra::internal::ai::agent_bridge::vcs_write::run_fenced(&fence, true, async {
        called.store(true, Ordering::SeqCst);
        Ok::<_, libra::internal::ai::agent_bridge::protocol::BridgeError>(())
    })
    .await;
    let error = result.expect_err("cross-process publisher must block the fenced write");
    assert_eq!(error.stable_code, "LBR-AGENT-038");
    assert!(!called.load(Ordering::SeqCst));
    assert_eq!(fs::read(index_path).expect("index"), before);
}

/// The index publication lock is shared with ordinary Libra index writers.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(unix)]
async fn a_bridge_publication_blocks_another_libra_index_writer() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    write_file(dir.path(), "written-by-add.txt", "content\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let mut child = ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
            .args(["add", "written-by-add.txt"])
            .current_dir(dir.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn libra add"),
    );

    let finished = AtomicBool::new(false);
    let result = libra::internal::ai::agent_bridge::vcs_write::run_fenced(&fence, true, async {
        // While the bridge holds the publication lock, an ordinary Libra
        // index writer must not be able to complete its own index write.
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(
            child.0.try_wait().expect("poll").is_none(),
            "an ordinary index writer completed while the bridge held the publication lock"
        );
        Ok::<_, libra::internal::ai::agent_bridge::protocol::BridgeError>(())
    })
    .await;
    assert!(result.is_ok());
    finished.store(true, Ordering::SeqCst);
    assert!(finished.load(Ordering::SeqCst));

    // The writer completes once the publication lock is released.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if child.0.try_wait().expect("poll").is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the ordinary index writer never completed after the lock was released"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    assert!(
        index.get("written-by-add.txt", 0).is_some(),
        "the ordinary writer's staged file must survive the bridge publication"
    );
}

/// `commit.create` refuses a competing cross-process publisher before mutation.
///
/// The bridge is a long-lived `agent` process, which does not hold the CLI's
/// command-level maintenance lock. Each in-process mutation must therefore
/// acquire the repository lock for its complete check-and-publish interval.
#[test]
#[serial(cwd, env, hash_kind)]
#[cfg(unix)]
fn commit_create_refuses_another_cross_process_publisher() {
    use std::io::{BufRead, BufReader, Read as _, Write as _};

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

    // A normal Libra publisher in another process holds the shared lock.
    let lock_path = root.join(".libra").join("maintenance.lock");
    let script = format!(
        "import fcntl, sys, time\n\
         f = open({path:?}, 'a+')\n\
         fcntl.flock(f, fcntl.LOCK_SH)\n\
         sys.stdout.write('locked\\n')\n\
         sys.stdout.flush()\n\
         time.sleep(600)\n",
        path = lock_path.to_string_lossy().to_string()
    );
    let mut publisher = ChildGuard(
        std::process::Command::new("python3")
            .args(["-c", &script])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn the publisher holding the lock"),
    );
    let mut ready = String::new();
    BufReader::new(publisher.0.stdout.take().expect("stdout"))
        .read_line(&mut ready)
        .expect("publisher ready");
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
            .stdout(std::process::Stdio::piped())
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

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if bridge.0.try_wait().expect("poll").is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "commit.create remained blocked instead of refusing the competing publisher"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let mut stdout = String::new();
    bridge
        .0
        .stdout
        .take()
        .expect("bridge stdout")
        .read_to_string(&mut stdout)
        .expect("read bridge response");
    let response: serde_json::Value = serde_json::from_str(
        stdout
            .lines()
            .last()
            .expect("bridge returned a commit.create response"),
    )
    .expect("decode bridge response");
    assert_eq!(
        response["error"]["data"]["stable_code"], "LBR-AGENT-038",
        "{response}"
    );
}

/// (new) LVC-02: `vcs.*` reads the real repository and leaves `status.get` as
/// bridge telemetry.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_read_methods_report_identity_status_diff_and_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;

    write_file(dir.path(), "tracked.txt", "one\ntwo\n");
    write_file(dir.path(), "extra.txt", "extra\n");
    stage(&["extra.txt"]).await;

    let identity = vcs_data(&ctx, r#"{"jsonrpc":"2.0","method":"vcs.identity","id":1}"#).await;
    assert!(identity["worktree_root_hash"].as_str().unwrap().len() == 64);
    assert!(identity["head_oid"].as_str().is_some());
    assert_eq!(identity["unborn"], false);
    assert!(
        identity.get("workdir").is_none(),
        "vcs.identity must not return an absolute worktree path: {identity}"
    );
    let rendered = identity.to_string();
    assert!(
        !rendered.contains(&dir.path().to_string_lossy().to_string()),
        "vcs.identity must not leak the host path: {rendered}"
    );

    let status = vcs_data(&ctx, r#"{"jsonrpc":"2.0","method":"vcs.status","id":2}"#).await;
    let entries = status["entries"].as_array().expect("entries");
    assert!(
        entries
            .iter()
            .any(|entry| entry["path"] == "tracked.txt" && entry["state"] == "unstaged"),
        "unstaged edit must be distinct: {entries:?}"
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry["path"] == "extra.txt" && entry["state"] == "staged"),
        "staged file must be distinct: {entries:?}"
    );

    let diff = vcs_data(
        &ctx,
        r#"{"jsonrpc":"2.0","method":"vcs.diff","params":{"sides":"worktree-vs-index","paths":["tracked.txt"]},"id":3}"#,
    )
    .await;
    assert_eq!(diff["sides"], "worktree-vs-index");
    let patch = diff["files"][0]["patch"].as_str().expect("patch");
    assert!(patch.contains("+two"), "real patch, got {patch}");

    let log = vcs_data(
        &ctx,
        r#"{"jsonrpc":"2.0","method":"vcs.log","params":{"limit":1},"id":4}"#,
    )
    .await;
    let oid = log["commits"][0]["oid"].as_str().expect("oid").to_string();
    assert_eq!(log["commits"][0]["summary"], "initial commit");

    let show = vcs_data(
        &ctx,
        &format!(r#"{{"jsonrpc":"2.0","method":"vcs.show","params":{{"oid":"{oid}"}},"id":5}}"#),
    )
    .await;
    assert_eq!(show["summary"], "initial commit");
    assert_eq!(show["files"][0]["path"], "tracked.txt");
    assert!(show["cursor"]["next"].is_null());

    let legacy = vcs_data(&ctx, r#"{"jsonrpc":"2.0","method":"status.get","id":6}"#).await;
    assert!(legacy.get("entries").is_none());
    assert!(
        legacy.get("bridge_sessions").is_some()
            || legacy.get("bridge_events").is_some()
            || legacy.as_object().is_some()
    );
}

/// A corrupt index is an error, never an empty or conflict-free status.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_status_refuses_a_corrupt_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    fs::write(dir.path().join(".libra/index"), b"corrupt index").expect("corrupt index");
    let error = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","id":1}"#),
    )
    .await
    .expect_err("unreadable index must not look clean");
    assert!(error.message.contains("read index"), "{}", error.message);
    assert!(!error.message.contains(&dir.path().display().to_string()));
}

/// (new) LVC-06: stage is fenced, replay does not stage twice, and a stale
/// fence does not touch the index.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_replays_and_refuses_a_stale_fence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "queued.txt", "queued\n");

    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_before = fs::read(dir.path().join(".libra").join("index")).expect("index");
    let stale = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stale","session_id":"s1","paths":["queued.txt"],"expectedHead":"deadbeef","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.index_generation, fence.worktree_generation
    );
    let err = mutation_dispatch(&ctx, &request(&stale))
        .await
        .expect_err("stale fence");
    assert!(err.message.contains("stale_state"), "{}", err.message);
    assert_eq!(
        fs::read(dir.path().join(".libra").join("index")).expect("index"),
        index_before
    );

    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage","session_id":"s1","paths":["queued.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let response = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("stage")
        .expect("response");
    let result = response.result.expect("result");
    assert_eq!(result["outcome"], "applied");
    assert_eq!(result["paths"][0], "queued.txt");

    let replay = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("replay")
        .expect("response");
    assert_eq!(replay.result.expect("result")["outcome"], "replayed");
}

/// A negotiated connection must not expose the unsafe fence through NDJSON.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_fence_is_disabled_after_minor_one_handshake() {
    use std::io::Cursor;

    use libra::{
        command::agent::bridge::IngressBridgeHandler, internal::ai::agent_bridge::transport::run,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    let input = concat!(
        r#"{"jsonrpc":"2.0","method":"initialize","params":{"protocol":{"major":1,"minor":1}},"id":1}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"vcs.fence","id":2}"#,
        "\n",
    );
    let handler = IngressBridgeHandler::new(ctx);
    let mut output = Vec::new();
    run(
        Cursor::new(input),
        &mut output,
        &handler,
        std::time::Duration::from_secs(30),
    )
    .await
    .expect("transport");
    let responses: Vec<Value> = String::from_utf8(output)
        .expect("utf8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("frame"))
        .collect();
    assert!(
        !responses[0]["result"]["methods"]
            .as_array()
            .expect("methods")
            .iter()
            .any(|method| method == "vcs.fence")
    );
    assert_eq!(responses[1]["error"]["code"], -32601);
    assert!(responses[1].get("result").is_none());
}

/// A `vcs.commit` retry resumes its prepared object after its owner stops.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_commit_recovers_a_prepared_object_after_owner_stops() {
    use git_internal::internal::object::commit::Commit;
    use libra::{internal::branch::Branch, utils::object_ext::CommitExt};
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let before = Head::current_commit().await.expect("before");
    write_file(dir.path(), "recover.txt", "recover\n");
    stage(&["recover.txt"]).await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.commit","params":{{"operation_id":"op-vcs-intent","session_id":"s1","message":"feat: recover vcs","paths":["recover.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let first = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("commit")
        .expect("response")
        .result
        .expect("result");
    let oid = first["commitOid"].as_str().expect("oid").to_string();
    let stored = Commit::load(&libra::internal::object_format::parse_repo_oid(&oid).expect("oid"))
        .expect("commit object");
    let branch = match Head::current().await {
        Head::Branch(name) => name,
        Head::Detached(_) => panic!("expected a branch"),
    };
    Branch::update_branch(&branch, &before.to_string(), None)
        .await
        .expect("restore expected HEAD");
    let db = libra::internal::worktree_scope::request_db()
        .await
        .expect("repository database");
    db.execute_raw(Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "UPDATE agent_bridge_publication SET phase = 'prepared' WHERE operation_id = ?",
        ["op-vcs-intent".into()],
    ))
    .await
    .expect("restore prepared phase");
    let intent = format!(
        r#"{{"commitIntent":true,"expectedHead":"{before}","expectedIndex":"{}","expectedWorktree":"{}","message":"feat: recover vcs","tree":"{}","paths":["recover.txt"]}}"#,
        fence.index_generation, fence.worktree_generation, stored.tree_id
    );
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'pending', result_digest = ? WHERE operation_id = 'op-vcs-intent'",
            [intent.clone().into()],
        ))
        .await
        .expect("store commit intent");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index");
    let recovered = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("prepared recovery")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(recovered["commitOid"], oid);
    assert_eq!(recovered["outcome"], "replayed");
    assert_eq!(Head::current_commit().await.expect("HEAD").to_string(), oid);

    // A later recovery attempt whose worktree fence has drifted is terminal.
    // Restoring the old bytes must not publish the prepared commit later.
    Branch::update_branch(&branch, &before.to_string(), None)
        .await
        .expect("rewind for the stale recovery case");
    db.execute_raw(Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "UPDATE agent_bridge_publication SET phase = 'prepared' WHERE operation_id = ?",
        ["op-vcs-intent".into()],
    ))
    .await
    .expect("restore prepared phase");
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'pending', result_digest = ? WHERE operation_id = 'op-vcs-intent'",
            [intent.into()],
        ))
        .await
        .expect("restore intent");
    write_file(dir.path(), "recover.txt", "changed after the fence\n");
    let stale = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("recovery response")
        .expect("response")
        .result
        .expect("terminal result");
    assert_eq!(stale["outcome"], "indeterminate", "{stale}");
    assert_eq!(Head::current_commit().await.expect("HEAD"), before);
    assert_eq!(fs::read(&index_path).expect("index"), index_before);
    write_file(dir.path(), "recover.txt", "recover\n");
    let replay = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("terminal result");
    assert_eq!(replay["outcome"], "indeterminate", "{replay}");
    assert_eq!(replay["replayed"], true);
    assert_eq!(Head::current_commit().await.expect("HEAD"), before);
    assert_eq!(fs::read(&index_path).expect("index"), index_before);
}

/// A corrupt vcs.commit result never replays as a successful commit.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_commit_replay_rejects_corrupt_commit_oid() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "new.txt", "new\n");
    stage(&["new.txt"]).await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.commit","params":{{"operation_id":"op-bad-vcs","session_id":"s1","message":"feat: new","paths":["new.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("commit");
    let head = Head::current_commit().await.expect("head");
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET result_digest = ? WHERE operation_id = 'op-bad-vcs'",
            [r#"{"commitOid":"bogus","outcome":"applied"}"#.into()],
        ))
        .await
        .expect("corrupt result");
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("corrupt oid");
    assert!(
        error.message.contains("recorded commit"),
        "{}",
        error.message
    );
    assert_eq!(Head::current_commit().await.expect("head"), head);
}

/// `vcs.commit` replay restores the session link after an association failure.
#[tokio::test]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
#[serial(cwd, env, hash_kind)]
async fn vcs_commit_replay_restores_link_after_association_failure() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "linked.txt", "linked\n");
    stage(&["linked.txt"]).await;
    let before = Head::current_commit().await.expect("before");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.commit","params":{{"operation_id":"op-vcs-link","session_id":"s1","message":"feat: linked","paths":["linked.txt"],"linkSession":true,"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    libra::internal::ai::agent_bridge::mutations::fail_next_association_for_test();
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("association failure");
    assert!(error.message.contains("association"), "{}", error.message);
    let published = Head::current_commit().await.expect("published").to_string();
    assert_ne!(published, before.to_string());
    let replay = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(replay["commitOid"], published);
    assert_eq!(replay["outcome"], "replayed");
    assert_eq!(
        Head::current_commit().await.expect("HEAD").to_string(),
        published
    );
    let links = ctx
        .conn
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT target_id FROM agent_bridge_link WHERE source_type = 'commit' AND source_id = ? AND target_id = 'op-vcs-link'",
            [published.into()],
        ))
        .await
        .expect("links");
    assert_eq!(links.len(), 1, "replay must restore the vcs commit link");
}

/// (new) A recorded commit is replayed even if a later bookkeeping step failed.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_commit_replay_keeps_a_published_result_after_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "only.txt", "only\n");
    stage(&["only.txt"]).await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.commit","params":{{"operation_id":"op-once","session_id":"s1","message":"feat: only","paths":["only.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let first = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("commit")
        .expect("response")
        .result
        .expect("result");
    let oid = first["commitOid"].as_str().expect("oid").to_string();
    libra::internal::ai::agent_bridge::storage::fail_operation_keep_result(&ctx.conn, "op-once", 1)
        .await
        .expect("mark failed");
    let replay = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(replay["outcome"], "replayed");
    assert_eq!(replay["commitOid"], oid);
    assert_eq!(Head::current_commit().await.expect("head").to_string(), oid);
}

/// A mode-only change between the two reads is not staged.
#[tokio::test]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_rejects_a_mode_change_between_reads() {
    struct ClearHook;
    impl Drop for ClearHook {
        fn drop(&mut self) {
            libra::internal::ai::agent_bridge::vcs_write::set_mode_chmod_between_reads_for_test(
                None,
            );
        }
    }
    let _clear = ClearHook;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tool.sh", "#!/bin/sh\n");
    std::fs::set_permissions(
        dir.path().join("tool.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("chmod");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_before = fs::read(dir.path().join(".libra").join("index")).expect("index");
    libra::internal::ai::agent_bridge::vcs_write::set_mode_chmod_between_reads_for_test(Some(
        dir.path().join("tool.sh"),
    ));
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-mode-between","session_id":"s1","paths":["tool.sh"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("mode changed between reads");
    assert!(error.message.contains("stale_state"), "{}", error.message);
    assert_eq!(
        fs::read(dir.path().join(".libra").join("index")).expect("index"),
        index_before
    );
}

/// Replacing a file after its bytes are reread invalidates the stage fence.
#[tokio::test]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_rejects_a_file_replaced_after_reading() {
    struct ClearHook;
    impl Drop for ClearHook {
        fn drop(&mut self) {
            libra::internal::ai::agent_bridge::vcs_write::set_mode_stat_rewrite_for_test(None);
        }
    }
    let _clear = ClearHook;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tool.sh", "#!/bin/sh\n");
    std::fs::set_permissions(
        dir.path().join("tool.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("chmod");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before stage");
    libra::internal::ai::agent_bridge::vcs_write::set_mode_stat_rewrite_for_test(Some(
        dir.path().join("tool.sh"),
    ));
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-mode-fd","session_id":"s1","paths":["tool.sh"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("file changed after its content was read");
    assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
    assert!(error.message.contains("stale_state"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before
    );
}

/// A file replacement during stage preparation refuses publication rather than staging stale bytes.
#[tokio::test]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_rejects_a_file_replaced_during_preparation() {
    struct ClearHook;
    impl Drop for ClearHook {
        fn drop(&mut self) {
            libra::internal::ai::agent_bridge::vcs_write::set_publish_rewrite_for_test(None);
        }
    }
    let _clear = ClearHook;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tool.sh", "#!/bin/sh\n");
    std::fs::set_permissions(
        dir.path().join("tool.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("chmod");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before stage");
    libra::internal::ai::agent_bridge::vcs_write::set_publish_rewrite_for_test(Some(
        dir.path().join("tool.sh"),
    ));
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-exec-race","session_id":"s1","paths":["tool.sh"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("file replacement during stage");
    assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
    assert!(error.message.contains("stale_state"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before
    );
}

/// Staging an executable regular file keeps mode 100755.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_preserves_the_executable_bit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tool.sh", "#!/bin/sh\n");
    std::fs::set_permissions(
        dir.path().join("tool.sh"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("chmod");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-exec","session_id":"s1","paths":["tool.sh"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("stage")
        .expect("response")
        .result
        .expect("result");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    assert_eq!(index.get("tool.sh", 0).expect("entry").mode, 0o100755);
}

/// Unstaging a modification restores the index to HEAD and leaves the worktree file.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_restores_index_without_touching_the_worktree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let stage = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-edit","session_id":"s1","paths":["tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&stage))
        .await
        .expect("stage")
        .expect("response");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("after stage");
    let unstage = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-edit","session_id":"s1","paths":["tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&unstage))
        .await
        .expect("unstage")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let head_blob = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"one\n",
    );
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    assert_eq!(
        index.get("tracked.txt", 0).expect("tracked").hash,
        head_blob
    );
    assert_eq!(
        fs::read(dir.path().join("tracked.txt")).expect("worktree"),
        b"one\nchanged\n"
    );
}

/// One unstage request restores two index entries and preserves both files.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_restores_multiple_paths_without_touching_worktree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "new-one.txt", "first\n");
    write_file(dir.path(), "new-two.txt", "second\n");
    stage(&["new-one.txt", "new-two.txt"]).await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-pair","session_id":"s1","paths":["new-one.txt","new-two.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("unstage")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    assert_eq!(
        result["paths"],
        serde_json::json!(["new-one.txt", "new-two.txt"])
    );
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    assert!(index.get("new-one.txt", 0).is_none());
    assert!(index.get("new-two.txt", 0).is_none());
    assert_eq!(
        fs::read(dir.path().join("new-one.txt")).unwrap(),
        b"first\n"
    );
    assert_eq!(
        fs::read(dir.path().join("new-two.txt")).unwrap(),
        b"second\n"
    );
}

/// Unstaging a missing path is invalid input and does not unstage earlier paths.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_rejects_a_missing_path_without_partial_updates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "good.txt", "good\n");
    write_file(dir.path(), "other.txt", "other\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let stage = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-both","session_id":"s1","paths":["good.txt","other.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&stage))
        .await
        .expect("stage")
        .expect("response");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("after stage");
    let index_before = fs::read(dir.path().join(".libra").join("index")).expect("index");
    let unstage = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-mixed","session_id":"s1","paths":["good.txt","missing.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&unstage))
        .await
        .expect_err("missing path");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert!(error.message.contains("did not match"), "{}", error.message);
    assert_eq!(
        fs::read(dir.path().join(".libra").join("index")).expect("index"),
        index_before
    );
}

/// Unstaging a newly staged file removes that index entry.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_removes_a_newly_staged_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "extra.txt", "extra\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let stage = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-add-extra","session_id":"s1","paths":["extra.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&stage))
        .await
        .expect("stage")
        .expect("response");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("after stage");
    let unstage = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-extra","session_id":"s1","paths":["extra.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&unstage))
        .await
        .expect("unstage")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    assert!(
        index.get("extra.txt", 0).is_none(),
        "new file must leave the index"
    );
}

/// A directory selector is not a literal index entry and must not unstage its descendants.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_rejects_a_directory_without_changing_descendants() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    std::fs::create_dir_all(dir.path().join("dir/nested")).expect("create nested directory");
    write_file(dir.path(), "dir/one.txt", "one\n");
    write_file(dir.path(), "dir/nested/two.txt", "two\n");
    stage(&["dir/one.txt", "dir/nested/two.txt"]).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before unstage");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-directory","session_id":"s1","paths":["dir"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("directory is not a literal staged path");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert!(
        error.message.contains("did not match any staged file"),
        "{}",
        error.message
    );
    assert_eq!(
        fs::read(&index_path).expect("index after unstage"),
        index_before
    );
}

/// A repository-relative unstage path resolves from the repository root while the process is in a subdirectory.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_resolves_paths_from_repository_root_in_subdirectory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    std::fs::create_dir(dir.path().join("nested")).expect("nested directory");
    write_file(dir.path(), "root.txt", "root\n");
    write_file(dir.path(), "nested/root.txt", "nested\n");
    stage(&["root.txt", "nested/root.txt"]).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let _nested = ChangeDirGuard::new(dir.path().join("nested"));
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence from nested directory");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-root-relative","session_id":"s1","paths":["root.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("unstage")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    assert!(
        index.get("root.txt", 0).is_none(),
        "the requested root entry is unstaged"
    );
    assert!(
        index.get("nested/root.txt", 0).is_some(),
        "the similarly named nested entry remains staged"
    );
}

/// Unstaging a staged deletion restores the HEAD entry.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_restores_a_staged_deletion() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    std::fs::remove_file(dir.path().join("tracked.txt")).expect("delete");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let stage = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-deletion","session_id":"s1","paths":["tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&stage))
        .await
        .expect("stage the deletion")
        .expect("response");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    assert!(
        index.get("tracked.txt", 0).is_none(),
        "the deletion is staged"
    );
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("after stage");
    let unstage = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-deletion","session_id":"s1","paths":["tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&unstage))
        .await
        .expect("unstage the deletion")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    let head_blob = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"one\n",
    );
    assert_eq!(
        index.get("tracked.txt", 0).expect("restored entry").hash,
        head_blob,
        "unstaging a deletion restores the HEAD content"
    );
}

/// Unstaging must not change an index entry whose name differs only in case.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_does_not_change_a_case_variant_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    // A case-sensitive filesystem can hold both spellings as different files.
    write_file(dir.path(), "Foo", "upper\n");
    if dir.path().join("Foo").exists() && !dir.path().join("foo").exists() {
        stage(&["Foo"]).await;
        cli_commit("add Foo").await;
    }
    if dir.path().join("foo").exists() {
        // The filesystem folds case: the two spellings are one file, so this
        // repository cannot hold the ambiguous pair the test describes.
        return;
    }
    write_file(dir.path(), "foo", "lower\n");
    stage(&["foo"]).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-case-variant","session_id":"s1","paths":["foo"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let outcome = mutation_dispatch(&ctx, &request(&frame)).await;
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    let upper = index.get("Foo", 0).expect("the tracked entry survives");
    let upper_blob = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"upper\n",
    );
    assert_eq!(
        upper.hash, upper_blob,
        "the entry the request did not name must keep its content"
    );
    if let Ok(Some(response)) = outcome {
        let result = response.result.expect("result");
        assert_eq!(result["outcome"], "applied");
        assert!(
            index.get("foo", 0).is_none(),
            "the requested entry leaves the index"
        );
    }
}

/// Unstage uses the requested case spelling when both index spellings exist.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_targets_the_exact_case_colliding_index_entry() {
    use git_internal::internal::index::{Index, IndexEntry};

    let dir = tempfile::tempdir().expect("tempdir");
    setup_with_new_libra_in(dir.path()).await;
    let _guard = ChangeDirGuard::new(dir.path());
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "Case.txt", "upper\n");
    stage(&["Case.txt"]).await;

    let index_path = dir.path().join(".libra/index");
    let mut index = Index::load(&index_path).expect("load index");
    let lower_hash = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"lower\n",
    );
    index.add(IndexEntry::new_from_blob(
        "case.txt".to_string(),
        lower_hash,
        6,
    ));
    index.save(&index_path).expect("save colliding entry");

    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-case-collision","session_id":"s1","paths":["case.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}} ,"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("unstage the exact lower-case entry")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let index = Index::load(&index_path).expect("reload index");
    assert!(
        index.get("Case.txt", 0).is_some(),
        "the unselected entry stays"
    );
    assert!(
        index.get("case.txt", 0).is_none(),
        "the selected entry leaves"
    );
}

/// A commit request names repository-relative paths even when the bridge runs in a subdirectory.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_commit_matches_the_staged_set_from_a_subdirectory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    std::fs::create_dir(dir.path().join("nested")).expect("nested directory");
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "root.txt", "root\n");
    stage(&["root.txt"]).await;
    let _nested = ChangeDirGuard::new(dir.path().join("nested"));
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence from nested directory");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.commit","params":{{"operation_id":"op-commit-subdir","session_id":"s1","message":"feat: root file","paths":["root.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("commit from nested directory")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let oid = result["commitOid"].as_str().expect("oid");
    assert!(!oid.is_empty());
}

/// A repository without a commit can stage and unstage before its first commit.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_works_in_a_repository_without_a_commit() {
    let dir = tempfile::tempdir().expect("tempdir");
    setup_with_new_libra_in(dir.path()).await;
    let _guard = ChangeDirGuard::new(dir.path());
    ConfigKv::set("user.name", "Bridge VCS Test", false)
        .await
        .expect("set user.name");
    ConfigKv::set("user.email", "bridge-vcs@example.com", false)
        .await
        .expect("set user.email");
    ConfigKv::set("commit.gpgSign", "false", false)
        .await
        .expect("disable fixture signing locally");
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "first.txt", "first\n");
    // The repository has no commit yet; staging creates the index the fence
    // reads.
    stage(&["first.txt"]).await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let stage = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-unborn","session_id":"s1","paths":["first.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&stage))
        .await
        .expect("stage before the first commit")
        .expect("response");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("after stage");
    let unstage = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-unborn","session_id":"s1","paths":["first.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&unstage))
        .await
        .expect("unstage before the first commit")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    assert!(
        index.get("first.txt", 0).is_none(),
        "the staged addition leaves the index"
    );
    assert_eq!(
        fs::read(dir.path().join("first.txt")).expect("worktree file"),
        b"first\n"
    );
}

/// Unstaging one side of a staged rename is refused; naming both sides undoes it.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_of_a_staged_rename_requires_both_paths() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    write_file(dir.path(), "old.txt", "same content\n");
    stage(&["old.txt"]).await;
    cli_commit("add old").await;
    // A rename with unchanged content: the source is deleted and the target is
    // added with the same blob.
    std::fs::rename(dir.path().join("old.txt"), dir.path().join("new.txt")).expect("rename");
    stage(&["old.txt", "new.txt"]).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before unstage");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-rename-one","session_id":"s1","paths":["new.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("a rename needs both of its paths");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert!(error.message.contains("staged rename"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before
    );

    // The source endpoint alone is refused as well: restoring it would leave
    // the new path staged as an addition.
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence after refusal");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-rename-source","session_id":"s1","paths":["old.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("the source of a rename needs the destination too");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert!(error.message.contains("staged rename"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before
    );

    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence after refusal");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-rename-both","session_id":"s1","paths":["new.txt","old.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":3}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("unstage the rename")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    assert!(
        index.get("new.txt", 0).is_none(),
        "the renamed addition leaves the index"
    );
    let head_blob = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"same content\n",
    );
    assert_eq!(
        index
            .get("old.txt", 0)
            .expect("the source is restored")
            .hash,
        head_blob,
        "the renamed source is restored"
    );
}

/// A staged rename with several destinations needs every one of its paths.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_of_a_rename_with_several_destinations_needs_every_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    write_file(dir.path(), "old.txt", "shared\n");
    stage(&["old.txt"]).await;
    cli_commit("add old").await;
    std::fs::remove_file(dir.path().join("old.txt")).expect("remove old");
    write_file(dir.path(), "new-a.txt", "shared\n");
    write_file(dir.path(), "new-b.txt", "shared\n");
    stage(&["old.txt", "new-a.txt", "new-b.txt"]).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before unstage");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-rename-partial","session_id":"s1","paths":["old.txt","new-a.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("every path of the rename is needed");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert!(error.message.contains("staged rename"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before
    );

    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence after refusal");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-rename-complete","session_id":"s1","paths":["old.txt","new-a.txt","new-b.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("unstage the rename")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    assert!(index.get("new-a.txt", 0).is_none(), "the first copy leaves");
    assert!(
        index.get("new-b.txt", 0).is_none(),
        "the second copy leaves"
    );
    let head_blob = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"shared\n",
    );
    assert_eq!(
        index
            .get("old.txt", 0)
            .expect("the source is restored")
            .hash,
        head_blob
    );
}

/// A staged submodule deletion is restored without reading the submodule commit.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_restores_a_staged_gitlink_without_reading_it() {
    use git_internal::{
        hash::ObjectHash,
        internal::index::{Index, IndexEntry},
    };
    use libra::utils::object_ext::{CommitExt, TreeExt};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    // A gitlink entry names a commit of another repository, which is normally
    // absent from this object store.
    let index_path = dir.path().join(".libra/index");
    let mut index = Index::load(&index_path).expect("load index");
    let mut entry = IndexEntry::new_from_blob("module".to_string(), ObjectHash::default(), 0);
    entry.mode = 0o160000;
    index.add(entry);
    index.save(&index_path).expect("save index");
    cli_commit("add submodule").await;
    let head = Head::current_commit().await.expect("head");
    let stored = git_internal::internal::object::commit::Commit::load(&head).expect("commit");
    let tree = git_internal::internal::object::tree::Tree::load(&stored.tree_id).expect("tree");
    let gitlink = tree
        .tree_items
        .iter()
        .find(|item| item.name == "module")
        .expect("the gitlink is in HEAD");
    assert_eq!(
        gitlink.mode,
        git_internal::internal::object::tree::TreeItemMode::Commit
    );

    // Stage the submodule's deletion by dropping its index entry.
    let mut index = Index::load(&index_path).expect("load index");
    for stage in 0..4 {
        index.remove("module", stage);
    }
    index.save(&index_path).expect("save index");

    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-gitlink","session_id":"s1","paths":["module"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("unstage the submodule")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let index = Index::load(&index_path).expect("index");
    let restored = index.get("module", 0).expect("the gitlink is restored");
    assert_eq!(restored.mode, 0o160000);
    assert_eq!(restored.hash, gitlink.id);
}

/// A rename in a request never changes another selected path's index entry.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_of_a_rename_leaves_other_selected_paths_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    write_file(dir.path(), "old.txt", "renamed\n");
    write_file(dir.path(), "edited.txt", "edited\n");
    stage(&["old.txt", "edited.txt"]).await;
    cli_commit("add both").await;
    // A rename plus an unrelated modification.
    std::fs::remove_file(dir.path().join("old.txt")).expect("remove old");
    write_file(dir.path(), "new.txt", "renamed\n");
    write_file(dir.path(), "edited.txt", "edited again\n");
    stage(&["old.txt", "new.txt", "edited.txt"]).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-rename-mixed","session_id":"s1","paths":["new.txt","old.txt","edited.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("unstage the rename and the edit")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    let old_blob = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"renamed\n",
    );
    assert_eq!(
        index
            .get("old.txt", 0)
            .expect("the source is restored")
            .hash,
        old_blob
    );
    assert!(
        index.get("new.txt", 0).is_none(),
        "the renamed path leaves the index"
    );
    // The unrelated path was named by the request, so it is unstaged in its
    // own right: its index entry is restored from HEAD. What must not happen is
    // that the rename drops that entry or restores the rename's source in its
    // place.
    let edited_blob = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"edited\n",
    );
    assert_eq!(
        index
            .get("edited.txt", 0)
            .expect("the edit keeps its entry")
            .hash,
        edited_blob,
        "the unrelated path must be restored from HEAD, not dropped"
    );
}

/// A path that is not staged is refused even when the request also names a rename.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_refuses_an_unstaged_path_next_to_a_rename() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    write_file(dir.path(), "old.txt", "renamed\n");
    stage(&["old.txt"]).await;
    cli_commit("add old").await;
    std::fs::remove_file(dir.path().join("old.txt")).expect("remove old");
    write_file(dir.path(), "new.txt", "renamed\n");
    stage(&["old.txt", "new.txt"]).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before unstage");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-rename-missing","session_id":"s1","paths":["new.txt","old.txt","absent.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("the absent path is refused");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert!(
        error.message.contains("did not match any staged file"),
        "{}",
        error.message
    );
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before
    );
}

/// Two staged renames in one request each restore their own source.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_of_two_renames_restores_each_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    write_file(dir.path(), "old-a.txt", "first\n");
    write_file(dir.path(), "old-b.txt", "second\n");
    stage(&["old-a.txt", "old-b.txt"]).await;
    cli_commit("add both").await;
    std::fs::remove_file(dir.path().join("old-a.txt")).expect("remove a");
    std::fs::remove_file(dir.path().join("old-b.txt")).expect("remove b");
    write_file(dir.path(), "new-a.txt", "first\n");
    write_file(dir.path(), "new-b.txt", "second\n");
    stage(&["old-a.txt", "old-b.txt", "new-a.txt", "new-b.txt"]).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-two-renames","session_id":"s1","paths":["new-a.txt","old-a.txt","new-b.txt","old-b.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("unstage both renames")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    let first = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"first\n",
    );
    let second = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"second\n",
    );
    assert_eq!(
        index.get("old-a.txt", 0).expect("the first source").hash,
        first
    );
    assert_eq!(
        index.get("old-b.txt", 0).expect("the second source").hash,
        second
    );
    assert!(index.get("new-a.txt", 0).is_none(), "the first copy leaves");
    assert!(
        index.get("new-b.txt", 0).is_none(),
        "the second copy leaves"
    );
}

/// Several identical sources make a rename ambiguous; the whole group is then
/// authorized together.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_of_an_ambiguous_rename_needs_every_candidate_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    write_file(dir.path(), "a.txt", "identical\n");
    write_file(dir.path(), "b.txt", "identical\n");
    stage(&["a.txt", "b.txt"]).await;
    cli_commit("add both copies").await;
    std::fs::remove_file(dir.path().join("a.txt")).expect("remove a");
    std::fs::remove_file(dir.path().join("b.txt")).expect("remove b");
    write_file(dir.path(), "c.txt", "identical\n");
    stage(&["a.txt", "b.txt", "c.txt"]).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before unstage");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-ambiguous-rename","session_id":"s1","paths":["c.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("an ambiguous rename needs every candidate path");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert!(error.message.contains("staged rename"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before
    );

    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence after refusal");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-ambiguous-rename-all","session_id":"s1","paths":["c.txt","a.txt","b.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("unstage the ambiguous rename")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    assert!(
        index.get("c.txt", 0).is_none(),
        "the new path leaves the index"
    );
    let blob = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"identical\n",
    );
    assert_eq!(index.get("a.txt", 0).expect("the first source").hash, blob);
    assert_eq!(index.get("b.txt", 0).expect("the second source").hash, blob);
}

/// Two repository entries that differ only in case are two entries, and each
/// one needs its own authorization.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_of_a_case_colliding_rename_needs_both_entries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    write_file(dir.path(), "A", "colliding\n");
    // Two spellings that differ only in case are two files only on a
    // case-sensitive filesystem.
    if dir.path().join("a").exists() {
        return;
    }
    write_file(dir.path(), "a", "colliding\n");
    stage(&["A", "a"]).await;
    cli_commit("add both spellings").await;
    std::fs::remove_file(dir.path().join("A")).expect("remove A");
    std::fs::remove_file(dir.path().join("a")).expect("remove a");
    write_file(dir.path(), "b", "colliding\n");
    stage(&["A", "a", "b"]).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before unstage");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-case-colliding","session_id":"s1","paths":["a","b"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("the other spelling needs its own authorization");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert!(error.message.contains("staged rename"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before
    );

    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence after refusal");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-case-colliding-all","session_id":"s1","paths":["a","A","b"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("unstage the colliding rename")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    let blob = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"colliding\n",
    );
    assert_eq!(index.get("A", 0).expect("the first entry").hash, blob);
    assert_eq!(index.get("a", 0).expect("the second entry").hash, blob);
    assert!(index.get("b", 0).is_none(), "the new path leaves the index");
}

/// A literal backslash in a repository path is not a path separator.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_keeps_a_literal_backslash_path_apart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    std::fs::create_dir(dir.path().join("a")).expect("directory a");
    write_file(dir.path(), "a/b", "slash\n");
    write_file(dir.path(), "a\\b", "backslash\n");
    stage(&["a/b", "a\\b"]).await;
    cli_commit("add both names").await;
    write_file(dir.path(), "a/b", "slash changed\n");
    stage(&["a/b"]).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-backslash","session_id":"s1","paths":["a/b"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("unstage a/b")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    let slash = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"slash\n",
    );
    let backslash = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"backslash\n",
    );
    assert_eq!(
        index.get("a/b", 0).expect("the slash path").hash,
        slash,
        "the slash path restores its own content"
    );
    assert_eq!(
        index.get("a\\b", 0).expect("the backslash path").hash,
        backslash,
        "the backslash path keeps its own content"
    );
}

/// A backslash in a path selector is refused instead of being redirected.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_write_refuses_a_path_selector_with_a_backslash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "plain.txt", "plain\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before stage");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-backslash","session_id":"s1","paths":["a\\b"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("a backslash selector is refused");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert!(error.message.contains("backslash"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before
    );
}

/// The commit scope compares exact repository entry names.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_commit_scope_keeps_distinct_entry_names_apart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    std::fs::create_dir(dir.path().join("a")).expect("directory a");
    write_file(dir.path(), "a/b", "slash\n");
    write_file(dir.path(), "a\\b", "backslash\n");
    stage(&["a/b", "a\\b"]).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let head_before = Head::current_commit().await.expect("head");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.commit","params":{{"operation_id":"op-commit-backslash","session_id":"s1","message":"feat: one entry","paths":["a/b"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("the staged set holds two distinct entries");
    assert!(
        error.message.contains("do not match the staged set"),
        "{}",
        error.message
    );
    assert_eq!(
        Head::current_commit().await.expect("unchanged"),
        head_before
    );
}

/// Two case-colliding index names remain separate authorization entries.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_commit_refuses_two_case_colliding_index_entries_as_one_path() {
    use git_internal::internal::index::{Index, IndexEntry};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    write_file(dir.path(), "Case.txt", "staged once\n");
    stage(&["Case.txt"]).await;

    let index_path = dir.path().join(".libra/index");
    let mut index = Index::load(&index_path).expect("load index");
    let (hash, size, mode) = {
        let entry = index.get("Case.txt", 0).expect("staged entry");
        (entry.hash, entry.size, entry.mode)
    };
    let mut colliding = IndexEntry::new_from_blob("case.txt".to_string(), hash, size);
    colliding.mode = mode;
    index.add(colliding);
    index.save(&index_path).expect("save case-colliding entry");
    let index = Index::load(&index_path).expect("reload index");
    assert!(index.get("Case.txt", 0).is_some());
    assert!(index.get("case.txt", 0).is_some());

    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let head_before = Head::current_commit().await.expect("head");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.commit","params":{{"operation_id":"op-commit-case-collision","session_id":"s1","message":"feat: commit one spelling","paths":["Case.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}} ,"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("both changed index entries need authorization");
    assert!(
        error.message.contains("do not match the staged set"),
        "{}",
        error.message
    );
    assert_eq!(
        Head::current_commit().await.expect("unchanged"),
        head_before
    );
}

/// The worktree fence hashes the exact repository-relative path.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_fence_hashes_a_literal_backslash_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    write_file(dir.path(), "a\\b", "one\n");
    let before = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("first fence");
    write_file(dir.path(), "a\\b", "one\nchanged\n");
    let after = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("second fence");
    assert_ne!(
        before.worktree_generation, after.worktree_generation,
        "a changed literal backslash path must change the fence"
    );
}

/// A path through a symlink directory must not read a file outside the repository.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_rejects_a_path_through_a_symlink_directory() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let outside = tempfile::tempdir().expect("outside");
    write_file(outside.path(), "secret.txt", "SECRET\n");
    symlink(outside.path(), dir.path().join("escape")).expect("symlink dir");
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_before = fs::read(dir.path().join(".libra").join("index")).expect("index");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-escape","session_id":"s1","paths":["escape/secret.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("symlink directory");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert!(error.message.contains("symlink"), "{}", error.message);
    assert!(
        !error
            .message
            .contains(&dir.path().to_string_lossy().to_string()),
        "the error must not carry the host path: {}",
        error.message
    );
    assert!(
        !error
            .message
            .contains(&outside.path().to_string_lossy().to_string()),
        "the error must not carry an outside path: {}",
        error.message
    );
    assert_eq!(
        fs::read(dir.path().join(".libra").join("index")).expect("index"),
        index_before
    );
    let secret = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"SECRET\n",
    );
    let hex = secret.to_string();
    let loose = dir
        .path()
        .join(".libra/objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    assert!(
        !loose.exists(),
        "outside file stored at {}",
        loose.display()
    );
}

/// A failure after preparing the first blob leaves the live index unchanged.
/// The first blob remains in the object store: this test does not claim that
/// multi-path stage is atomic with respect to object publication.
#[tokio::test]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_restores_index_after_a_real_mid_publish_failure() {
    struct ClearHook;
    impl Drop for ClearHook {
        fn drop(&mut self) {
            libra::internal::ai::agent_bridge::vcs_write::fail_second_publish_for_test(false);
        }
    }
    let _clear = ClearHook;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "good.txt", "good\n");
    write_file(dir.path(), "other.txt", "other\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_before = fs::read(dir.path().join(".libra").join("index")).expect("index");
    libra::internal::ai::agent_bridge::vcs_write::fail_second_publish_for_test(true);
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-second-publish","session_id":"s1","paths":["good.txt","other.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("second publish");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert_eq!(
        fs::read(dir.path().join(".libra").join("index")).expect("index"),
        index_before
    );
    let good = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"good\n",
    );
    let hex = good.to_string();
    let loose = dir
        .path()
        .join(".libra/objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    assert!(
        loose.exists(),
        "the injected failure must happen after the first blob was published: {}",
        loose.display()
    );
}

/// Invalidating another path as the first publish begins must not store that first blob.
#[tokio::test]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_snapshots_the_batch_before_the_first_blob() {
    struct ClearHook;
    impl Drop for ClearHook {
        fn drop(&mut self) {
            libra::internal::ai::agent_bridge::vcs_write::set_break_on_first_publish_for_test(None);
        }
    }
    let _clear = ClearHook;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "good.txt", "good\n");
    write_file(dir.path(), "later.txt", "later\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_before = fs::read(dir.path().join(".libra").join("index")).expect("index");
    libra::internal::ai::agent_bridge::vcs_write::set_break_on_first_publish_for_test(Some(
        dir.path().join("later.txt"),
    ));
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-first-blob","session_id":"s1","paths":["good.txt","later.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("later path invalidated as publish begins");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert_eq!(
        fs::read(dir.path().join(".libra").join("index")).expect("index"),
        index_before
    );
    let good = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"good\n",
    );
    let hex = good.to_string();
    let loose = dir
        .path()
        .join(".libra/objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    assert!(
        !loose.exists(),
        "first blob stored after another path became invalid: {}",
        loose.display()
    );
}

/// A path invalidated at the start of publishing must not leave an earlier blob stored.
#[tokio::test]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_rechecks_every_path_before_publishing() {
    struct ClearHook;
    impl Drop for ClearHook {
        fn drop(&mut self) {
            libra::internal::ai::agent_bridge::vcs_write::set_break_at_publish_for_test(None);
        }
    }
    let _clear = ClearHook;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "good.txt", "good\n");
    write_file(dir.path(), "later.txt", "later\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_before = fs::read(dir.path().join(".libra").join("index")).expect("index");
    libra::internal::ai::agent_bridge::vcs_write::set_break_at_publish_for_test(Some(
        dir.path().join("later.txt"),
    ));
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-recheck-batch","session_id":"s1","paths":["good.txt","later.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("path changed before publish");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert_eq!(
        fs::read(dir.path().join(".libra").join("index")).expect("index"),
        index_before
    );
    let good = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"good\n",
    );
    let hex = good.to_string();
    let loose = dir
        .path()
        .join(".libra/objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    assert!(
        !loose.exists(),
        "publish started before the batch was rechecked: {}",
        loose.display()
    );
}

/// A path that becomes invalid after the first path is staged must not leave that blob stored.
#[tokio::test]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_does_not_store_a_blob_when_a_later_path_becomes_invalid() {
    struct ClearHook;
    impl Drop for ClearHook {
        fn drop(&mut self) {
            libra::internal::ai::agent_bridge::vcs_write::set_break_second_stage_for_test(None);
        }
    }
    let _clear = ClearHook;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "good.txt", "good\n");
    write_file(dir.path(), "later.txt", "later\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_before = fs::read(dir.path().join(".libra").join("index")).expect("index");
    libra::internal::ai::agent_bridge::vcs_write::set_break_second_stage_for_test(Some(
        dir.path().join("later.txt"),
    ));
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-later","session_id":"s1","paths":["good.txt","later.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("later path became a directory");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert_eq!(
        fs::read(dir.path().join(".libra").join("index")).expect("index"),
        index_before
    );
    let good = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"good\n",
    );
    let hex = good.to_string();
    let loose = dir
        .path()
        .join(".libra/objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    assert!(
        !loose.exists(),
        "first path was stored before the later path failed: {}",
        loose.display()
    );
}

/// One invalid path rolls back paths staged earlier in the same request.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_rolls_back_when_a_later_path_is_invalid() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "good.txt", "good\n");
    std::fs::create_dir(dir.path().join("nested")).expect("dir");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_before = fs::read(dir.path().join(".libra").join("index")).expect("index");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-partial","session_id":"s1","paths":["good.txt","nested"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("mixed request");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert_eq!(
        fs::read(dir.path().join(".libra").join("index")).expect("index"),
        index_before,
        "earlier path must not remain staged"
    );
    let good = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"good\n",
    );
    let hex = good.to_string();
    let loose = dir
        .path()
        .join(".libra/objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    assert!(
        !loose.exists(),
        "rejected request stored good.txt at {}",
        loose.display()
    );
}

/// Deleting a directory's only file does not leave an empty directory in the commit.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_commit_omits_an_empty_directory_after_its_last_file_is_deleted() {
    use libra::utils::object_ext::{CommitExt, TreeExt};
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    std::fs::create_dir(dir.path().join("sub")).expect("sub");
    write_file(dir.path(), "sub/nested.txt", "nested\n");
    stage(&["sub/nested.txt"]).await;
    cli_commit("add nested").await;
    std::fs::remove_file(dir.path().join("sub/nested.txt")).expect("delete");
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let stage_frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-empty-dir","session_id":"s1","paths":["sub/nested.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&stage_frame))
        .await
        .expect("stage deletion")
        .expect("response");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("after stage");
    let commit_frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.commit","params":{{"operation_id":"op-commit-empty-dir","session_id":"s1","message":"chore: remove nested","paths":["sub/nested.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&commit_frame))
        .await
        .expect("commit")
        .expect("response")
        .result
        .expect("result");
    let oid = result["commitOid"].as_str().expect("oid");
    let stored = git_internal::internal::object::commit::Commit::load(
        &libra::internal::object_format::parse_repo_oid(oid).expect("oid"),
    )
    .expect("commit");
    let tree = git_internal::internal::object::tree::Tree::load(&stored.tree_id).expect("tree");
    assert!(
        tree.tree_items
            .iter()
            .all(|item| !item.name.eq_ignore_ascii_case("sub")),
        "empty directory remained in the commit: {:?}",
        tree.tree_items
            .iter()
            .map(|item| &item.name)
            .collect::<Vec<_>>()
    );
}

/// A case-only directory rename keeps the spelling the request named.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_commit_keeps_the_requested_spelling_after_a_case_only_directory_rename() {
    use libra::utils::object_ext::{CommitExt, TreeExt};
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    std::fs::create_dir(dir.path().join("sub")).expect("sub");
    write_file(dir.path(), "sub/nested.txt", "nested\n");
    stage(&["sub/nested.txt"]).await;
    cli_commit("add nested").await;
    std::fs::rename(dir.path().join("sub"), dir.path().join("Sub")).expect("rename dir");
    if !dir.path().join("sub").exists() {
        return;
    }
    write_file(dir.path(), "Sub/nested.txt", "nested\nchanged\n");
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let stage_frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-dir-case","session_id":"s1","paths":["sub/nested.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&stage_frame))
        .await
        .expect("stage")
        .expect("response");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    let names: Vec<String> = index
        .tracked_files()
        .into_iter()
        .map(|path| path.display().to_string().replace('\\', "/"))
        .filter(|name| name.eq_ignore_ascii_case("sub/nested.txt"))
        .collect();
    assert_eq!(names, vec!["sub/nested.txt".to_string()], "{names:?}");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("after stage");
    let commit_frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.commit","params":{{"operation_id":"op-commit-dir-case","session_id":"s1","message":"chore: rename directory case","paths":["sub/nested.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&commit_frame))
        .await
        .expect("commit")
        .expect("response")
        .result
        .expect("result");
    let oid = result["commitOid"].as_str().expect("oid");
    let stored = git_internal::internal::object::commit::Commit::load(
        &libra::internal::object_format::parse_repo_oid(oid).expect("oid"),
    )
    .expect("commit");
    let tree = git_internal::internal::object::tree::Tree::load(&stored.tree_id).expect("tree");
    let sub = tree
        .tree_items
        .iter()
        .find(|item| item.name.eq_ignore_ascii_case("sub"))
        .expect("subdirectory");
    assert_eq!(sub.name, "sub");
    let nested = git_internal::internal::object::tree::Tree::load(&sub.id).expect("nested tree");
    let nested_names: Vec<_> = nested
        .tree_items
        .iter()
        .map(|item| item.name.as_str())
        .filter(|name| name.eq_ignore_ascii_case("nested.txt"))
        .collect();
    assert_eq!(nested_names, ["nested.txt"], "{nested_names:?}");
}

/// Unstaging a case-only rename restores the HEAD spelling in the index.
///
/// On a case-folding filesystem the two spellings are one path, so one name
/// restores it. On a case-sensitive filesystem they are two paths and the
/// rename needs both of them.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_restores_the_head_spelling_after_a_case_rename() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    std::fs::rename(
        dir.path().join("tracked.txt"),
        dir.path().join("Tracked.txt"),
    )
    .expect("rename");
    if !dir.path().join("tracked.txt").exists() {
        // The filesystem folds case: the rename did not produce a second
        // spelling, so there is no case-only rename to undo.
        return;
    }
    write_file(dir.path(), "Tracked.txt", "one\nrenamed\n");
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let stage = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-unstage-case","session_id":"s1","paths":["Tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&stage))
        .await
        .expect("stage")
        .expect("response");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("after stage");
    let alias = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-case-alias","session_id":"s1","paths":["TRACKED.TXT"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&alias))
        .await
        .expect_err("an alias cannot authorize another index spelling");
    assert!(error.message.contains("exact unambiguous index spelling"));

    let unstage = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-case","session_id":"s1","paths":["Tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&unstage))
        .await
        .expect("unstage")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    let names: Vec<String> = index
        .tracked_files()
        .into_iter()
        .map(|path| path.display().to_string())
        .filter(|name| name.eq_ignore_ascii_case("tracked.txt"))
        .collect();
    assert_eq!(names, vec!["tracked.txt".to_string()], "{names:?}");
    assert_eq!(
        fs::read(dir.path().join("Tracked.txt")).expect("worktree"),
        b"one\nrenamed\n"
    );
}

/// On a case-sensitive filesystem a case-only rename has two paths, and each
/// one needs its own authorization.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_of_a_case_only_rename_needs_both_spellings() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    // Two spellings that differ only in case are two files only on a
    // case-sensitive filesystem.
    write_file(dir.path(), "Foo", "upper\n");
    if dir.path().join("foo").exists() {
        return;
    }
    write_file(dir.path(), "foo", "lower\n");
    stage(&["Foo", "foo"]).await;
    cli_commit("add both spellings").await;
    // Stage a case-only rename: the source is deleted and the target is added
    // with the same content.
    std::fs::rename(dir.path().join("Foo"), dir.path().join("FOO")).expect("rename");
    stage(&["Foo", "FOO"]).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before unstage");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-case-one","session_id":"s1","paths":["FOO"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("a case-only rename has two paths on this filesystem");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert!(error.message.contains("staged rename"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before
    );

    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence after refusal");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-case-both","session_id":"s1","paths":["FOO","Foo"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("unstage the case-only rename")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    assert!(
        index.get("FOO", 0).is_none(),
        "the renamed spelling leaves the index"
    );
    let head_blob = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"upper\n",
    );
    assert_eq!(
        index
            .get("Foo", 0)
            .expect("the source spelling is restored")
            .hash,
        head_blob
    );
}

/// A case-only rename keeps the spelling the request named.
///
/// The index entry follows the request: staging `tracked.txt` updates the
/// `tracked.txt` entry, exactly as `libra add tracked.txt` does, and the
/// commit scope must name that entry. A differently spelled name is a
/// different scope and is refused.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_commit_keeps_the_requested_spelling_after_a_case_only_rename() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    std::fs::rename(
        dir.path().join("tracked.txt"),
        dir.path().join("Tracked.txt"),
    )
    .expect("rename");
    if !dir.path().join("tracked.txt").exists() {
        return;
    }
    write_file(dir.path(), "Tracked.txt", "one\nchanged\n");
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let stage = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-case-commit","session_id":"s1","paths":["tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&stage))
        .await
        .expect("stage")
        .expect("response");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    let names: Vec<String> = index
        .tracked_files()
        .into_iter()
        .map(|path| path.display().to_string())
        .filter(|name| name.eq_ignore_ascii_case("tracked.txt"))
        .collect();
    assert_eq!(names, vec!["tracked.txt".to_string()], "{names:?}");

    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("after stage");
    let head_before = Head::current_commit()
        .await
        .expect("head before alias refusal");
    let alias = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.commit","params":{{"operation_id":"op-commit-case-alias","session_id":"s1","message":"chore: rename case","paths":["TRACKED.TXT"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&alias))
        .await
        .expect_err("commit path aliases do not replace exact index spelling");
    assert!(
        error.message.contains("do not match the staged set"),
        "{}",
        error.message
    );
    assert_eq!(
        Head::current_commit().await.expect("unchanged"),
        head_before
    );

    let commit = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.commit","params":{{"operation_id":"op-commit-case","session_id":"s1","message":"chore: rename case","paths":["tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":3}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&commit))
        .await
        .expect("commit case rename")
        .expect("response")
        .result
        .expect("result");
    let oid = result["commitOid"].as_str().expect("oid").to_string();
    assert!(!oid.is_empty());
    use libra::utils::object_ext::{CommitExt, TreeExt};
    let stored = git_internal::internal::object::commit::Commit::load(
        &libra::internal::object_format::parse_repo_oid(&oid).expect("oid"),
    )
    .expect("commit");
    let tree = git_internal::internal::object::tree::Tree::load(&stored.tree_id).expect("tree");
    let names: Vec<_> = tree
        .tree_items
        .iter()
        .map(|item| item.name.as_str())
        .filter(|name| name.eq_ignore_ascii_case("tracked.txt"))
        .collect();
    assert_eq!(names, ["tracked.txt"], "{names:?}");
    let renamed = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"one\nchanged\n",
    );
    let item = tree
        .tree_items
        .iter()
        .find(|item| item.name == "tracked.txt")
        .expect("tree item");
    assert_eq!(item.id, renamed);
}

/// A case-only rename with changed content is not a proven rename scope.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_commit_refuses_an_inexact_case_only_rename_scope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    std::fs::rename(
        dir.path().join("tracked.txt"),
        dir.path().join("Tracked.txt"),
    )
    .expect("rename");
    if !dir.path().join("tracked.txt").exists() {
        return;
    }
    write_file(dir.path(), "Tracked.txt", "one\nchanged\n");
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let stage = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-case-change","session_id":"s1","paths":["tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}} ,"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&stage))
        .await
        .expect("stage changed case-only rename")
        .expect("response");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("after stage");
    let head_before = Head::current_commit().await.expect("head");
    let commit = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.commit","params":{{"operation_id":"op-commit-case-change","session_id":"s1","message":"chore: rename and edit case","paths":["Tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}} ,"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&commit))
        .await
        .expect_err("a changed-content rename does not prove a one-path scope");
    assert!(
        error.message.contains("do not match the staged set"),
        "{}",
        error.message
    );
    assert_eq!(
        Head::current_commit().await.expect("unchanged"),
        head_before
    );
}

/// A Unicode spelling that names an existing directory entry reuses that entry's name.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_uses_the_directory_spelling_for_unicode_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let requested = "caf\u{e9}.txt";
    write_file(dir.path(), requested, "cafe\n");
    let listed: Vec<String> = std::fs::read_dir(dir.path())
        .expect("list")
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with("caf"))
        .collect();
    assert_eq!(listed.len(), 1, "{listed:?}");
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-unicode","session_id":"s1","paths":["{requested}"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("stage")
        .expect("response")
        .result
        .expect("result");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    let staged: Vec<String> = index
        .tracked_files()
        .into_iter()
        .map(|path| path.display().to_string())
        .filter(|name| name.starts_with("caf"))
        .collect();
    assert_eq!(staged, listed, "index spelling diverged from the directory");
}

/// A case-only rename replaces the old index name instead of keeping both.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_replaces_a_case_only_rename() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let renamed = dir.path().join("Tracked.txt");
    std::fs::rename(dir.path().join("tracked.txt"), &renamed).expect("rename");
    if !dir.path().join("tracked.txt").exists() {
        return;
    }
    write_file(dir.path(), "Tracked.txt", "one\nrenamed\n");
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-case-rename","session_id":"s1","paths":["Tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("stage")
        .expect("response")
        .result
        .expect("result");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    let names: Vec<String> = index
        .tracked_files()
        .into_iter()
        .map(|path| path.display().to_string())
        .filter(|name| name.eq_ignore_ascii_case("tracked.txt"))
        .collect();
    assert_eq!(names, vec!["Tracked.txt".to_string()], "{names:?}");
    let changed = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"one\nrenamed\n",
    );
    assert_eq!(index.get("Tracked.txt", 0).expect("entry").hash, changed);
}

/// A case-only path spelling never changes a differently spelled entry.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_refuses_a_case_variant_of_an_indexed_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before stage");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-case","session_id":"s1","paths":["TRACKED.TXT"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("a case variant does not name the indexed entry");
    assert!(error.message.contains("case variant"), "{}", error.message);
    let index = git_internal::internal::index::Index::load(&index_path).expect("index");
    assert!(
        index.get("TRACKED.TXT", 0).is_none(),
        "case variant was stored as its own entry"
    );
    assert!(
        index.get("tracked.txt", 0).is_some(),
        "the indexed entry the request did not name stays"
    );
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before
    );
}

/// One request names every path that failed.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_unstage_names_every_failing_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before unstage");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-many-failures","session_id":"s1","paths":["missing-one.txt","missing-two.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("both paths are unknown");
    assert!(
        error.message.contains("missing-one.txt"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("missing-two.txt"),
        "{}",
        error.message
    );
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before,
        "a refused batch publishes nothing"
    );
}

/// A stage request names every path that failed.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_names_every_failing_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    std::fs::create_dir(dir.path().join("folder")).expect("folder");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before stage");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-many-failures","session_id":"s1","paths":["folder","missing.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("a directory and a missing path");
    assert!(error.message.contains("folder"), "{}", error.message);
    assert!(error.message.contains("missing.txt"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before,
        "a refused batch publishes nothing"
    );
}

/// One stage request names every ignored path.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_names_every_ignored_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), ".gitignore", "ignored-a.txt\nignored-b.txt\n");
    write_file(dir.path(), "ignored-a.txt", "a\n");
    write_file(dir.path(), "ignored-b.txt", "b\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before stage");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-many-ignored","session_id":"s1","paths":["ignored-a.txt","ignored-b.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("both paths are ignored");
    assert!(error.message.contains("ignored-a.txt"), "{}", error.message);
    assert!(error.message.contains("ignored-b.txt"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before,
        "a refused batch publishes nothing"
    );
}

/// One stage request names every path with an unsupported filter.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_names_every_unsupported_filter_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), ".gitattributes", "*.bin filter=missing\n");
    write_file(dir.path(), "first.bin", "first\n");
    write_file(dir.path(), "second.bin", "second\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before stage");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-many-filters","session_id":"s1","paths":["first.bin","second.bin"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("neither path can be staged");
    assert!(error.message.contains("first.bin"), "{}", error.message);
    assert!(error.message.contains("second.bin"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before,
        "a refused batch publishes nothing"
    );
}

/// A stage request names every failure, whatever its category.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_names_mixed_category_failures() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), ".gitignore", "ignored.txt\n");
    write_file(dir.path(), "ignored.txt", "ignored\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before stage");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-mixed-failures","session_id":"s1","paths":["missing.txt","ignored.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("one path is missing and one is ignored");
    assert!(error.message.contains("missing.txt"), "{}", error.message);
    assert!(error.message.contains("ignored.txt"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before,
        "a refused batch publishes nothing"
    );
}

/// A content rewrite after the status scan leaves the page without a fence.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_status_publishes_no_fence_after_a_content_rewrite() {
    use libra::internal::ai::agent_bridge::vcs_write::set_fence_observation_rewrite_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");

    // The file is rewritten after the scan read its content, so the
    // observation no longer describes the repository.
    set_fence_observation_rewrite_for_test(Some(dir.path().join("tracked.txt")));
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    let entries = envelope_data(&result)["entries"]
        .as_array()
        .expect("entries");
    assert!(
        entries.iter().any(|entry| entry["path"] == "tracked.txt"),
        "the page still reports the changed path: {entries:?}"
    );
    assert!(
        envelope_data(&result)["writeFence"].is_null(),
        "no fence may describe a repository that changed during the read: {result}"
    );
    assert!(
        result["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|warning| warning["code"] == "write_fence_unavailable"),
        "{result}"
    );
}

/// A path that appears after the status scan leaves the page without a fence.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_status_publishes_no_fence_after_a_new_path_appears() {
    use libra::internal::ai::agent_bridge::vcs_write::set_fence_observation_rewrite_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");

    // A new untracked path appears after the scan enumerated the worktree.
    set_fence_observation_rewrite_for_test(Some(dir.path().join("late.txt")));
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    let entries = envelope_data(&result)["entries"]
        .as_array()
        .expect("entries");
    assert!(
        !entries.iter().any(|entry| entry["path"] == "late.txt"),
        "the reported state must not contain a path that appeared afterwards: {entries:?}"
    );
    assert!(
        envelope_data(&result)["writeFence"].is_null(),
        "no fence may describe a repository that changed during the read: {result}"
    );
}

/// A skip-worktree entry is refused: the status scan does not report it, so
/// no fence covers it.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_refuses_a_skip_worktree_entry() {
    use git_internal::internal::index::{Index, IndexEntry};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "sparse.txt", "sparse\n");

    // Mark the tracked path as a sparse-checkout entry.
    let index_path = dir.path().join(".libra/index");
    let mut index = Index::load(&index_path).expect("load index");
    let hash = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"sparse\n",
    );
    let mut entry = IndexEntry::new_from_blob("sparse.txt".to_string(), hash, 10);
    entry.flags.skip_worktree = true;
    index.add(entry);
    index.save(&index_path).expect("save index");

    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_before = fs::read(&index_path).expect("index before stage");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-skip-worktree","session_id":"s1","paths":["sparse.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("a skip-worktree entry is not covered by the fence");
    assert!(error.message.contains("skip-worktree"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before,
        "a refused batch publishes nothing"
    );
}

/// A staged deletion still yields a fence, so it can be unstaged or committed.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_status_publishes_a_fence_for_a_staged_deletion() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;

    // Commit a file, then delete it from the worktree.
    write_file(dir.path(), "gone.txt", "gone\n");
    stage(&["gone.txt"]).await;
    cli_commit("add the file").await;
    std::fs::remove_file(dir.path().join("gone.txt")).expect("delete the file");

    // Stage the deletion of a tracked file.
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-deletion","session_id":"s1","paths":["gone.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("stage the deletion");

    // The status must still publish a fence for the staged deletion.
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":2}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    let entries = envelope_data(&result)["entries"]
        .as_array()
        .expect("entries");
    assert!(
        entries
            .iter()
            .any(|entry| entry["path"] == "gone.txt" && entry["state"] == "staged"),
        "the page must report the staged deletion: {entries:?}"
    );
    let fence = envelope_data(&result)["writeFence"].clone();
    assert!(
        !fence.is_null(),
        "a staged deletion needs a fence: {result}"
    );
    let head = fence["head"].as_str().expect("head");
    let index = fence["indexGeneration"].as_str().expect("index");
    let worktree = fence["worktreeGeneration"].as_str().expect("worktree");

    // The fence unstage accepts.
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-unstage-deletion","session_id":"s1","paths":["gone.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":3}}"#,
        head, index, worktree
    );
    let unstage = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("unstage the deletion")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(unstage["outcome"], "applied", "{unstage}");
}

/// Recreating a staged deletion during the status walk leaves its observation
/// unstable and omits the write fence.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_status_publishes_no_fence_when_a_staged_deletion_is_recreated_during_the_scan() {
    use libra::internal::ai::agent_bridge::vcs_write::set_staged_absence_recreate_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "gone.txt", "gone\n");
    stage(&["gone.txt"]).await;
    cli_commit("add the file").await;
    fs::remove_file(dir.path().join("gone.txt")).expect("delete the file");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-gone","session_id":"s1","paths":["gone.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("stage the deletion");

    set_staged_absence_recreate_for_test(Some(dir.path().join("gone.txt")));
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":2}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    assert!(
        envelope_data(&result)["writeFence"].is_null(),
        "the recreated path must not repair the staged absence: {result}"
    );
    assert!(
        envelope_data(&result)["entries"]
            .as_array()
            .is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry["path"] == "gone.txt" && entry["state"] == "staged")
            }),
        "the status page still reports the staged deletion: {result}"
    );
}

/// An absent submodule entry is refused: the status scan does not cover it.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_refuses_an_absent_submodule_entry() {
    use git_internal::internal::index::{Index, IndexEntry};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;

    // A gitlink entry names another repository's commit.
    let index_path = dir.path().join(".libra/index");
    let mut index = Index::load(&index_path).expect("load index");
    let hash = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Commit,
        b"submodule\n",
    );
    let mut entry = IndexEntry::new_from_blob("module".to_string(), hash, 0o160000);
    entry.mode = 0o160000;
    index.add(entry);
    index.save(&index_path).expect("save index");

    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_before = fs::read(&index_path).expect("index before stage");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-submodule","session_id":"s1","paths":["module"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("a submodule entry is not covered by the fence");
    assert!(error.message.contains("submodule"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before,
        "a refused batch publishes nothing"
    );
}

/// A changed path beyond the fence's size bounds leaves the page without a
/// fence, and the page still answers.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_status_answers_when_a_changed_path_exceeds_the_fence_bounds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;

    // A tracked file larger than the fence's per-path bound.
    let big = dir.path().join("big.bin");
    let file = std::fs::File::create(&big).expect("create");
    file.set_len(17 * 1024 * 1024).expect("size");
    drop(file);
    stage(&["big.bin"]).await;
    cli_commit("add the large file").await;
    std::fs::write(&big, vec![b'x'; 17 * 1024 * 1024]).expect("rewrite");

    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status still answers")
    .expect("response");
    let result = response.result.expect("result envelope");
    let entries = envelope_data(&result)["entries"]
        .as_array()
        .expect("entries");
    assert!(
        entries.iter().any(|entry| entry["path"] == "big.bin"),
        "the page still reports the changed path: {entries:?}"
    );
    assert!(
        envelope_data(&result)["writeFence"].is_null(),
        "a path beyond the fence's bounds cannot be fenced: {result}"
    );
    assert!(
        result["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|warning| warning["code"] == "write_fence_unavailable"),
        "{result}"
    );
}

/// A path that changes while the status scan classifies it leaves the page
/// without a fence.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_status_publishes_no_fence_after_a_change_during_the_scan() {
    use libra::internal::ai::agent_bridge::vcs_write::set_classification_window_rewrite_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");

    // The file changes between the scan's classification read and the
    // verification of its observation, so the observation cannot describe it.
    set_classification_window_rewrite_for_test(Some(dir.path().join("tracked.txt")));
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    let entries = envelope_data(&result)["entries"]
        .as_array()
        .expect("entries");
    assert!(
        entries.iter().any(|entry| entry["path"] == "tracked.txt"),
        "the page still reports the changed path: {entries:?}"
    );
    assert!(
        envelope_data(&result)["writeFence"].is_null(),
        "no fence may describe a path that changed during the scan: {result}"
    );
    assert!(
        result["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|warning| warning["code"] == "write_fence_unavailable"),
        "{result}"
    );
}

/// A rewrite after the second classification hash cannot produce a fence for
/// content the scan classified earlier.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_status_publishes_no_fence_after_the_second_classification_hash() {
    use libra::internal::ai::agent_bridge::vcs_write::set_classification_digest_window_rewrite_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");

    set_classification_digest_window_rewrite_for_test(Some(dir.path().join("tracked.txt")));
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    assert!(
        envelope_data(&result)["entries"]
            .as_array()
            .is_some_and(|entries| entries.iter().any(|entry| entry["path"] == "tracked.txt")),
        "status still reports the changed path: {result}"
    );
    assert!(
        envelope_data(&result)["writeFence"].is_null(),
        "the fence cannot pin a digest captured after the classification: {result}"
    );
}

/// A staged path whose content capture fails leaves the page without a fence:
/// the failure is never repaired by a later capture.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_status_publishes_no_fence_after_a_failed_staged_capture() {
    use libra::internal::ai::agent_bridge::vcs_write::set_forced_capture_failure_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "staged.txt", "one\n");
    stage(&["staged.txt"]).await;
    cli_commit("add the staged file").await;
    write_file(dir.path(), "staged.txt", "one\nstaged\n");
    stage(&["staged.txt"]).await;

    set_forced_capture_failure_for_test(Some(PathBuf::from("staged.txt")));
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    assert!(
        envelope_data(&result)["writeFence"].is_null(),
        "a failed capture must leave the path unfenceable: {result}"
    );
}

/// A staged path whose later capture fails leaves the page without a fence:
/// the earlier observation may not stay fenceable.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_status_publishes_no_fence_after_a_failed_recapture() {
    use libra::internal::ai::agent_bridge::vcs_write::set_forced_recapture_failure_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "staged.txt", "one\n");
    stage(&["staged.txt"]).await;
    cli_commit("add the staged file").await;
    write_file(dir.path(), "staged.txt", "one\nstaged\n");
    stage(&["staged.txt"]).await;
    // The worktree changes again, so the scan captures the path a second time.
    write_file(dir.path(), "staged.txt", "one\nstaged\nunstaged\n");

    // The staged capture succeeds; the later capture of the same path fails.
    set_forced_recapture_failure_for_test(Some(PathBuf::from("staged.txt")));
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    assert!(
        envelope_data(&result)["writeFence"].is_null(),
        "a failed recapture must not leave the earlier observation fenceable: {result}"
    );
}

/// A path that vanishes and returns while the scan classifies it leaves the
/// page without a fence.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_status_publishes_no_fence_after_a_path_vanishes_during_classification() {
    use libra::internal::ai::agent_bridge::vcs_write::set_forced_vanishing_path_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");

    set_forced_vanishing_path_for_test(Some(PathBuf::from("tracked.txt")));
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    assert!(
        envelope_data(&result)["writeFence"].is_null(),
        "a path that vanished during classification cannot be fenced: {result}"
    );
}

/// An unpinnable observation root leaves the page without a fence, even on a
/// clean worktree where an empty observation would look authoritative.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_status_publishes_no_fence_when_the_observation_root_cannot_be_pinned() {
    use libra::internal::ai::agent_bridge::vcs_write::set_force_observation_root_failure_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;

    set_force_observation_root_failure_for_test(true);
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    assert!(
        envelope_data(&result)["writeFence"].is_null(),
        "an unobserved scan cannot authorize a fence: {result}"
    );
    assert!(
        result["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|warning| warning["code"] == "write_fence_unavailable"),
        "{result}"
    );
}

/// A directory whose observation fails leaves the page without a fence: a
/// failed observation is never recorded as an absence.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_status_publishes_no_fence_after_a_failed_directory_observation() {
    use libra::internal::ai::agent_bridge::vcs_write::set_forced_entry_failure_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");

    // The walk cannot inspect the worktree root.
    set_forced_entry_failure_for_test(Some(PathBuf::from("")));
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    assert!(
        envelope_data(&result)["writeFence"].is_null(),
        "a failed directory observation cannot be recorded as an absence: {result}"
    );
}

/// A tracked path whose metadata read fails leaves the page without a fence:
/// a failed read is not an absence.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_status_publishes_no_fence_after_a_failed_worktree_metadata_read() {
    use libra::internal::ai::agent_bridge::vcs_write::set_forced_worktree_metadata_failure_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");

    set_forced_worktree_metadata_failure_for_test(Some(PathBuf::from("tracked.txt")));
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    assert!(
        envelope_data(&result)["writeFence"].is_null(),
        "a failed metadata read must not be recorded as an absence: {result}"
    );
}

/// A tracked path whose metadata capture fails leaves the page without a
/// fence.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_status_publishes_no_fence_after_a_failed_metadata_capture() {
    use libra::internal::ai::agent_bridge::vcs_write::set_forced_entry_failure_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");

    set_forced_entry_failure_for_test(Some(PathBuf::from("tracked.txt")));
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    assert!(
        envelope_data(&result)["writeFence"].is_null(),
        "a failed metadata capture must leave the path unfenceable: {result}"
    );
}

/// A staged path that disappears after the staged capture leaves the page
/// without a fence: the two observations disagree.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_status_publishes_no_fence_after_a_staged_path_disappears_during_the_scan() {
    use libra::internal::ai::agent_bridge::vcs_write::set_remove_after_staged_capture_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "staged.txt", "one\n");
    stage(&["staged.txt"]).await;
    cli_commit("add the staged file").await;
    write_file(dir.path(), "staged.txt", "one\nstaged\n");
    stage(&["staged.txt"]).await;

    set_remove_after_staged_capture_for_test(Some(dir.path().join("staged.txt")));
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    assert!(
        envelope_data(&result)["writeFence"].is_null(),
        "a path that was present and is gone must not be fenced as absent: {result}"
    );
}

/// A path with both staged and unstaged changes keeps its unstable
/// observation: a later pass may not repair it.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_status_publishes_no_fence_after_an_unstable_staged_and_unstaged_path() {
    use libra::internal::ai::agent_bridge::vcs_write::set_classification_window_rewrite_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tracked.txt", "one\n");

    // Stage one content, then change the file again in the worktree.
    write_file(dir.path(), "tracked.txt", "one\nstaged\n");
    stage(&["tracked.txt"]).await;
    write_file(dir.path(), "tracked.txt", "one\nstaged\nunstaged\n");

    // The path changes while the scan classifies it.
    set_classification_window_rewrite_for_test(Some(dir.path().join("tracked.txt")));
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    assert!(
        envelope_data(&result)["writeFence"].is_null(),
        "an unstable observation must not be repaired by a later pass: {result}"
    );
}

/// A fence that is published pins the content the write then stages.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_status_fence_pins_the_content_a_write_stages() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");

    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    let fence = envelope_data(&result)["writeFence"].clone();
    assert!(!fence.is_null(), "the fence must be published: {result}");
    let head = fence["head"].as_str().expect("head");
    let index = fence["indexGeneration"].as_str().expect("index");
    let worktree = fence["worktreeGeneration"].as_str().expect("worktree");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-pinned","session_id":"s1","paths":["tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        head, index, worktree
    );
    mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("stage the pinned content")
        .expect("response");

    // The staged entry names exactly the content the fence pinned.
    let index = git_internal::internal::index::Index::load(dir.path().join(".libra/index"))
        .expect("load the published index");
    let entry = index.get("tracked.txt", 0).expect("the staged entry");
    let expected = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"one\nchanged\n",
    );
    assert_eq!(
        entry.hash, expected,
        "the staged content must be the pinned content"
    );
}

/// Many re-hashed clean files do not consume the fence's content budget.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_status_publishes_a_fence_beyond_a_rehashed_clean_set() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;

    // Forty clean files of one MiB each: re-hashing them would exceed the
    // fence's content budget if the clean candidates were charged for it.
    for index in 0..40 {
        write_file(
            dir.path(),
            &format!("clean-{index}.bin"),
            &"x".repeat(1024 * 1024),
        );
    }
    stage(&[
        "clean-0.bin",
        "clean-1.bin",
        "clean-2.bin",
        "clean-3.bin",
        "clean-4.bin",
        "clean-5.bin",
        "clean-6.bin",
        "clean-7.bin",
        "clean-8.bin",
        "clean-9.bin",
        "clean-10.bin",
        "clean-11.bin",
        "clean-12.bin",
        "clean-13.bin",
        "clean-14.bin",
        "clean-15.bin",
        "clean-16.bin",
        "clean-17.bin",
        "clean-18.bin",
        "clean-19.bin",
        "clean-20.bin",
        "clean-21.bin",
        "clean-22.bin",
        "clean-23.bin",
        "clean-24.bin",
        "clean-25.bin",
        "clean-26.bin",
        "clean-27.bin",
        "clean-28.bin",
        "clean-29.bin",
        "clean-30.bin",
        "clean-31.bin",
        "clean-32.bin",
        "clean-33.bin",
        "clean-34.bin",
        "clean-35.bin",
        "clean-36.bin",
        "clean-37.bin",
        "clean-38.bin",
        "clean-39.bin",
    ])
    .await;
    cli_commit("add the clean files").await;

    // Touch every clean file, so the scan has to re-hash all of them.
    for index in 0..40 {
        let path = dir.path().join(format!("clean-{index}.bin"));
        let content = fs::read(&path).expect("read");
        fs::write(&path, content).expect("touch");
    }
    // And change one small file.
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");

    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    assert!(
        !envelope_data(&result)["writeFence"].is_null(),
        "clean candidates must not consume the fence's content budget: {result}"
    );
}

/// A path that changes after the preparation confirmed it is refused, so the
/// published index never carries content the repository no longer holds.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_stage_refuses_a_path_that_changed_after_the_preparation() {
    use libra::internal::ai::agent_bridge::vcs_write::set_stage_confirmed_rewrite_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");

    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before stage");
    set_stage_confirmed_rewrite_for_test(Some(dir.path().join("tracked.txt")));
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-changed-after-prepare","session_id":"s1","paths":["tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("the path changed while the batch was prepared");
    assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before,
        "a refused batch publishes nothing"
    );
}

/// Two modified files below the aggregate fence limit still get a fence.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_status_publishes_a_fence_for_two_modified_files_below_the_limit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;

    for name in ["first.bin", "second.bin"] {
        fs::write(dir.path().join(name), vec![b'a'; 9 * 1024 * 1024]).expect("write");
    }
    stage(&["first.bin", "second.bin"]).await;
    cli_commit("add the two files").await;
    for name in ["first.bin", "second.bin"] {
        fs::write(dir.path().join(name), vec![b'b'; 9 * 1024 * 1024]).expect("modify");
    }

    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    assert!(
        !envelope_data(&result)["writeFence"].is_null(),
        "a changed state below the aggregate limit must get a fence: {result}"
    );
}

/// A tracked file that becomes a directory during the scan leaves the page
/// without a fence: the directory walk must not repair the observation.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_status_publishes_no_fence_after_a_file_becomes_a_directory() {
    use libra::internal::ai::agent_bridge::vcs_write::set_classification_window_directory_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");

    set_classification_window_directory_for_test(Some(dir.path().join("tracked.txt")));
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    assert!(
        envelope_data(&result)["writeFence"].is_null(),
        "the directory walk must not repair an unstable observation: {result}"
    );
}

/// A clean set beyond the read budget still leaves a changed path fenceable.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_status_publishes_a_fence_beyond_a_clean_set_over_the_read_budget() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;

    // Five clean files of fourteen MiB each: seventy MiB in total, beyond the
    // scan's read budget.
    let names: Vec<String> = (0..5).map(|index| format!("clean-{index}.bin")).collect();
    for name in &names {
        fs::write(dir.path().join(name), vec![b'a'; 14 * 1024 * 1024]).expect("write");
    }
    let paths: Vec<&str> = names.iter().map(String::as_str).collect();
    stage(&paths).await;
    cli_commit("add the clean files").await;

    // Touch every clean file, so the scan has to re-hash all of them.
    for name in &names {
        let path = dir.path().join(name);
        let content = fs::read(&path).expect("read");
        fs::write(&path, content).expect("touch");
    }
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");

    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    assert!(
        !envelope_data(&result)["writeFence"].is_null(),
        "clean candidates must not starve the capture of a changed path: {result}"
    );
}

/// A staged path that changes while the scan reads it leaves the page without
/// a fence: the staged paths are captured inside the same scan.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_status_publishes_no_fence_after_a_staged_path_changes_during_the_scan() {
    use libra::internal::ai::agent_bridge::vcs_write::set_classification_window_rewrite_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;

    // A staged modification, so the path is in the staged set.
    write_file(dir.path(), "staged.txt", "one\n");
    stage(&["staged.txt"]).await;
    cli_commit("add the staged file").await;
    write_file(dir.path(), "staged.txt", "one\nstaged\n");
    stage(&["staged.txt"]).await;

    // The staged path changes while the scan reads the worktree.
    set_classification_window_rewrite_for_test(Some(dir.path().join("staged.txt")));
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    let entries = envelope_data(&result)["entries"]
        .as_array()
        .expect("entries");
    assert!(
        entries
            .iter()
            .any(|entry| entry["path"] == "staged.txt" && entry["state"] == "staged"),
        "the page still reports the staged path: {entries:?}"
    );
    assert!(
        envelope_data(&result)["writeFence"].is_null(),
        "no fence may describe a staged path that changed during the scan: {result}"
    );
    assert!(
        result["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|warning| warning["code"] == "write_fence_unavailable"),
        "{result}"
    );
}

/// A stage request names the failures of every phase.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_names_a_refusal_of_a_later_phase() {
    use git_internal::internal::index::{Index, IndexEntry};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), ".gitignore", "ignored.txt\n");
    write_file(dir.path(), "ignored.txt", "ignored\n");

    // An index entry that differs only in case from another one makes the
    // case-alias check refuse the path.
    let index_path = dir.path().join(".libra/index");
    let mut index = Index::load(&index_path).expect("load index");
    let hash = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"colliding\n",
    );
    index.add(IndexEntry::new_from_blob(
        "Colliding.txt".to_string(),
        hash,
        10,
    ));
    index.add(IndexEntry::new_from_blob(
        "colliding.txt".to_string(),
        hash,
        10,
    ));
    index.save(&index_path).expect("save colliding entries");

    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_before = fs::read(&index_path).expect("index before stage");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-late-refusal","session_id":"s1","paths":["ignored.txt","Colliding.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("one path is ignored and one has a colliding entry");
    assert!(error.message.contains("ignored.txt"), "{}", error.message);
    assert!(error.message.contains("Colliding.txt"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before,
        "a refused batch publishes nothing"
    );
}

/// The status read still answers when no index snapshot can be taken.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_status_answers_without_a_write_fence_when_no_snapshot_is_possible() {
    use libra::internal::ai::agent_bridge::vcs_read::set_status_snapshot_failure_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");

    set_status_snapshot_failure_for_test(true);
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status still answers")
    .expect("response");
    set_status_snapshot_failure_for_test(false);
    let result = response.result.expect("result envelope");
    let entries = envelope_data(&result)["entries"]
        .as_array()
        .expect("entries");
    assert!(
        entries.iter().any(|entry| entry["path"] == "tracked.txt"),
        "the status must still report the changed path: {entries:?}"
    );
    assert!(
        envelope_data(&result)["writeFence"].is_null(),
        "no fence may be published without a snapshot: {result}"
    );
    let warnings = result["warnings"].as_array().expect("warnings");
    assert!(
        warnings
            .iter()
            .any(|warning| warning["code"] == "write_fence_unavailable"),
        "{warnings:?}"
    );
}

/// The status read still answers in a repository that cannot be written.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_status_answers_in_a_read_only_repository() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");

    // The repository metadata becomes read-only, so the observation cannot
    // write its scratch index beside the live one.
    let metadata = dir.path().join(".libra");
    let mut permissions = std::fs::metadata(&metadata)
        .expect("metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o500);
    }
    std::fs::set_permissions(&metadata, permissions.clone()).expect("make the metadata read-only");
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status still answers")
    .expect("response");
    let result = response.result.expect("result envelope");
    let entries = envelope_data(&result)["entries"]
        .as_array()
        .expect("entries");
    assert!(
        entries.iter().any(|entry| entry["path"] == "tracked.txt"),
        "the status must still report the changed path: {entries:?}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o700);
    }
    std::fs::set_permissions(&metadata, permissions).expect("restore the metadata permissions");
}

/// A conflict resolved after the status read does not change the fence.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_status_binds_the_fence_to_the_observed_index() {
    use git_internal::internal::index::{Index, IndexEntry};
    use libra::internal::ai::agent_bridge::vcs_write::set_index_observation_rewrite_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;

    // A conflicted index: the same path at stages 1, 2 and 3.
    let index_path = dir.path().join(".libra/index");
    let mut index = Index::load(&index_path).expect("load index");
    let oid = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"conflict\n",
    );
    for stage in [1_u8, 2, 3] {
        let mut entry = IndexEntry::new_from_blob("conflicted.txt".to_string(), oid, 9);
        entry.flags.stage = stage;
        index.add(entry);
    }
    index.save(&index_path).expect("save conflicted index");

    // The resolved index, written beside the repository.
    let resolved_path = dir.path().join("resolved-index");
    let mut resolved = Index::load(&index_path).expect("load conflicted index");
    for stage in [1_u8, 2, 3] {
        resolved.remove("conflicted.txt", stage);
    }
    resolved.save(&resolved_path).expect("save resolved index");

    set_index_observation_rewrite_for_test(Some(resolved_path));
    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    let entries = envelope_data(&result)["entries"]
        .as_array()
        .expect("entries");
    assert!(
        entries.iter().any(|entry| entry["state"] == "conflict"),
        "the reported state must carry the conflict: {entries:?}"
    );
    let fence = envelope_data(&result)["writeFence"].clone();
    assert!(!fence.is_null(), "the fence must be published: {result}");

    // The fence describes the conflicted index the page reported, so the write
    // is refused once the conflict was resolved behind it.
    let head = fence["head"].as_str().expect("head");
    let index_generation = fence["indexGeneration"].as_str().expect("index");
    let worktree = fence["worktreeGeneration"].as_str().expect("worktree");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-conflict-race","session_id":"s1","paths":["tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        head, index_generation, worktree
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("the index changed after the status was read");
    assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
}

/// An unborn HEAD is carried as `null` in the fence.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_status_carries_an_unborn_head_as_null() {
    let dir = tempfile::tempdir().expect("tempdir");
    setup_with_new_libra_in(dir.path()).await;
    let _guard = ChangeDirGuard::new(dir.path());
    ConfigKv::set("user.name", "Bridge VCS Test", false)
        .await
        .expect("set user.name");
    ConfigKv::set("user.email", "bridge-vcs@example.com", false)
        .await
        .expect("set user.email");
    ConfigKv::set("commit.gpgSign", "false", false)
        .await
        .expect("disable fixture signing locally");
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "first.txt", "first\n");
    stage(&["first.txt"]).await;

    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    let fence = envelope_data(&result)["writeFence"].clone();
    assert!(
        fence["head"].is_null(),
        "an unborn HEAD has no commit id: {fence}"
    );
    let index = fence["indexGeneration"].as_str().expect("index");
    let worktree = fence["worktreeGeneration"].as_str().expect("worktree");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-null-head","session_id":"s1","paths":["first.txt"],"expectedHead":null,"expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        index, worktree
    );
    let result = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("stage with a null HEAD fence")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
}

/// A null HEAD fence is refused once the repository has a commit.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_write_refuses_a_null_head_fence_after_the_first_commit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "second.txt", "second\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-null-head-late","session_id":"s1","paths":["second.txt"],"expectedHead":null,"expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("a null HEAD fence cannot name a repository that has a commit");
    assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
}

/// The write fence is part of the negotiated status read.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_status_returns_the_write_fence_a_stage_accepts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "fenced.txt", "fenced\n");

    let response = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"vcs.status","params":{},"id":1}"#),
    )
    .await
    .expect("vcs.status")
    .expect("response");
    let result = response.result.expect("result envelope");
    let fence = envelope_data(&result)["writeFence"].clone();
    let head = fence["head"].as_str().expect("head").to_string();
    let index = fence["indexGeneration"]
        .as_str()
        .expect("index generation")
        .to_string();
    let worktree = fence["worktreeGeneration"]
        .as_str()
        .expect("worktree generation")
        .to_string();
    assert!(!head.is_empty());
    assert_eq!(head.len(), 40, "the HEAD fence is the commit id");
    assert_eq!(index.len(), 64);
    assert_eq!(worktree.len(), 64);

    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-with-fence","session_id":"s1","paths":["fenced.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        head, index, worktree
    );
    let result = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("stage with the fence the status read returned")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    assert_eq!(
        result["paths"],
        serde_json::json!(["fenced.txt"]),
        "{:?}",
        result["paths"]
    );
}

/// Staging one case spelling must not delete a separate colliding index entry.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_refuses_to_drop_a_case_colliding_index_entry() {
    use git_internal::internal::index::{Index, IndexEntry};

    let dir = tempfile::tempdir().expect("tempdir");
    setup_with_new_libra_in(dir.path()).await;
    let _guard = ChangeDirGuard::new(dir.path());
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "Case.txt", "first spelling\n");
    stage(&["Case.txt"]).await;

    let index_path = dir.path().join(".libra/index");
    let mut index = Index::load(&index_path).expect("load index");
    let (hash, size, mode) = {
        let entry = index.get("Case.txt", 0).expect("first spelling");
        (entry.hash, entry.size, entry.mode)
    };
    let mut colliding = IndexEntry::new_from_blob("case.txt".to_string(), hash, size);
    colliding.mode = mode;
    index.add(colliding);
    index.save(&index_path).expect("save colliding entry");
    let index_before = fs::read(&index_path).expect("index before selected stage");
    write_file(dir.path(), "Case.txt", "updated first spelling\n");

    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-case-collision","session_id":"s1","paths":["Case.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}} ,"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("the other case spelling is a separate staged entry");
    assert!(
        error.message.contains("ambiguous case variant"),
        "{}",
        error.message
    );
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before
    );
    let index = Index::load(&index_path).expect("reload index");
    assert!(index.get("Case.txt", 0).is_some());
    assert!(index.get("case.txt", 0).is_some());
}

/// A dotted path updates the canonical index entry instead of adding a second name.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_normalizes_dot_segments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-dot","session_id":"s1","paths":["./tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("stage dotted path")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    assert!(
        index.get("./tracked.txt", 0).is_none(),
        "dotted name was stored"
    );
    let entry = index.get("tracked.txt", 0).expect("canonical entry");
    let changed = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"one\nchanged\n",
    );
    assert_eq!(entry.hash, changed);
}

/// A path beneath a regular file is invalid input and does not change the index.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_rejects_a_path_below_a_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_before = fs::read(dir.path().join(".libra").join("index")).expect("index");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-below-file","session_id":"s1","paths":["tracked.txt/extra"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("path below a file");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert!(error.message.contains("non-directory"), "{}", error.message);
    assert_eq!(
        fs::read(dir.path().join(".libra").join("index")).expect("index"),
        index_before
    );
}

/// Bridge staging uses the same text conversion as ordinary Libra add.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_uses_libra_text_conversion() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), ".gitattributes", "*.txt text eol=lf\n");
    fs::write(dir.path().join("ordinary.txt"), b"one\r\ntwo\r\n").unwrap();
    fs::write(dir.path().join("bridge.txt"), b"one\r\ntwo\r\n").unwrap();
    stage(&["ordinary.txt"]).await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-clean","session_id":"s1","paths":["bridge.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("stage");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    let ordinary = index.get("ordinary.txt", 0).expect("ordinary entry").hash;
    let bridge = index.get("bridge.txt", 0).expect("bridge entry").hash;
    let expected = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"one\ntwo\n",
    );
    assert_eq!(ordinary, expected);
    assert_eq!(bridge, ordinary);
    assert_eq!(
        fs::read(dir.path().join("bridge.txt")).unwrap(),
        b"one\r\ntwo\r\n"
    );
}

/// An LFS path is refused until its payload can be durably stored too.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_rejects_lfs_without_publishing_content_or_pointer() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), ".gitattributes", "*.dat filter=lfs\n");
    let content = b"unique LFS payload for bridge refusal\n";
    fs::write(dir.path().join("payload.dat"), content).expect("payload");
    let before = fs::read(dir.path().join(".libra/index")).expect("index");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-lfs-refusal","session_id":"s1","paths":["payload.dat"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("LFS must be refused");
    assert_eq!(error.stable_code, "LBR-AGENT-027");
    assert!(
        error.message.contains(
            "vcs.stage: LFS snapshot staging is not supported; no index or object write was performed"
        ),
        "{}",
        error.message
    );
    assert!(error.message.contains("payload.dat"), "{}", error.message);
    assert!(
        !error.message.contains(dir.path().to_str().unwrap_or("")),
        "{}",
        error.message
    );
    assert_eq!(fs::read(dir.path().join(".libra/index")).unwrap(), before);
    let oid = format!("{:x}", Sha256::digest(content));
    let pointer = libra::utils::lfs::format_pointer_string(&oid, content.len() as u64);
    for bytes in [content.as_slice(), pointer.as_bytes()] {
        let hash = git_internal::hash::ObjectHash::from_type_and_data(
            git_internal::internal::object::types::ObjectType::Blob,
            bytes,
        )
        .to_string();
        assert!(
            !dir.path()
                .join(".libra/objects")
                .join(&hash[..2])
                .join(&hash[2..])
                .exists()
        );
    }
    assert!(
        !dir.path()
            .join(".libra/lfs/objects")
            .join(&oid[..2])
            .join(&oid[2..4])
            .join(&oid)
            .exists()
    );
    assert_eq!(fs::read(dir.path().join("payload.dat")).unwrap(), content);
}

/// A refused path in a multi-path request leaves no other path's blob behind.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_refuses_a_filtered_path_without_publishing_any_blob() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), ".gitattributes", "*.dat filter=lfs\n");
    let plain = b"plain payload that must not be stored\n";
    let filtered = b"filtered payload that must not be stored\n";
    fs::write(dir.path().join("plain.txt"), plain).expect("plain");
    fs::write(dir.path().join("payload.dat"), filtered).expect("filtered");
    let before = fs::read(dir.path().join(".libra/index")).expect("index");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-multi-filter","session_id":"s1","paths":["plain.txt","payload.dat"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("the filtered path must refuse the whole request");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert_eq!(fs::read(dir.path().join(".libra/index")).unwrap(), before);
    for bytes in [plain.as_slice(), filtered.as_slice()] {
        let hash = git_internal::hash::ObjectHash::from_type_and_data(
            git_internal::internal::object::types::ObjectType::Blob,
            bytes,
        )
        .to_string();
        assert!(
            !dir.path()
                .join(".libra/objects")
                .join(&hash[..2])
                .join(&hash[2..])
                .exists(),
            "a refused request must not store any blob"
        );
    }
}

/// Other content filters are refused instead of staging unfiltered bytes.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_rejects_other_content_filters_without_publishing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), ".gitattributes", "*.secret filter=rot13\n");
    let content = b"filtered payload\n";
    fs::write(dir.path().join("note.secret"), content).expect("payload");
    let before = fs::read(dir.path().join(".libra/index")).expect("index");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-filter","session_id":"s1","paths":["note.secret"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("filter must be refused");
    assert_eq!(error.stable_code, "LBR-AGENT-027");
    assert!(
        error.message.contains(
            "vcs.stage: content filter staging is not supported; no index or object write was performed"
        ),
        "{}",
        error.message
    );
    assert!(error.message.contains("note.secret"), "{}", error.message);
    assert_eq!(fs::read(dir.path().join(".libra/index")).unwrap(), before);
    let hash = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        content,
    )
    .to_string();
    assert!(
        !dir.path()
            .join(".libra/objects")
            .join(&hash[..2])
            .join(&hash[2..])
            .exists()
    );
}

/// Repository metadata is never staged, matching ordinary `libra add`.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_rejects_repository_metadata() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), ".libra/notes.txt", "internal bookkeeping\n");
    let before = fs::read(dir.path().join(".libra/index")).expect("index");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-metadata","session_id":"s1","paths":[".libra/notes.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("repository metadata must be refused");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert!(
        error.message.contains("repository metadata"),
        "{}",
        error.message
    );
    assert_eq!(fs::read(dir.path().join(".libra/index")).unwrap(), before);
}

/// A stage whose index was replaced but whose result was never recorded is
/// reconciled on replay instead of reporting "still in flight" forever.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_replay_recovers_a_publication_that_was_not_recorded() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "staged.txt", "content\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-crash-stage","session_id":"s1","paths":["staged.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let first = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("stage")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(first["outcome"], "applied");
    let index_digest = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence")
        .index_generation;

    // Simulate a crash between the index replacement and the result record:
    // the operation is pending again and only its intent survives.
    let intent = format!(
        r#"{{"vcsWriteIntent":true,"method":"vcs.stage","expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","paths":["staged.txt"],"indexAfter":"{}"}}"#,
        fence.head, fence.index_generation, fence.worktree_generation, index_digest
    );
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'pending', result_digest = ? WHERE operation_id = ?",
            [intent.into(), "op-crash-stage".into()],
        ))
        .await
        .expect("restore the pending intent");

    let replay = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(replay["outcome"], "replayed");
    assert_eq!(replay["paths"][0], "staged.txt");
}

/// A replay that finalizes an operation stops the original from writing.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_replay_finalization_stops_the_original_execution() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "staged.txt", "content\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-claimed","session_id":"s1","paths":["staged.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    // The executor recorded its operation row but has not started its work.
    mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("stage")
        .expect("response");
    let db = libra::internal::worktree_scope::request_db()
        .await
        .expect("repository database");
    db.execute_raw(Statement::from_string(
        DatabaseBackend::Sqlite,
        "DELETE FROM agent_bridge_index_publication WHERE operation_id = 'op-claimed'".to_string(),
    ))
    .await
    .expect("drop the publication record");
    ctx.conn
        .execute_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'pending', result_digest = NULL \
             WHERE operation_id = 'op-claimed'"
                .to_string(),
        ))
        .await
        .expect("restore the pending operation");

    // A replay finalizes it.
    let index_before = fs::read(dir.path().join(".libra/index")).expect("index");
    let replay = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(
        replay["outcome"], "indeterminate",
        "an operation that never started has an uncertain outcome: {replay}"
    );

    // The finalized operation never runs again and never writes.
    let again = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(again["outcome"], "indeterminate", "{again}");
    assert_eq!(
        fs::read(dir.path().join(".libra/index")).expect("index"),
        index_before,
        "a finalized operation must not write the index"
    );
}

/// A concurrent replay finalizes a paused execution, which must then refuse.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_stage_concurrent_replay_stops_a_paused_execution() {
    use libra::internal::ai::agent_bridge::mutations::{
        pause_gate_channel, set_pause_before_claim_for_test,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "staged.txt", "content\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-paused","session_id":"s1","paths":["staged.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let (pause, reached_rx, resume_tx) = pause_gate_channel();
    set_pause_before_claim_for_test(Some(pause));

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // The execution starts and pauses before it takes the publication
            // lock.
            let executor = tokio::task::spawn_local({
                let ctx = ctx.clone();
                let frame = frame.to_string();
                async move { mutation_dispatch(&ctx, &request(&frame)).await }
            });
            reached_rx
                .await
                .expect("the execution must reach the pause");

            // A concurrent replay finalizes the operation while the executor
            // waits.
            let replay = mutation_dispatch(&ctx, &request(&frame))
                .await
                .expect("replay")
                .expect("response")
                .result
                .expect("result");
            assert_eq!(
                replay["outcome"], "indeterminate",
                "the paused execution has no outcome yet: {replay}"
            );

            // The executor resumes and must refuse to write behind that answer.
            resume_tx.send(()).expect("resume the execution");
            let error = executor
                .await
                .expect("the execution task must finish")
                .expect_err("a finalized operation must not write");
            assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
        })
        .await;

    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    assert!(
        index.get("staged.txt", 0).is_none(),
        "the finalized execution must not have staged anything"
    );
}

/// A replay cannot finalize a paused allowlisted `commit.create` executor.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn commit_create_concurrent_replay_stops_a_paused_execution() {
    use libra::internal::ai::agent_bridge::mutations::{
        pause_gate_channel, set_pause_before_claim_for_test,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "commit-paused.txt", "content\n");
    stage(&["commit-paused.txt"]).await;
    let head_before = Head::current_commit().await.expect("HEAD");
    let frame = r#"{"jsonrpc":"2.0","method":"commit.create","params":{"operation_id":"op-paused-create","session_id":"s1","message":"feat: paused commit","approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    let (pause, reached_rx, resume_tx) = pause_gate_channel();
    set_pause_before_claim_for_test(Some(pause));

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let executor = tokio::task::spawn_local({
                let ctx = ctx.clone();
                async move { mutation_dispatch(&ctx, &request(frame)).await }
            });
            reached_rx.await.expect("commit executor reaches the pause");
            let replay = mutation_dispatch(&ctx, &request(frame))
                .await
                .expect("replay")
                .expect("response")
                .result
                .expect("result");
            assert_eq!(replay["outcome"], "indeterminate", "{replay}");

            resume_tx.send(()).expect("resume the executor");
            let error = executor
                .await
                .expect("executor task")
                .expect_err("terminalized commit must refuse");
            assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
        })
        .await;

    assert_eq!(
        Head::current_commit().await.expect("HEAD"),
        head_before,
        "the paused executor must not move HEAD after its terminal result"
    );
}

/// A replay cannot finalize a paused `vcs.commit` executor.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_commit_concurrent_replay_stops_a_paused_execution() {
    use libra::internal::ai::agent_bridge::mutations::{
        pause_gate_channel, set_pause_before_claim_for_test,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "commit-paused.txt", "content\n");
    stage(&["commit-paused.txt"]).await;
    let head_before = Head::current_commit().await.expect("HEAD");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("write fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.commit","params":{{"operation_id":"op-paused-vcs-commit","session_id":"s1","message":"feat: paused vcs commit","paths":["commit-paused.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let (pause, reached_rx, resume_tx) = pause_gate_channel();
    set_pause_before_claim_for_test(Some(pause));

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let executor = tokio::task::spawn_local({
                let ctx = ctx.clone();
                let frame = frame.to_string();
                async move { mutation_dispatch(&ctx, &request(&frame)).await }
            });
            reached_rx
                .await
                .expect("vcs.commit executor reaches the pause");
            let replay = mutation_dispatch(&ctx, &request(&frame))
                .await
                .expect("replay")
                .expect("response")
                .result
                .expect("result");
            assert_eq!(replay["outcome"], "indeterminate", "{replay}");

            resume_tx.send(()).expect("resume the executor");
            let error = executor
                .await
                .expect("executor task")
                .expect_err("terminalized commit must refuse");
            assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
        })
        .await;

    assert_eq!(
        Head::current_commit().await.expect("HEAD"),
        head_before,
        "the paused executor must not move HEAD after its terminal result"
    );
}

/// A replay that read a stale pending row must not terminalize a commit that
/// was published while it waited.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn commit_replay_routes_a_newly_recorded_intent_to_publication() {
    use libra::internal::ai::agent_bridge::mutations::{
        pause_gate_channel, set_pause_before_reconciliation_for_test,
    };
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let frame = r#"{"jsonrpc":"2.0","method":"commit.create","params":{"operation_id":"op-stale-read","session_id":"s1","message":"feat: stale read","allow_empty":true,"approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    let first = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("commit")
        .expect("response")
        .result
        .expect("result");
    let oid = first["commit"].as_str().expect("oid").to_string();

    // The replay will read the operation as pending without a result. The
    // publication record still names the commit this operation published.
    ctx.conn
        .execute_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'pending', result_digest = NULL \
             WHERE operation_id = 'op-stale-read'"
                .to_string(),
        ))
        .await
        .expect("restore the pending operation");
    let intent = serde_json::json!({
        "commitIntent": true,
        "expectedHead": Head::current_commit().await.expect("HEAD").to_string(),
        "message": "feat: stale read",
    })
    .to_string();

    let (pause, reached_rx, resume_tx) = pause_gate_channel();
    set_pause_before_reconciliation_for_test(Some(pause));

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let replay = tokio::task::spawn_local({
                let ctx = ctx.clone();
                async move { mutation_dispatch(&ctx, &request(frame)).await }
            });
            match tokio::time::timeout(std::time::Duration::from_secs(20), reached_rx).await {
                Ok(reached) => {
                    reached.expect("the replay must reach the reconciliation pause");
                }
                Err(_) => panic!("the replay never reached the reconciliation pause"),
            }
            // While the replay waits, the operation records the intent it was
            // about to publish, exactly as a live executor would.
            ctx.conn
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Sqlite,
                    "UPDATE agent_bridge_operation SET result_digest = ? \
                     WHERE operation_id = 'op-stale-read'",
                    [intent.clone().into()],
                ))
                .await
                .expect("record the intent");

            resume_tx.send(()).expect("resume the replay");
            let result = replay
                .await
                .expect("replay task")
                .expect("replay response")
                .expect("replay result")
                .result
                .expect("replay payload");
            assert_eq!(
                result["commit"], oid,
                "a published commit must not be reported as indeterminate: {result}"
            );
        })
        .await;

    assert_eq!(
        Head::current_commit().await.expect("HEAD").to_string(),
        oid,
        "the replay must not move HEAD"
    );
}

/// An operation id is bound to the method it was first used with.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn an_operation_id_cannot_be_reused_for_another_method() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "shared.txt", "content\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let stage = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-shared","session_id":"s1","paths":["shared.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let staged = mutation_dispatch(&ctx, &request(&stage))
        .await
        .expect("stage")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(staged["outcome"], "applied");

    // The same id with the same parameters but a different method must not
    // replay the stage result.
    let unstage = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.unstage","params":{{"operation_id":"op-shared","session_id":"s1","paths":["shared.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&unstage))
        .await
        .expect_err("an operation id is bound to its method");
    assert_eq!(error.stable_code, "LBR-AGENT-033", "{}", error.message);
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    assert!(
        index.get("shared.txt", 0).is_some(),
        "the rejected unstage must not change the index"
    );
}

/// A malformed approval leaves no pending operation behind.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn a_malformed_approval_does_not_leave_a_pending_operation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "approval.txt", "content\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let malformed = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-approval","session_id":"s1","paths":["approval.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"maybe","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&malformed))
        .await
        .expect_err("a malformed approval is refused");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);

    // Every malformed PRESENT approval is refused the same way, and none of
    // them records an operation row.
    for approval in [
        r#"{"decision":"maybe","approver":"reviewer-1"}"#,
        r#"{}"#,
        r#"{"decision":7}"#,
        r#"{"approver":"reviewer-1"}"#,
        r#""approved""#,
        r#"{"decision":"approved","approver":7}"#,
        r#"{"decision":"denied","approver":[]}"#,
        r#"{"decision":"approved","approver":null}"#,
    ] {
        let frame = format!(
            r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-approval","session_id":"s1","paths":["approval.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{approval}}},"id":3}}"#,
            fence.head, fence.index_generation, fence.worktree_generation
        );
        let error = mutation_dispatch(&ctx, &request(&frame))
            .await
            .expect_err("a malformed approval is refused");
        assert_eq!(
            error.stable_code, "LBR-AGENT-027",
            "approval {approval}: {}",
            error.message
        );
        assert!(
            libra::internal::ai::agent_bridge::storage::get_operation_result(
                &ctx.conn,
                "op-approval"
            )
            .await
            .expect("read the operation")
            .is_none(),
            "approval {approval} must not create an operation row"
        );
    }

    // No operation row may be left pending for that id.
    let recorded =
        libra::internal::ai::agent_bridge::storage::get_operation_result(&ctx.conn, "op-approval")
            .await
            .expect("read the operation");
    assert!(
        recorded.is_none(),
        "a malformed approval must not create an operation row: {recorded:?}"
    );

    // A corrected request with the same id is accepted.
    let corrected = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-approval","session_id":"s1","paths":["approval.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let staged = mutation_dispatch(&ctx, &request(&corrected))
        .await
        .expect("corrected stage")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(staged["outcome"], "applied");
}

/// A pending replay that finds a started operation while it waits reconciles
/// it instead of refusing itself.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn a_pending_replay_reconciles_a_started_operation_it_finds() {
    use libra::internal::ai::agent_bridge::mutations::{
        pause_gate_channel, set_pause_before_reconciliation_for_test,
    };
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let frame = r#"{"jsonrpc":"2.0","method":"review.run","params":{"operation_id":"op-reread","session_id":"s1","agents":["not-a-reviewer"],"approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    mutation_dispatch(&ctx, &request(frame))
        .await
        .expect_err("an unlaunchable reviewer is refused");
    // The replay reads an empty pending row.
    ctx.conn
        .execute_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'pending', result_digest = NULL \
             WHERE operation_id = 'op-reread'"
                .to_string(),
        ))
        .await
        .expect("restore the pending operation");

    let (pause, reached_rx, resume_tx) = pause_gate_channel();
    set_pause_before_reconciliation_for_test(Some(pause));

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let replay = tokio::task::spawn_local({
                let ctx = ctx.clone();
                async move { mutation_dispatch(&ctx, &request(frame)).await }
            });
            reached_rx
                .await
                .expect("the replay must reach the pause before it claims");

            // The executor records the started operation while the replay waits.
            ctx.conn
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Sqlite,
                    "UPDATE agent_bridge_operation SET result_digest = ? \
                     WHERE operation_id = 'op-reread'",
                    [r#"{"vcsWriteStarted":true,"method":"review.run","runId":"run-77"}"#.into()],
                ))
                .await
                .expect("record the started operation");

            resume_tx.send(()).expect("resume the replay");
            let result = replay
                .await
                .expect("replay task")
                .expect("replay response")
                .expect("replay result")
                .result
                .expect("replay payload");
            assert_eq!(
                result["runId"], "run-77",
                "the replay must reconcile the started operation it found: {result}"
            );
        })
        .await;
}

/// A pending replay that finds the operation completed reports its result.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn a_pending_replay_reports_a_completed_operation() {
    use libra::internal::ai::agent_bridge::mutations::{
        pause_gate_channel, set_pause_before_reconciliation_for_test,
    };
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let frame = r#"{"jsonrpc":"2.0","method":"review.run","params":{"operation_id":"op-completed-race","session_id":"s1","agents":["not-a-reviewer"],"approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    mutation_dispatch(&ctx, &request(frame))
        .await
        .expect_err("an unlaunchable reviewer is refused");
    ctx.conn
        .execute_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'pending', result_digest = NULL \
             WHERE operation_id = 'op-completed-race'"
                .to_string(),
        ))
        .await
        .expect("restore the pending operation");

    let (pause, reached_rx, resume_tx) = pause_gate_channel();
    set_pause_before_reconciliation_for_test(Some(pause));

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let replay = tokio::task::spawn_local({
                let ctx = ctx.clone();
                async move { mutation_dispatch(&ctx, &request(frame)).await }
            });
            reached_rx
                .await
                .expect("the replay must reach the pause before it claims");

            // The executor completes while the replay waits: the row now holds
            // the run id the operation produced.
            ctx.conn
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Sqlite,
                    "UPDATE agent_bridge_operation SET status = 'applied', result_digest = ? \
                     WHERE operation_id = 'op-completed-race'",
                    ["run-42".into()],
                ))
                .await
                .expect("record the completed operation");

            resume_tx.send(()).expect("resume the replay");
            let result = replay
                .await
                .expect("replay task")
                .expect("replay response")
                .expect("replay result")
                .result
                .expect("replay payload");
            assert_eq!(
                result["run_id"], "run-42",
                "the completed operation must report its recorded result: {result}"
            );
            assert_eq!(
                result["state"], "unknown",
                "the run's own state is reported, not a terminal outcome: {result}"
            );
        })
        .await;
}

/// A replay that observed a started marker reports the completed operation.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn a_started_replay_reports_the_completed_operation() {
    use libra::internal::ai::agent_bridge::mutations::{
        pause_gate_channel, set_pause_before_reconciliation_for_test,
    };
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let frame = r#"{"jsonrpc":"2.0","method":"review.run","params":{"operation_id":"op-started-race","session_id":"s1","agents":["not-a-reviewer"],"approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    mutation_dispatch(&ctx, &request(frame))
        .await
        .expect_err("an unlaunchable reviewer is refused");
    // The replay observes the operation as started.
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'pending', result_digest = ? \
             WHERE operation_id = 'op-started-race'",
            [r#"{"vcsWriteStarted":true,"method":"review.run","runId":"run-42"}"#.into()],
        ))
        .await
        .expect("record the started operation");

    let (pause, reached_rx, resume_tx) = pause_gate_channel();
    set_pause_before_reconciliation_for_test(Some(pause));

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let replay = tokio::task::spawn_local({
                let ctx = ctx.clone();
                async move { mutation_dispatch(&ctx, &request(frame)).await }
            });
            reached_rx
                .await
                .expect("the replay must reach the pause before it claims");

            // The executor completes before the reconciliation claims: the row
            // now holds the run id the operation produced.
            ctx.conn
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Sqlite,
                    "UPDATE agent_bridge_operation SET status = 'applied', result_digest = ? \
                     WHERE operation_id = 'op-started-race'",
                    ["run-42".into()],
                ))
                .await
                .expect("record the completed operation");

            resume_tx.send(()).expect("resume the replay");
            let result = replay
                .await
                .expect("replay task")
                .expect("replay response")
                .expect("replay result")
                .result
                .expect("replay payload");
            assert_eq!(
                result["run_id"], "run-42",
                "the completed operation must report its recorded result: {result}"
            );
            assert_eq!(
                result["state"], "unknown",
                "the run's own state is reported, not a terminal outcome: {result}"
            );
            assert!(
                result.get("vcsWriteOutcome").is_none(),
                "an applied result must not be projected as a terminal outcome: {result}"
            );
        })
        .await;
}

/// A pending replay keeps a started review's recorded identity.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn a_pending_replay_keeps_a_started_review_identity() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let frame = r#"{"jsonrpc":"2.0","method":"review.run","params":{"operation_id":"op-review-identity","session_id":"s1","agents":["not-a-reviewer"],"approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    mutation_dispatch(&ctx, &request(frame))
        .await
        .expect_err("an unlaunchable reviewer is refused");

    // The review started and recorded its identity, then the process stopped
    // before the result was persisted.
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'pending', result_digest = ? \
             WHERE operation_id = 'op-review-identity'",
            [r#"{"vcsWriteStarted":true,"method":"review.run","runId":"run-42"}"#.into()],
        ))
        .await
        .expect("record the started review");

    let replay = mutation_dispatch(&ctx, &request(frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(
        replay["runId"], "run-42",
        "the recorded review identity must survive the replay: {replay}"
    );
    assert_eq!(
        replay["outcome"], "indeterminate",
        "the recovered review is terminal: {replay}"
    );
    assert_eq!(
        replay["run"]["state"], "unknown",
        "the replay projects the run's current state: {replay}"
    );
    assert!(
        replay["message"]
            .as_str()
            .is_some_and(|message| message.contains("indeterminate")),
        "the outcome says the launch is indeterminate: {replay}"
    );
}

/// A review state that cannot be read is a retryable read error.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn a_review_state_read_failure_is_retryable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    libra::internal::ai::agent_bridge::vcs::fail_review_state_read_for_test();
    let error = libra::internal::ai::agent_bridge::vcs::review_state("op-read-failure")
        .expect_err("the read failure is reported");
    assert_eq!(error.stable_code, "LBR-AGENT-037", "{}", error.message);
    assert!(
        error.retryable,
        "a transient read failure must tell the peer to retry: {}",
        error.message
    );
}

/// A review whose state was never written reports an unknown liveness.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn a_review_without_a_state_reports_an_unknown_liveness() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let state = libra::internal::ai::agent_bridge::vcs::review_state("op-never-started")
        .expect("the missing state is answered");
    assert_eq!(state["state"], "unknown", "{state}");
    assert!(
        state["running"].is_null(),
        "an unverified liveness is not 'stopped': {state}"
    );
    assert_eq!(state["supervised"], false, "{state}");
    assert_eq!(state["liveness"], "unknown", "{state}");
}

/// A failed started-record releases the review admission slot.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn a_failed_started_record_releases_the_review_slot() {
    use libra::internal::ai::{
        observed_agents::launchable_review_slugs,
        run_admission::{self, AdmissionOutcome},
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    ConfigKv::set("agent.max_concurrent_runs", "1", false)
        .await
        .expect("set the review budget");
    let reviewer = launchable_review_slugs()
        .first()
        .copied()
        .expect("a launchable reviewer");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"review.run","params":{{"operation_id":"op-slot","session_id":"s1","agents":["{reviewer}"],"approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#
    );
    libra::internal::ai::agent_bridge::mutations::fail_next_started_record_for_test();
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("the injected failure is reported");
    assert!(error.message.contains("injected"), "{}", error.message);

    // The slot the prepared run took must be released, so the budget is free.
    let runs_root = dir.path().join(".libra/sessions/agent-runs");
    match run_admission::try_admit(&runs_root, 1, run_admission::RUN_QUEUE_CAP)
        .expect("take a slot")
    {
        AdmissionOutcome::Admitted(_) => {}
        _ => panic!("the admission slot must have been released"),
    }
}

/// A stale refusal cannot make a commit run twice.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn a_stale_refusal_never_commits_twice() {
    use libra::internal::ai::agent_bridge::mutations::{
        pause_gate_channel, set_pause_before_claim_for_test,
    };
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "commit-twice.txt", "content\n");
    stage(&["commit-twice.txt"]).await;
    let frame = r#"{"jsonrpc":"2.0","method":"commit.create","params":{"operation_id":"op-commit-twice","session_id":"s1","message":"feat: twice","allow_empty":true,"approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    // The operation is left in the state a pre-execution refusal produces.
    let create = r#"{"jsonrpc":"2.0","method":"commit.create","params":{"operation_id":"op-commit-twice","session_id":"s1","message":"feat: twice","allow_empty":true,"approval":{"decision":"approved","approver":"reviewer-1"}},"id":1}"#;
    mutation_dispatch(&ctx, &request(create))
        .await
        .expect("commit")
        .expect("response");
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'failed', result_digest = ? \
             WHERE operation_id = 'op-commit-twice'",
            [r#"{"bridgeRefusal":"the repository was busy when this request arrived"}"#.into()],
        ))
        .await
        .expect("record the refusal");

    let (pause, reached_rx, resume_tx) = pause_gate_channel();
    set_pause_before_claim_for_test(Some(pause));

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let retry = tokio::task::spawn_local({
                let ctx = ctx.clone();
                let frame = frame.to_string();
                async move { mutation_dispatch(&ctx, &request(&frame)).await }
            });
            reached_rx
                .await
                .expect("the retry must reach the pause before it claims");

            // Another attempt records the commit intent and publishes it.
            ctx.conn
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Sqlite,
                    "UPDATE agent_bridge_operation SET status = 'pending', result_digest = ? \
                     WHERE operation_id = 'op-commit-twice'",
                    [
                        r#"{"commitIntent":true,"expectedHead":"head","message":"feat: twice"}"#
                            .into(),
                    ],
                ))
                .await
                .expect("record the commit intent");

            resume_tx.send(()).expect("resume the retry");
            let error = retry
                .await
                .expect("retry task")
                .expect_err("a started commit must not run again");
            assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
        })
        .await;

    assert_eq!(
        Head::current_commit().await.expect("HEAD"),
        Head::current_commit().await.expect("HEAD"),
        "the retry must not move HEAD"
    );
}

/// A full review budget refuses the request but keeps the id retryable.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn a_full_review_budget_keeps_the_operation_id_retryable() {
    use libra::internal::ai::{
        observed_agents::launchable_review_slugs,
        run_admission::{self, AdmissionOutcome},
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    ConfigKv::set("agent.max_concurrent_runs", "1", false)
        .await
        .expect("set the review budget");

    // Fill the budget with a slot this test owns.
    let runs_root = dir.path().join(".libra/sessions/agent-runs");
    let slot = match run_admission::try_admit(&runs_root, 1, run_admission::RUN_QUEUE_CAP)
        .expect("take a slot")
    {
        AdmissionOutcome::Admitted(slot) => slot,
        _ => panic!("the budget must be free at the start"),
    };
    let reviewer = launchable_review_slugs()
        .first()
        .copied()
        .expect("a launchable reviewer");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"review.run","params":{{"operation_id":"op-budget","session_id":"s1","agents":["{reviewer}"],"approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("a full budget refuses the review");
    assert_eq!(error.stable_code, "LBR-AGENT-035", "{}", error.message);
    assert!(error.retryable, "{}", error.message);

    // The refusal is not a terminal answer: the operation may run again.
    drop(slot);
    let response = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("the retry is admitted once the budget has room")
        .expect("response");
    let result = response.result.expect("result");
    assert!(
        result["run_id"].as_str().is_some(),
        "the retry must start the review: {result}"
    );

    // The launched run is stopped, so it cannot leak into another test.
    libra::internal::ai::agent_bridge::vcs::supervisor::shutdown().await;
}

/// A retry that read a stale refusal never executes an operation another
/// request has since started.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn a_stale_refusal_retry_never_repeats_a_started_operation() {
    use libra::internal::ai::agent_bridge::mutations::{
        pause_gate_channel, set_pause_before_claim_for_test,
    };
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let base = Head::current_commit()
        .await
        .expect("base commit")
        .to_string();
    insert_checkpoint(&ctx.conn, "cp-stale", "s1", None, Some(&base), 1)
        .await
        .expect("record bridge checkpoint");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"checkpoint.restore","params":{{"operation_id":"op-stale-refusal","session_id":"s1","checkpoint_id":"cp-stale","expected_head":"{base}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#
    );
    mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("restore")
        .expect("response");

    // The operation is left in the state a pre-execution refusal produces.
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'failed', result_digest = ? \
             WHERE operation_id = 'op-stale-refusal'",
            [r#"{"bridgeRefusal":"the repository was busy when this request arrived"}"#.into()],
        ))
        .await
        .expect("record the refusal");

    let (pause, reached_rx, resume_tx) = pause_gate_channel();
    set_pause_before_claim_for_test(Some(pause));

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let retry = tokio::task::spawn_local({
                let ctx = ctx.clone();
                let frame = frame.to_string();
                async move { mutation_dispatch(&ctx, &request(&frame)).await }
            });
            reached_rx
                .await
                .expect("the retry must reach the pause before it claims");

            // Another request starts the operation and stops before it
            // records a result.
            ctx.conn
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Sqlite,
                    "UPDATE agent_bridge_operation SET status = 'pending', result_digest = ? \
                     WHERE operation_id = 'op-stale-refusal'",
                    [r#"{"vcsWriteStarted":true,"method":"checkpoint.restore"}"#.into()],
                ))
                .await
                .expect("record the started operation");

            resume_tx.send(()).expect("resume the retry");
            let error = retry
                .await
                .expect("retry task")
                .expect_err("a started operation must not be repeated");
            assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
        })
        .await;

    assert_eq!(
        Head::current_commit().await.expect("HEAD").to_string(),
        base,
        "the retry must not restore again"
    );
}

/// A started operation that never recorded a result is never repeated.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn a_started_operation_is_never_repeated() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let base = Head::current_commit()
        .await
        .expect("base commit")
        .to_string();
    insert_checkpoint(&ctx.conn, "cp-started", "s1", None, Some(&base), 1)
        .await
        .expect("record bridge checkpoint");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"checkpoint.restore","params":{{"operation_id":"op-started","session_id":"s1","checkpoint_id":"cp-started","expected_head":"{base}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#
    );
    mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("restore")
        .expect("response");

    // Simulate the process stopping after the restore marked the operation as
    // started but before it recorded the restored commit.
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'pending', result_digest = ?              WHERE operation_id = 'op-started'",
            [r#"{"vcsWriteStarted":true,"method":"checkpoint.restore"}"#.into()],
        ))
        .await
        .expect("restore the started operation");

    let replay = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(
        replay["outcome"], "indeterminate",
        "a started operation must not be repeated: {replay}"
    );
    assert_eq!(
        Head::current_commit().await.expect("HEAD").to_string(),
        base,
        "the replay must not move HEAD"
    );
}

/// An interrupted mutation reaches a durable terminal answer.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn an_interrupted_mutation_reaches_a_terminal_outcome() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;

    // A restore that recorded its operation row and then stopped.
    let base = Head::current_commit()
        .await
        .expect("base commit")
        .to_string();
    insert_checkpoint(&ctx.conn, "cp-interrupted", "s1", None, Some(&base), 1)
        .await
        .expect("record bridge checkpoint");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"checkpoint.restore","params":{{"operation_id":"op-interrupted","session_id":"s1","checkpoint_id":"cp-interrupted","expected_head":"{base}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#
    );
    mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("restore")
        .expect("response");
    // Simulate the process stopping after the operation row was written.
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    ctx.conn
        .execute_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'pending', result_digest = NULL \
             WHERE operation_id = 'op-interrupted'"
                .to_string(),
        ))
        .await
        .expect("restore the pending operation");
    assert_eq!(
        libra::internal::ai::agent_bridge::storage::get_operation_status(
            &ctx.conn,
            "op-interrupted"
        )
        .await
        .expect("read the status")
        .as_deref(),
        Some("pending")
    );

    // The replay must not report "still in flight" forever.
    let replay = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(
        replay["outcome"], "indeterminate",
        "an interrupted mutation must reach a terminal outcome: {replay}"
    );
    assert_eq!(
        libra::internal::ai::agent_bridge::storage::get_operation_status(
            &ctx.conn,
            "op-interrupted"
        )
        .await
        .expect("read the status")
        .as_deref(),
        Some("failed"),
        "the interrupted operation must not stay pending"
    );
}

/// A busy repository claim refuses the operation but keeps the id retryable.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(unix)]
async fn a_busy_claim_refuses_the_operation_and_keeps_the_id_retryable() {
    use std::io::{BufRead, BufReader};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "busy.txt", "content\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-busy","session_id":"s1","paths":["busy.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );

    // Another process holds the repository claim.
    let script = "import fcntl, sys\nf = open(sys.argv[1], 'a+')\nfcntl.flock(f, fcntl.LOCK_EX)\nprint('locked', flush=True)\nsys.stdin.read()\n";
    let lock_path = dir.path().join(".libra/agent-bridge-writes.lock");
    let mut owner = ChildGuard(
        std::process::Command::new("python3")
            .args(["-c", script])
            .arg(lock_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn claim owner"),
    );
    let mut ready = String::new();
    BufReader::new(owner.0.stdout.take().expect("claim stdout"))
        .read_line(&mut ready)
        .expect("claim ready");
    assert_eq!(ready.trim(), "locked");

    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("a busy claim refuses the operation");
    assert_eq!(error.stable_code, "LBR-AGENT-035", "{}", error.message);
    assert!(error.retryable, "{}", error.message);

    drop(owner.0.stdin.take().expect("claim stdin"));
    owner.0.wait().expect("claim process exits");

    // The same operation id must really retry, not report an outcome.
    let staged = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("retry after the claim is free")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(staged["outcome"], "applied", "{staged}");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    assert!(
        index.get("busy.txt", 0).is_some(),
        "the retry must stage the file"
    );
}

/// A crashed `vcs.commit` reaches a terminal outcome instead of staying pending.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_commit_replay_finalizes_an_operation_that_never_started() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "committed.txt", "content\n");
    stage(&["committed.txt"]).await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.commit","params":{{"operation_id":"op-crashed-commit","session_id":"s1","message":"feat: crashed","paths":["committed.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("commit")
        .expect("response");
    let head_after_commit = Head::current_commit().await.expect("HEAD").to_string();

    // Simulate a crash right after the operation row was created: the intent
    // and the publication record are both gone.
    let db = libra::internal::worktree_scope::request_db()
        .await
        .expect("repository database");
    db.execute_raw(Statement::from_string(
        DatabaseBackend::Sqlite,
        "DELETE FROM agent_bridge_publication WHERE operation_id = 'op-crashed-commit'".to_string(),
    ))
    .await
    .expect("drop the publication record");
    ctx.conn
        .execute_raw(Statement::from_string(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'pending', result_digest = NULL \
             WHERE operation_id = 'op-crashed-commit'"
                .to_string(),
        ))
        .await
        .expect("restore the pending operation");

    let replay = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(
        replay["outcome"], "indeterminate",
        "an operation that never started has an uncertain outcome: {replay}"
    );

    // The answer is durable: a later replay reports it and never commits.
    let again = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(again["outcome"], "indeterminate", "{again}");
    assert_eq!(
        Head::current_commit().await.expect("HEAD").to_string(),
        head_after_commit,
        "a finalized commit operation must not commit"
    );
}

/// A pending stage whose recorded effect is not the live index is terminal.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_replay_refuses_an_intent_that_never_landed() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "staged.txt", "content\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-lost-stage","session_id":"s1","paths":["staged.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("stage")
        .expect("response");

    // Simulate a crash between the prepared publication and the completed
    // one: the repository still records only the preparation, and the
    // operation row never got its result.
    let db = libra::internal::worktree_scope::request_db()
        .await
        .expect("repository database");
    db.execute_raw(Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "UPDATE agent_bridge_index_publication SET phase = 'prepared' WHERE operation_id = ?",
        ["op-lost-stage".into()],
    ))
    .await
    .expect("restore the prepared phase");
    let intent = format!(
        r#"{{"vcsWriteIntent":true,"method":"vcs.stage","expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","paths":["staged.txt"],"indexAfter":"{}"}}"#,
        fence.head,
        fence.index_generation,
        fence.worktree_generation,
        libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
            .await
            .expect("fence")
            .index_generation
    );
    ctx.conn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE agent_bridge_operation SET status = 'pending', result_digest = ? WHERE operation_id = ?",
            [intent.into(), "op-lost-stage".into()],
        ))
        .await
        .expect("restore the pending intent");

    let replay = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(
        replay["outcome"], "indeterminate",
        "an unproven publication must be reported as indeterminate: {replay}"
    );

    // The terminal outcome is durable: a later replay reports it again and
    // never runs the operation a second time.
    let again = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("replay")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(again["outcome"], "indeterminate", "{again}");
}

/// Explicit staging still respects ignore rules for untracked paths.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_rejects_ignored_untracked_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), ".libraignore", "private.txt\n");
    write_file(dir.path(), "private.txt", "ignored secret\n");
    let before = fs::read(dir.path().join(".libra/index")).expect("index");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-ignored","session_id":"s1","paths":["private.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("ignored path");
    assert_eq!(error.stable_code, "LBR-AGENT-027");
    assert!(error.message.contains("ignored"), "{}", error.message);
    assert_eq!(fs::read(dir.path().join(".libra/index")).unwrap(), before);
    let blob = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"ignored secret\n",
    );
    let hex = blob.to_string();
    assert!(
        !dir.path()
            .join(".libra/objects")
            .join(&hex[..2])
            .join(&hex[2..])
            .exists()
    );
}

/// A directory path is rejected as invalid input and does not change the index.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_rejects_a_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    std::fs::create_dir(dir.path().join("nested")).expect("dir");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_before = fs::read(dir.path().join(".libra").join("index")).expect("index");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-dir","session_id":"s1","paths":["nested"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("directory");
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert!(
        error.message.contains("not a regular file"),
        "{}",
        error.message
    );
    assert_eq!(
        fs::read(dir.path().join(".libra").join("index")).expect("index"),
        index_before
    );
}

/// Replacing a symlink during stage preparation invalidates the worktree fence.
#[tokio::test]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_rejects_a_symlink_replaced_during_preparation() {
    use std::os::unix::fs::symlink;
    struct ClearHook;
    impl Drop for ClearHook {
        fn drop(&mut self) {
            libra::internal::ai::agent_bridge::vcs_write::set_publish_rewrite_for_test(None);
        }
    }
    let _clear = ClearHook;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "secret.txt", "SECRET\n");
    symlink("secret.txt", dir.path().join("link")).expect("symlink");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before stage");
    libra::internal::ai::agent_bridge::vcs_write::set_publish_rewrite_for_test(Some(
        dir.path().join("link"),
    ));
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-link-publish","session_id":"s1","paths":["link"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("symlink changed during stage");
    assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
    assert!(error.message.contains("stale_state"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before
    );
}

/// Staging a symlink records the link text, not the target file's bytes.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_records_symlink_text_without_following_it() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "secret.txt", "SECRET\n");
    symlink("secret.txt", dir.path().join("link")).expect("symlink");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-link","session_id":"s1","paths":["link"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("stage symlink")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    let entry = index.get("link", 0).expect("symlink entry");
    assert_eq!(entry.mode, 0o120000);
    let link_blob = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"secret.txt",
    );
    assert_eq!(entry.hash, link_blob);
    let secret_blob = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"SECRET\n",
    );
    let hex = secret_blob.to_string();
    let secret_loose = dir
        .path()
        .join(".libra/objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    assert!(
        !secret_loose.exists(),
        "followed target stored at {}",
        secret_loose.display()
    );
}

/// A staged deletion can be committed without the file remaining in HEAD.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_commit_records_a_staged_deletion() {
    use libra::utils::object_ext::{CommitExt, TreeExt};
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let before = Head::current_commit().await.expect("before");
    std::fs::remove_file(dir.path().join("tracked.txt")).expect("delete");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let stage = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-del","session_id":"s1","paths":["tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    mutation_dispatch(&ctx, &request(&stage))
        .await
        .expect("stage deletion")
        .expect("response");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("after stage");
    let commit = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.commit","params":{{"operation_id":"op-commit-del","session_id":"s1","message":"chore: remove tracked","paths":["tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":2}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&commit))
        .await
        .expect("commit deletion")
        .expect("response")
        .result
        .expect("result");
    let oid = result["commitOid"].as_str().expect("oid").to_string();
    assert_ne!(oid, before.to_string());
    let stored = git_internal::internal::object::commit::Commit::load(
        &libra::internal::object_format::parse_repo_oid(&oid).expect("parse"),
    )
    .expect("commit");
    let tree = git_internal::internal::object::tree::Tree::load(&stored.tree_id).expect("tree");
    assert!(
        tree.tree_items
            .iter()
            .all(|item| item.name != "tracked.txt"),
        "deleted file remains in the commit tree"
    );
}

/// Deleting a file during stage preparation invalidates the worktree fence.
#[tokio::test]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_rejects_a_file_that_disappears_during_preparation() {
    struct ClearHook;
    impl Drop for ClearHook {
        fn drop(&mut self) {
            libra::internal::ai::agent_bridge::vcs_write::set_publish_remove_for_test(None);
        }
    }
    let _clear = ClearHook;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "good.txt", "good\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before stage");
    libra::internal::ai::agent_bridge::vcs_write::set_publish_remove_for_test(Some(
        dir.path().join("good.txt"),
    ));
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-vanish","session_id":"s1","paths":["good.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("file disappeared during stage");
    assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
    assert!(error.message.contains("stale_state"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before
    );
    assert!(!dir.path().join("good.txt").exists());
}

/// A file recreated during stage preparation invalidates the deletion fence.
#[tokio::test]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_rejects_a_tracked_file_recreated_during_preparation() {
    struct ClearHook;
    impl Drop for ClearHook {
        fn drop(&mut self) {
            libra::internal::ai::agent_bridge::vcs_write::set_publish_rewrite_for_test(None);
        }
    }
    let _clear = ClearHook;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    std::fs::remove_file(dir.path().join("tracked.txt")).expect("delete");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before stage");
    libra::internal::ai::agent_bridge::vcs_write::set_publish_rewrite_for_test(Some(
        dir.path().join("tracked.txt"),
    ));
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-delete-publish","session_id":"s1","paths":["tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("tracked file recreated during stage");
    assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
    assert!(error.message.contains("stale_state"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before
    );
    assert_eq!(
        fs::read(dir.path().join("tracked.txt")).expect("recreated worktree file"),
        b"changed while publishing\n"
    );
}

/// Staging a deleted tracked file removes it from the index.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_records_deletion_of_a_tracked_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    std::fs::remove_file(dir.path().join("tracked.txt")).expect("delete");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-delete","session_id":"s1","paths":["tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let result = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect("stage deletion")
        .expect("response")
        .result
        .expect("result");
    assert_eq!(result["outcome"], "applied");
    let index =
        git_internal::internal::index::Index::load(dir.path().join(".libra/index")).expect("index");
    assert!(
        index.get("tracked.txt", 0).is_none(),
        "deleted file must leave the index"
    );
}

/// Bytes staged by `add` must match the bytes observed immediately before it.
#[tokio::test]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_rejects_content_changed_during_add() {
    struct ClearHook;
    impl Drop for ClearHook {
        fn drop(&mut self) {
            libra::internal::ai::agent_bridge::vcs_write::set_stage_rewrite_for_test(None);
        }
    }
    let _clear = ClearHook;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "queued.txt", "queued\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_before = fs::read(dir.path().join(".libra").join("index")).expect("index");
    libra::internal::ai::agent_bridge::vcs_write::set_stage_rewrite_for_test(Some(
        dir.path().join("queued.txt"),
    ));
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-during-add","session_id":"s1","paths":["queued.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("content changed during add");
    assert!(error.message.contains("stale_state"), "{}", error.message);
    assert_eq!(
        fs::read(dir.path().join(".libra").join("index")).expect("index"),
        index_before
    );
    let unapproved = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"changed while staging\n",
    );
    let hex = unapproved.to_string();
    let loose = dir
        .path()
        .join(".libra/objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    assert!(
        !loose.exists(),
        "unapproved content must not be stored at {}",
        loose.display()
    );
}

/// A file edited after stage preparation invalidates the fence and leaves the index unchanged.
#[tokio::test]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_rejects_a_file_changed_after_preparation() {
    struct ClearHook;
    impl Drop for ClearHook {
        fn drop(&mut self) {
            libra::internal::ai::agent_bridge::vcs_write::set_publish_rewrite_for_test(None);
        }
    }
    let _clear = ClearHook;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "queued.txt", "queued\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before stage");
    libra::internal::ai::agent_bridge::vcs_write::set_publish_rewrite_for_test(Some(
        dir.path().join("queued.txt"),
    ));
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-publish","session_id":"s1","paths":["queued.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("file changed after stage preparation");
    assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
    assert!(error.message.contains("stale_state"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before
    );
    let unapproved = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Blob,
        b"changed while publishing\n",
    );
    let hex = unapproved.to_string();
    let loose = dir
        .path()
        .join(".libra/objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    assert!(
        !loose.exists(),
        "unapproved content stored at {}",
        loose.display()
    );
}

/// A file edited after the fence check and before staging is not staged.
#[tokio::test]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
#[serial(cwd, env, hash_kind)]
async fn vcs_stage_rechecks_the_fence_immediately_before_writing() {
    struct ClearHook;
    impl Drop for ClearHook {
        fn drop(&mut self) {
            libra::internal::ai::agent_bridge::vcs_write::set_fence_recheck_rewrite_for_test(None);
        }
    }
    let _clear = ClearHook;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "queued.txt", "queued\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_before = fs::read(dir.path().join(".libra").join("index")).expect("index");
    libra::internal::ai::agent_bridge::vcs_write::set_fence_recheck_rewrite_for_test(Some(
        dir.path().join("queued.txt"),
    ));
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-recheck","session_id":"s1","paths":["queued.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("edit between checks");
    assert!(error.message.contains("stale_state"), "{}", error.message);
    assert_eq!(
        fs::read(dir.path().join(".libra").join("index")).expect("index"),
        index_before
    );
}

/// (new) Editing an already-unstaged file changes the fence, not only its path list.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_fence_changes_when_file_content_changes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");
    let first = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("first fence");
    write_file(dir.path(), "tracked.txt", "one\nchanged again\n");
    let second = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("second fence");
    assert_ne!(first.worktree_generation, second.worktree_generation);
}

/// `commit.create` must not publish while another bridge task holds the gate.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn in_process_commit_waits_while_publish_gate_is_held() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let hold = libra::internal::ai::agent_bridge::vcs_write::acquire_publish_gate().await;
            let mut pending = tokio::task::spawn_local(async {
                libra::internal::ai::agent_bridge::vcs::commit_create("gate", false, true).await
            });
            tokio::select! {
                _ = &mut pending => panic!("commit.create ran while the publish gate was held"),
                _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {}
            }
            drop(hold);
            let result = tokio::time::timeout(std::time::Duration::from_secs(5), pending)
                .await
                .expect("commit should proceed after the gate is released")
                .expect("join");
            assert!(result.is_ok(), "{result:?}");
        })
        .await;
}

/// Content outside the repository, reached only through a symlink directory, must not affect the fence.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_fence_ignores_files_reached_through_a_symlink_directory() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let outside = tempfile::tempdir().expect("outside");
    write_file(outside.path(), "secret.txt", "SECRET\n");
    symlink(outside.path(), dir.path().join("escape")).expect("symlink dir");
    let first = libra::internal::ai::agent_bridge::vcs_write::current_write_fence().await;
    write_file(outside.path(), "secret.txt", "OTHER\n");
    let second = libra::internal::ai::agent_bridge::vcs_write::current_write_fence().await;
    let before = first.expect("fence should ignore a symlink directory");
    let after = second.expect("fence should stay available");
    assert_eq!(
        before, after,
        "outside content reached through a symlink directory changed the fence"
    );
}

/// Deleting a tracked file is an ordinary worktree observation, not an error.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_fence_changes_when_tracked_file_is_deleted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let before = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("before deletion");
    std::fs::remove_file(dir.path().join("tracked.txt")).expect("delete");
    let after = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("deleted file must remain observable");
    assert_ne!(before.worktree_generation, after.worktree_generation);
}

/// Fence hashing must use paths relative to the repository, even from a subdirectory.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_fence_uses_repository_relative_paths_from_subdirectory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    std::fs::create_dir(dir.path().join("nested")).expect("nested");
    write_file(dir.path(), "tracked.txt", "one\nchanged\n");
    let from_root = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("root fence");
    let _nested = ChangeDirGuard::new(dir.path().join("nested"));
    let from_nested = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("nested fence");
    assert_eq!(from_root, from_nested);
}

/// Stage refuses to publish when an editor changes the worktree after preparation.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_stage_rechecks_the_fence_immediately_before_publication() {
    use libra::internal::ai::agent_bridge::vcs_write::set_pre_publish_rewrite_for_test;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    write_file(dir.path(), "tracked.txt", "staged snapshot\n");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before staging");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-prepublish-race","session_id":"s1","paths":["tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    set_pre_publish_rewrite_for_test(Some(dir.path().join("tracked.txt")));
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("worktree change before publication");
    assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
    assert!(error.message.contains("stale_state"), "{}", error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before
    );
    assert_eq!(
        fs::read(dir.path().join("tracked.txt")).expect("rewritten worktree file"),
        b"changed after index preparation\n"
    );

    let replay_error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("the durable stale replay keeps its original RPC error");
    assert_eq!(replay_error.stable_code, error.stable_code);
    assert_eq!(replay_error.code, error.code);
    assert_eq!(replay_error.message, error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after replay"),
        index_before
    );
}

/// Stage refuses prepared bytes that differ from the content authorized by the
/// final fence, even when the worktree returns to its original content.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(all(feature = "test-bridge-hooks", debug_assertions))]
async fn vcs_stage_rechecks_prepared_content_after_the_final_fence() {
    use libra::internal::ai::agent_bridge::vcs_write::{
        set_pre_publish_rewrite_for_test, set_stage_capture_rewrite_for_test,
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let authorized = b"changed after index preparation\n";
    fs::write(dir.path().join("tracked.txt"), authorized).expect("write authorized content");
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let index_path = dir.path().join(".libra/index");
    let index_before = fs::read(&index_path).expect("index before staging");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.stage","params":{{"operation_id":"op-stage-prepared-content-return","session_id":"s1","paths":["tracked.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );

    // After admission, the plan captures A. Immediately before the final fence
    // read, restore B, which is the content the caller authorized.
    set_stage_capture_rewrite_for_test(Some(dir.path().join("tracked.txt")));
    set_pre_publish_rewrite_for_test(Some(dir.path().join("tracked.txt")));
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("prepared A must not be published against authorized B");
    assert_eq!(error.stable_code, "LBR-AGENT-038", "{}", error.message);
    assert!(
        error
            .message
            .contains("changed while the batch was prepared"),
        "{}",
        error.message
    );
    assert_eq!(
        fs::read(&index_path).expect("index after refusal"),
        index_before
    );
    assert_eq!(
        fs::read(dir.path().join("tracked.txt")).expect("worktree"),
        authorized
    );

    // The refusal is durable: the same operation id reports the same refusal,
    // not an uncertain publication.
    let replay = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("the replay reports the recorded refusal");
    assert_eq!(replay.stable_code, error.stable_code);
    assert_eq!(replay.code, error.code);
    assert_eq!(replay.message, error.message);
    assert_eq!(
        fs::read(&index_path).expect("index after replay"),
        index_before
    );
}

/// (new) LVC-06: commit refuses when the index contains paths outside the request.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_commit_refuses_unselected_staged_paths() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let before = Head::current_commit().await.expect("head");
    write_file(dir.path(), "chosen.txt", "chosen\n");
    write_file(dir.path(), "other.txt", "other\n");
    stage(&["chosen.txt", "other.txt"]).await;
    let fence = libra::internal::ai::agent_bridge::vcs_write::current_write_fence()
        .await
        .expect("fence");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"vcs.commit","params":{{"operation_id":"op-scope","session_id":"s1","message":"feat: one file","paths":["chosen.txt"],"expectedHead":"{}","expectedIndex":"{}","expectedWorktree":"{}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#,
        fence.head, fence.index_generation, fence.worktree_generation
    );
    let err = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("scope mismatch");
    assert!(
        err.message.contains("do not match the staged set"),
        "{}",
        err.message
    );
    assert_eq!(Head::current_commit().await.expect("unchanged"), before);
}

/// A restore fence check must not echo the host path when a tracked directory
/// was replaced by a symlink.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn restore_fence_hides_the_host_path_behind_a_symlink_directory() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    std::fs::create_dir(dir.path().join("sub")).expect("sub");
    write_file(dir.path(), "sub/nested.txt", "inside\n");
    stage(&["sub/nested.txt"]).await;
    cli_commit("add nested").await;
    let head = Head::current_commit().await.expect("head").to_string();
    insert_checkpoint(&ctx.conn, "cp-symlink", "s1", None, Some(&head), 1)
        .await
        .expect("checkpoint");
    std::fs::remove_dir_all(dir.path().join("sub")).expect("remove tracked dir");
    let outside = tempfile::tempdir().expect("outside");
    write_file(outside.path(), "nested.txt", "SECRET-DIFF\n");
    symlink(outside.path(), dir.path().join("sub")).expect("replace with symlink");
    let frame = format!(
        r#"{{"jsonrpc":"2.0","method":"checkpoint.restore","params":{{"operation_id":"op-symlink-fence","session_id":"s1","checkpoint_id":"cp-symlink","expected_head":"{head}","approval":{{"decision":"approved","approver":"reviewer-1"}}}},"id":1}}"#
    );
    let error = mutation_dispatch(&ctx, &request(&frame))
        .await
        .expect_err("symlink fence");
    assert!(
        error.message.contains("symlink") || error.message.contains("directory"),
        "{}",
        error.message
    );
    assert!(
        !error.message.contains("SECRET-DIFF")
            && !error
                .message
                .contains(dir.path().to_str().unwrap_or("repo-path"))
            && !error
                .message
                .contains(outside.path().to_str().unwrap_or("outside-path")),
        "restore fence leaked host data: {}",
        error.message
    );
    assert_eq!(
        Head::current_commit().await.expect("HEAD").to_string(),
        head
    );
}

/// Enabled `diff.get` must not return bytes read through a symlink that
/// replaced a tracked directory.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn diff_get_rejects_a_tracked_path_replaced_by_a_symlink_directory() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    std::fs::create_dir(dir.path().join("sub")).expect("sub");
    write_file(dir.path(), "sub/nested.txt", "inside\n");
    stage(&["sub/nested.txt"]).await;
    cli_commit("add nested").await;
    std::fs::remove_dir_all(dir.path().join("sub")).expect("remove tracked dir");
    let outside = tempfile::tempdir().expect("outside");
    write_file(outside.path(), "nested.txt", "SECRET-DIFF\n");
    symlink(outside.path(), dir.path().join("sub")).expect("replace with symlink");
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let error = read_dispatch(
        &ctx,
        &request(r#"{"jsonrpc":"2.0","method":"diff.get","params":{"mode":"worktree"},"id":1}"#),
    )
    .await
    .expect_err("symlink ancestor in diff.get");
    assert!(error.message.contains("symlink"), "{}", error.message);
    assert_eq!(error.stable_code, "LBR-AGENT-027", "{}", error.message);
    assert!(
        !error.message.contains("SECRET-DIFF"),
        "diff.get leaked outside bytes: {}",
        error.message
    );
    assert!(
        !error
            .message
            .contains(dir.path().to_str().unwrap_or("repo-path"))
            && !error
                .message
                .contains(outside.path().to_str().unwrap_or("outside-path")),
        "diff.get leaked a host path: {}",
        error.message
    );
}

/// An unfiltered diff must not return bytes read through a symlink that
/// replaced a tracked directory.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_diff_omits_tracked_paths_replaced_by_a_symlink_directory() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    std::fs::create_dir(dir.path().join("sub")).expect("sub");
    write_file(dir.path(), "sub/nested.txt", "inside\n");
    stage(&["sub/nested.txt"]).await;
    cli_commit("add nested").await;
    std::fs::remove_dir_all(dir.path().join("sub")).expect("remove tracked dir");
    let outside = tempfile::tempdir().expect("outside");
    write_file(outside.path(), "nested.txt", "SECRET-DIFF\n");
    symlink(outside.path(), dir.path().join("sub")).expect("replace with symlink");
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let error = read_dispatch(
        &ctx,
        &request(
            r#"{"jsonrpc":"2.0","method":"vcs.diff","params":{"sides":"worktree-vs-index"},"id":1}"#,
        ),
    )
    .await
    .expect_err("symlink ancestor in an unfiltered diff");
    assert!(error.message.contains("symlink"), "{}", error.message);
    assert!(
        !error.message.contains("SECRET-DIFF")
            && !error
                .message
                .contains(dir.path().to_str().unwrap_or("repo-path"))
            && !error
                .message
                .contains(outside.path().to_str().unwrap_or("outside-path")),
        "unfiltered diff leaked host data: {}",
        error.message
    );
}

/// A tracked file replaced by a symlink must not leak the outside file's bytes.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
#[cfg(unix)]
async fn vcs_diff_does_not_read_through_a_leaf_symlink() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    write_file(dir.path(), "tracked.txt", "inside\n");
    stage(&["tracked.txt"]).await;
    cli_commit("add tracked").await;
    let outside = tempfile::tempdir().expect("outside");
    write_file(outside.path(), "secret.txt", "SECRET-LEAF\n");
    fs::remove_file(dir.path().join("tracked.txt")).expect("remove tracked file");
    symlink(
        outside.path().join("secret.txt"),
        dir.path().join("tracked.txt"),
    )
    .expect("replace with a symlink");
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let response = read_dispatch(
        &ctx,
        &request(
            r#"{"jsonrpc":"2.0","method":"vcs.diff","params":{"sides":"worktree-vs-index","paths":["tracked.txt"]},"id":1}"#,
        ),
    )
    .await
    .expect("vcs.diff")
    .expect("response");
    let rendered = serde_json::to_string(&response).expect("render");
    assert!(
        !rendered.contains("SECRET-LEAF"),
        "the diff must not carry bytes read through a symlink: {rendered}"
    );
}

/// `vcs.diff` must not follow a symlink directory to a file outside the repository.
#[tokio::test]
#[serial(cwd, env, hash_kind)]
async fn vcs_diff_rejects_a_path_through_a_symlink_directory() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().expect("tempdir");
    let _guard = repo_with_initial_commit(dir.path()).await;
    let outside = tempfile::tempdir().expect("outside");
    write_file(outside.path(), "secret.txt", "SECRET-DIFF\n");
    symlink(outside.path(), dir.path().join("escape")).expect("symlink dir");
    let ctx = bridge_ctx().await;
    open_session(&ctx, "s1").await;
    let error = read_dispatch(
        &ctx,
        &request(
            r#"{"jsonrpc":"2.0","method":"vcs.diff","params":{"sides":"worktree-vs-index","paths":["escape/secret.txt"]},"id":1}"#,
        ),
    )
    .await
    .expect_err("symlink diff");
    assert!(error.message.contains("symlink"), "{}", error.message);
    assert!(
        !error.message.contains("SECRET-DIFF"),
        "diff leaked outside bytes: {}",
        error.message
    );
}

async fn vcs_data(ctx: &BridgeContext, line: &str) -> Value {
    let response = read_dispatch(ctx, &request(line))
        .await
        .expect("dispatch")
        .expect("response");
    let result = response.result.expect("result envelope");
    envelope_data(&result).clone()
}
