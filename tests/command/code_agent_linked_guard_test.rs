//! W4-08: linked-worktree enablement for `libra code` / `libra automation`.
//!
//! After the W4-06/W4-11/W4-12 resolver and W4-07/W4-13 approval ownership,
//! healthy linked worktrees launch through the resolver. Damaged/unreadable
//! scope still fail-closes. Layer: L1 (deterministic; tempdir, no network).

use std::fs;

use libra::internal::{
    ai::{
        hooks::{HookAction, HookRunner},
        permission::resolve_approval_runtime_cache,
        sandbox::{NetworkAccess, load_sandbox_config_network_access},
    },
    worktree_scope::RequestScope,
};

use super::{assert_cli_success, run_libra_command, spawn_libra_command_with_env};

/// A committed repo plus one linked worktree. Returns (repo_dir, wt_path).
fn repo_with_linked_worktree() -> (tempfile::TempDir, tempfile::TempDir) {
    let repo = tempfile::tempdir().expect("repo");
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["init", "--vault=false"], p), "init");
    assert_cli_success(&run_libra_command(&["config", "user.name", "t"], p), "name");
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "t@t"], p),
        "email",
    );
    fs::write(p.join("a.txt"), "a\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit",
    );
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], p),
        "worktree add",
    );
    (repo, parent)
}

/// Invalid `--port`/`--mcp-port` still fail, but the linked worktree must
/// reach mode validation (not the retired W0 preflight).
#[test]
fn code_linked_worktree_reaches_mode_validation() {
    let (repo, parent) = repo_with_linked_worktree();
    let wt = parent.path().join("wt");
    let invalid_modes = [
        "code",
        "--provider",
        "gemini",
        "--port",
        "5000",
        "--mcp-port",
        "5000",
    ];

    let linked = run_libra_command(&invalid_modes, &wt);
    assert!(
        !linked.status.success(),
        "invalid modes still refused in a linked worktree"
    );
    let linked_stderr = String::from_utf8_lossy(&linked.stderr);
    assert!(
        linked_stderr.contains("must be different"),
        "linked worktree must reach mode validation after W4-08, got: {linked_stderr}"
    );
    assert!(
        !linked_stderr.contains("cannot run in a linked worktree"),
        "W0 preflight must stay retired: {linked_stderr}"
    );

    let main = run_libra_command(&invalid_modes, repo.path());
    assert!(
        !main.status.success(),
        "invalid modes still refused in main"
    );
    let main_stderr = String::from_utf8_lossy(&main.stderr);
    assert!(
        main_stderr.contains("must be different"),
        "main must still reach mode validation: {main_stderr}"
    );
}

/// `--cwd` into a linked worktree from main also reaches mode validation.
#[test]
fn code_cwd_into_linked_worktree_reaches_mode_validation() {
    let (repo, parent) = repo_with_linked_worktree();
    let wt = parent.path().join("wt");
    let wt_str = wt.to_str().unwrap();

    let from_main = run_libra_command(
        &[
            "code",
            "--provider",
            "gemini",
            "--cwd",
            wt_str,
            "--port",
            "5000",
            "--mcp-port",
            "5000",
        ],
        repo.path(),
    );
    assert!(!from_main.status.success(), "invalid modes still refused");
    let stderr = String::from_utf8_lossy(&from_main.stderr);
    assert!(
        stderr.contains("must be different"),
        "--cwd into a linked worktree must reach mode validation: {stderr}"
    );
    assert!(
        !stderr.contains("cannot run in a linked worktree"),
        "W0 preflight must stay retired for --cwd: {stderr}"
    );
}

/// `libra automation` subcommands work from a linked worktree after W4-08.
#[test]
fn automation_cli_runs_in_linked_worktree() {
    let (_repo, parent) = repo_with_linked_worktree();
    let wt = parent.path().join("wt");

    let list = run_libra_command(&["automation", "list"], &wt);
    assert_cli_success(&list, "automation list in linked worktree");
    let history = run_libra_command(&["automation", "history"], &wt);
    assert_cli_success(&history, "automation history in linked worktree");
}

/// Healthy linked commits no longer warn that automation dispatch is disabled.
#[test]
fn linked_commit_does_not_warn_about_disabled_automation_dispatch() {
    let (_repo, parent) = repo_with_linked_worktree();
    let wt = parent.path().join("wt");

    fs::write(wt.join("b.txt"), "b\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b.txt"], &wt), "add in wt");
    let commit = run_libra_command(&["commit", "-m", "wt commit", "--no-verify"], &wt);
    assert_cli_success(&commit, "commit in linked worktree");

    let stderr = String::from_utf8_lossy(&commit.stderr);
    assert!(
        !stderr.contains("automation dispatch is disabled"),
        "W4-08 must restore linked automation dispatch without the W4-12 warning: {stderr}"
    );
}

/// Main-worktree commits stay quiet about linked-scope dispatch.
#[test]
fn main_commit_does_not_warn_about_automation_dispatch() {
    let (repo, _parent) = repo_with_linked_worktree();
    fs::write(repo.path().join("c.txt"), "c\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "c.txt"], repo.path()),
        "add in main",
    );
    let commit = run_libra_command(&["commit", "-m", "main commit", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit in main");
    let stderr = String::from_utf8_lossy(&commit.stderr);
    assert!(
        !stderr.contains("automation dispatch is disabled"),
        "main-worktree commits must not carry the linked-scope warning: {stderr}"
    );
}

/// Repository PreToolUse Block is effective when the runner is bound to a
/// linked worktree (plan-20260714 §C.12 / W4-08 named regression).
#[tokio::test]
async fn pretooluse_block_effective_in_linked_worktree() {
    let (repo, parent) = repo_with_linked_worktree();
    let main = repo.path();
    let wt = parent.path().join("wt");
    fs::write(
        main.join(".libra").join("hooks.json"),
        r#"{"hooks":[{"event":"pre_tool_use","matcher":"shell","command":"exit 129"}]}"#,
    )
    .expect("repo PreToolUse block");

    let runner = HookRunner::load(&wt).expect("load hooks from linked worktree");
    let action = runner
        .run_pre_tool_use("shell", serde_json::json!({"command": "echo hi"}))
        .await;
    assert!(
        matches!(action, HookAction::Block(_)),
        "repository PreToolUse must block in a linked worktree, got: {action:?}"
    );
}

/// post-commit automation in a linked worktree runs via the resolver
/// (repository automations.toml visible; history recorded).
#[test]
fn linked_post_commit_automation_runs_via_resolver_after_w4() {
    let (repo, parent) = repo_with_linked_worktree();
    let main = repo.path();
    let wt = parent.path().join("wt");
    fs::write(
        main.join(".libra").join("automations.toml"),
        r#"
[[rules]]
id = "linked_post_commit"
trigger = { kind = "vcs", event = "post_commit" }
action = { kind = "prompt", prompt = "summarize linked commit" }
"#,
    )
    .expect("repository automations.toml");

    fs::write(wt.join("d.txt"), "d\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "d.txt"], &wt), "add in wt");
    let commit = run_libra_command(&["commit", "-m", "linked automation", "--no-verify"], &wt);
    assert_cli_success(&commit, "commit in linked worktree");
    let commit_stderr = String::from_utf8_lossy(&commit.stderr);
    assert!(
        !commit_stderr.contains("automation dispatch is disabled"),
        "linked post-commit must dispatch, not warn-disable: {commit_stderr}"
    );

    let history = run_libra_command(&["automation", "history", "--limit", "20"], &wt);
    assert_cli_success(&history, "automation history from linked worktree");
    let stdout = String::from_utf8_lossy(&history.stdout);
    assert!(
        stdout.contains("linked_post_commit"),
        "linked post-commit must record history via resolver: {stdout}"
    );
}

/// Post-W4 joint regression: linked `libra code` launch path, repo-keyed
/// approval cache, sandbox/hook resolver, and damaged-scope fail-closed.
#[tokio::test]
async fn linked_code_runtime_runs_via_resolver_after_enablement() {
    let (repo, parent) = repo_with_linked_worktree();
    let main = repo.path();
    let wt = parent.path().join("wt");

    fs::write(
        main.join(".libra").join("sandbox.toml"),
        "[sandbox.network]\nmode = \"denied\"\n",
    )
    .expect("repository sandbox");
    fs::write(
        main.join(".libra").join("hooks.json"),
        r#"{"hooks":[{"event":"pre_tool_use","matcher":"shell","command":"echo repo-block"}]}"#,
    )
    .expect("repository hooks");

    let invalid_modes = [
        "code",
        "--provider",
        "gemini",
        "--port",
        "5000",
        "--mcp-port",
        "5000",
    ];
    let linked_launch = run_libra_command(&invalid_modes, &wt);
    assert!(
        !linked_launch.status.success(),
        "invalid modes still refused after enablement"
    );
    let launch_stderr = String::from_utf8_lossy(&linked_launch.stderr);
    assert!(
        launch_stderr.contains("must be different"),
        "linked libra code must start far enough to validate modes: {launch_stderr}"
    );
    assert!(
        !launch_stderr.contains("cannot run in a linked worktree"),
        "preflight must stay lifted: {launch_stderr}"
    );

    // Default Web runtime (not deprecated MCP `--stdio`): linked worktree
    // must bind and serve `/api/health`.
    let child = spawn_libra_command_with_env(
        &["code", "--port", "0", "--mcp-port", "0"],
        &wt,
        &[("GEMINI_API_KEY", "test-gemini-api-key")],
    );
    struct KillChildOnDrop(Option<std::process::Child>);
    impl Drop for KillChildOnDrop {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let mut child_guard = KillChildOnDrop(Some(child));
    let stdout = child_guard
        .0
        .as_mut()
        .expect("child")
        .stdout
        .take()
        .expect("stdout");
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let mut reader = BufReader::new(stdout);
        let mut captured = String::new();
        let mut line = String::new();
        let mut notified = false;
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    captured.push_str(&line);
                    if !notified
                        && let Some(rest) =
                            line.trim().strip_prefix("Libra Code server running at ")
                    {
                        notified = true;
                        let _ = tx.send(rest.trim_end_matches('/').to_string());
                    }
                }
                Err(_) => break,
            }
        }
        let _ = done_tx.send(captured);
    });
    let printed_url = match rx.recv_timeout(std::time::Duration::from_secs(45)) {
        Ok(url) => url,
        Err(_) => {
            let mut failed = child_guard.0.take().expect("child");
            let _ = failed.kill();
            let _ = failed.wait();
            let err = failed.stderr.take().map(|mut stderr| {
                use std::io::Read;
                let mut buf = String::new();
                let _ = stderr.read_to_string(&mut buf);
                buf
            });
            let captured = done_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap_or_default();
            panic!(
                "linked default Web did not print a bind URL; stdout={captured}; stderr={}",
                err.unwrap_or_default()
            );
        }
    };
    assert!(
        printed_url.starts_with("http://127.0.0.1:") || printed_url.starts_with("http://[::1]:"),
        "linked default Web must bind loopback; got {printed_url}"
    );
    let web_origin = printed_url
        .split_once('?')
        .map(|(origin, _)| origin)
        .unwrap_or(printed_url.as_str());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .no_proxy()
        .build()
        .expect("http client");
    let health_url = format!("{web_origin}/api/health");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
    loop {
        if std::time::Instant::now() > deadline {
            let mut failed = child_guard.0.take().expect("child");
            let _ = failed.kill();
            let _ = failed.wait();
            panic!("linked default Web UI did not become healthy at {health_url}");
        }
        if let Ok(resp) = client.get(&health_url).send().await
            && resp.status().is_success()
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let mut child = child_guard.0.take().expect("child");
    let _ = child.kill();
    let _ = child.wait();

    let network = load_sandbox_config_network_access(&wt).expect("linked sandbox via resolver");
    assert_eq!(
        network,
        Some(NetworkAccess::Denied),
        "linked sandbox must keep the repository deny baseline"
    );

    let hooks = HookRunner::load(&wt).expect("linked hooks via resolver");
    assert!(
        hooks.has_hooks(),
        "linked worktree must see repository hooks"
    );

    let main_cache = resolve_approval_runtime_cache(main)
        .await
        .expect("main approval cache");
    let linked_cache = resolve_approval_runtime_cache(&wt)
        .await
        .expect("linked approval cache");
    assert_eq!(
        main_cache.repo_id, linked_cache.repo_id,
        "Always cache must share the canonical repo_id across worktrees"
    );
    assert_eq!(
        main_cache.scope_key, linked_cache.scope_key,
        "ApprovalStore scope must be repo-keyed, not worktree-keyed"
    );
    assert!(
        linked_cache.scope_key.starts_with("repo:"),
        "cache key must be repo:{{libra.repoid}}, got {}",
        linked_cache.scope_key
    );

    fs::write(
        wt.join(".libra").join("commondir"),
        parent.path().join("gone").to_string_lossy().as_bytes(),
    )
    .expect("corrupt linked commondir");
    let damaged = RequestScope::try_resolve(wt.clone());
    assert!(
        damaged.is_err(),
        "damaged linked worktree must fail-closed: {damaged:?}"
    );
    let damaged_cache = resolve_approval_runtime_cache(&wt).await;
    assert!(
        damaged_cache.is_err(),
        "approval cache must fail-closed on damaged scope: {damaged_cache:?}"
    );
}

/// A lockless v1 `worktrees.json` still lets a healthy linked worktree run
/// after W4-08 (ids are matched via gitdir/path, not reported as unregistered).
#[test]
fn v1_registry_linked_worktree_still_runs_after_w4() {
    let (repo, parent) = repo_with_linked_worktree();
    let wt = parent.path().join("wt");
    let registry = repo.path().join(".libra").join("worktrees.json");
    let v2: serde_json::Value =
        serde_json::from_slice(&fs::read(&registry).expect("read registry")).expect("v2 json");
    let v1_entries: Vec<serde_json::Value> = v2["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| {
            serde_json::json!({
                "path": entry["path"],
                "is_main": entry["is_main"],
                "locked": entry["locked"],
                "lock_reason": entry["lock_reason"],
            })
        })
        .collect();
    fs::write(
        &registry,
        serde_json::to_vec_pretty(&serde_json::json!({ "worktrees": v1_entries }))
            .expect("serialize v1"),
    )
    .expect("write v1 registry");

    let list = run_libra_command(&["automation", "list"], &wt);
    assert_cli_success(&list, "automation list on v1 registry linked worktree");

    let invalid_modes = [
        "code",
        "--provider",
        "gemini",
        "--port",
        "5000",
        "--mcp-port",
        "5000",
    ];
    let linked = run_libra_command(&invalid_modes, &wt);
    let stderr = String::from_utf8_lossy(&linked.stderr);
    assert!(
        stderr.contains("must be different"),
        "v1-registry linked libra code must reach mode validation, not unregistered refusal: {stderr}"
    );
    assert!(
        !stderr.contains("not in the worktree registry"),
        "healthy v1 linked worktree must not be reported as unregistered: {stderr}"
    );
}

/// Keep-dir `worktree remove` (detached) must not count as registered.
#[test]
fn detached_linked_worktree_fail_closed_after_w4() {
    let (repo, parent) = repo_with_linked_worktree();
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "remove", wt.to_str().unwrap()], repo.path()),
        "worktree remove keep-dir",
    );

    let list = run_libra_command(&["automation", "list"], &wt);
    assert!(
        !list.status.success(),
        "automation must refuse a detached linked worktree"
    );

    let code = run_libra_command(
        &["code", "--cwd", wt.to_str().unwrap(), "--stdio"],
        repo.path(),
    );
    assert!(
        !code.status.success(),
        "libra code must refuse a detached linked worktree"
    );
}

/// Copying an active linked `worktree_id` into an unregistered directory
/// that only points `commondir` at the repo must still fail-close.
#[test]
fn copied_linked_worktree_id_fail_closed_after_w4() {
    let (repo, parent) = repo_with_linked_worktree();
    let wt = parent.path().join("wt");
    let stolen_id = fs::read_to_string(wt.join(".libra").join("worktree_id"))
        .expect("read registered worktree_id");
    let forge_parent = tempfile::tempdir().expect("forge parent");
    let forge = forge_parent.path().join("forged");
    fs::create_dir_all(forge.join(".libra")).expect("forge gitdir");
    fs::write(
        forge.join(".libra").join("commondir"),
        format!("{}\n", repo.path().join(".libra").display()),
    )
    .expect("forge commondir");
    fs::write(forge.join(".libra").join("worktree_id"), stolen_id.trim())
        .expect("copy registered worktree_id");

    let list = run_libra_command(&["automation", "list"], &forge);
    assert!(
        !list.status.success(),
        "automation must refuse a copied-id forged worktree"
    );
    let list_stderr = String::from_utf8_lossy(&list.stderr);
    assert!(
        list_stderr.contains("not in the worktree registry")
            || list_stderr.contains("could not be resolved"),
        "copied-id automation refusal must name the identity failure: {list_stderr}"
    );

    let from_main = run_libra_command(
        &["code", "--cwd", forge.to_str().unwrap(), "--stdio"],
        repo.path(),
    );
    assert!(
        !from_main.status.success(),
        "libra code must refuse a copied-id forged directory"
    );
    let code_stderr = String::from_utf8_lossy(&from_main.stderr);
    assert!(
        code_stderr.contains("not in the worktree registry")
            || code_stderr.contains("could not be resolved"),
        "copied-id code refusal must name the identity failure: {code_stderr}"
    );
}

/// Unregistered / synthesized linked identity still fail-closes after W4-08.
#[test]
fn unregistered_linked_worktree_fail_closed_after_w4() {
    let (repo, parent) = repo_with_linked_worktree();
    let wt = parent.path().join("wt");
    fs::write(wt.join(".libra").join("worktree_id"), "not-in-registry\n")
        .expect("forge unregistered worktree_id");

    let list = run_libra_command(&["automation", "list"], &wt);
    assert!(
        !list.status.success(),
        "automation must refuse an unregistered linked worktree"
    );
    let list_stderr = String::from_utf8_lossy(&list.stderr);
    assert!(
        list_stderr.contains("not in the worktree registry")
            || list_stderr.contains("could not be resolved"),
        "unregistered automation refusal must name the identity failure: {list_stderr}"
    );

    let from_main = run_libra_command(
        &["code", "--cwd", wt.to_str().unwrap(), "--stdio"],
        repo.path(),
    );
    assert!(
        !from_main.status.success(),
        "libra code --cwd into an unregistered linked worktree must refuse"
    );
    let code_stderr = String::from_utf8_lossy(&from_main.stderr);
    assert!(
        code_stderr.contains("not in the worktree registry")
            || code_stderr.contains("could not be resolved"),
        "unregistered code refusal must name the identity failure: {code_stderr}"
    );
}
