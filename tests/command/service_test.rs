//! Integration tests for `libra service` (lore.md §1.11): loopback-only
//! headless service, notification v1, token-gated dirty-mark ingestion, and
//! the §7.10 kill-9 fault row (marks persist; restart reclaims the lock).
//!
//! **Layer:** L1 — deterministic, loopback networking only.

use std::process::{Child, Stdio};

use super::*;

fn repository_id(main: &Path) -> String {
    let out = run_libra_command(&["config", "get", "libra.repoid"], main);
    assert_cli_success(&out, "config get libra.repoid");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The registration generation `worktree list` reports for `worktree_id` —
/// the fence a dirty-mark request must carry.
fn registered_epoch(main: &Path, worktree_id: &str) -> u64 {
    let listed = run_libra_command(&["--json", "worktree", "list"], main);
    assert_cli_success(&listed, "worktree list");
    let json: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("list json");
    json["data"]["worktrees"]
        .as_array()
        .expect("worktrees array")
        .iter()
        .find(|entry| entry["worktree_id"] == worktree_id)
        .and_then(|entry| entry["epoch"].as_u64())
        .unwrap_or_else(|| panic!("no epoch for '{worktree_id}' in {json}"))
}

fn service_repo() -> tempfile::TempDir {
    create_committed_repo_via_cli()
}

struct ServiceGuard(Child);

impl Drop for ServiceGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Spawn `libra service run --port 0` and wait for service.json + a live
/// health endpoint. Returns (guard, base_url, token).
fn spawn_service(p: &Path) -> (ServiceGuard, String, String) {
    spawn_service_with_fault(p, None)
}

/// [`spawn_service`] with an optional `LIBRA_TEST_FAULT` site, so the
/// handler's error branches can be reached from a test (every fixture
/// otherwise has a healthy database, which is how a swallowed error
/// survived unnoticed).
fn spawn_service_with_fault(p: &Path, fault: Option<&str>) -> (ServiceGuard, String, String) {
    spawn_service_with_env(p, fault, &[])
}

fn spawn_service_with_env(
    p: &Path,
    fault: Option<&str>,
    env: &[(&str, String)],
) -> (ServiceGuard, String, String) {
    let mut command = base_libra_command(&["service", "run", "--port", "0"], p);
    if let Some(site) = fault {
        command.env("LIBRA_TEST_FAULT", site);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn service");
    let guard = ServiceGuard(child);
    let info_path = p.join(".libra/service/service.json");
    let token_path = p.join(".libra/service/service-token");
    let client = reqwest::blocking::Client::new();
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let Ok(text) = fs::read_to_string(&info_path) else {
            continue;
        };
        let Ok(info) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(base_url) = info["baseUrl"].as_str() else {
            continue;
        };
        if let Ok(response) = client.get(format!("{base_url}/api/health")).send()
            && response.status().is_success()
        {
            let token = fs::read_to_string(&token_path).expect("token file");
            return (guard, base_url.to_string(), token.trim().to_string());
        }
    }
    let mut guard = guard;
    let _ = guard.0.kill();
    let _ = guard.0.wait();
    let mut stdout_text = String::new();
    let mut stderr_text = String::new();
    use std::io::Read;
    if let Some(mut out) = guard.0.stdout.take() {
        let _ = out.read_to_string(&mut stdout_text);
    }
    if let Some(mut err) = guard.0.stderr.take() {
        let _ = err.read_to_string(&mut stderr_text);
    }
    panic!("service did not come up\nstdout: {stdout_text}\nstderr: {stderr_text}");
}

#[test]
fn service_rejects_non_loopback_hosts_and_outside_repo() {
    let repo = service_repo();
    let p = repo.path();
    for bad in ["0.0.0.0", "192.168.1.10", "localhost"] {
        let out = run_libra_command(&["service", "run", "--host", bad], p);
        assert_eq!(out.status.code(), Some(129), "{bad} must be refused");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("loopback"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let outside = tempfile::tempdir().unwrap();
    let out = run_libra_command(&["service", "status"], outside.path());
    assert_eq!(out.status.code(), Some(128), "outside a repo");
}

#[test]
#[serial]
fn service_end_to_end_events_marks_and_fault_recovery() {
    let repo = service_repo();
    let p = repo.path();
    let (guard, base_url, token) = spawn_service(p);
    let client = reqwest::blocking::Client::new();

    // status sees the live instance.
    let status = run_libra_command(&["--json", "service", "status"], p);
    assert_cli_success(&status, "service status");
    let json = parse_json_stdout(&status);
    assert_eq!(json["data"]["running"].as_bool(), Some(true));
    assert_eq!(json["data"]["health"].as_str(), Some("ok"));

    // The event stream requires the token (fail-closed, SSE included).
    let refused = client
        .get(format!("{base_url}/api/service/events"))
        .send()
        .expect("request");
    assert_eq!(refused.status().as_u16(), 401, "events without token");

    // Subscribe with the token, then publish a mark and a custom notification.
    let mut events = client
        .get(format!("{base_url}/api/service/events"))
        .header("x-libra-service-token", token.clone())
        .send()
        .expect("subscribe");
    assert!(events.status().is_success());

    // Mark endpoint: no token → 401; bad path → 400 whole-batch; good → 200.
    let unauth = client
        .post(format!("{base_url}/api/service/dirty/mark"))
        .json(&serde_json::json!({ "paths": ["a.txt"] }))
        .send()
        .expect("request");
    assert_eq!(unauth.status().as_u16(), 401);
    let escape = client
        .post(format!("{base_url}/api/service/dirty/mark"))
        .header("x-libra-service-token", token.clone())
        .json(&serde_json::json!({ "paths": ["ok.txt", "../evil"] }))
        .send()
        .expect("request");
    assert_eq!(escape.status().as_u16(), 400, "escaping path refused");
    let marked = client
        .post(format!("{base_url}/api/service/dirty/mark"))
        .header("x-libra-service-token", token.clone())
        .json(&serde_json::json!({ "paths": ["svc.txt"] }))
        .send()
        .expect("request");
    assert_eq!(marked.status().as_u16(), 200);
    let notify = client
        .post(format!("{base_url}/api/service/notify"))
        .header("x-libra-service-token", token.clone())
        .json(&serde_json::json!({ "type": "custom", "data": {"k": "v"} }))
        .send()
        .expect("request");
    assert_eq!(notify.status().as_u16(), 200);

    // The SSE stream carries both events.
    use std::io::Read;
    let mut buffer = String::new();
    let mut chunk = [0u8; 4096];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline
        && !(buffer.contains("dirty_marked") && buffer.contains("custom"))
    {
        match events.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buffer.push_str(&String::from_utf8_lossy(&chunk[..n])),
            Err(_) => break,
        }
    }
    assert!(
        buffer.contains("dirty_marked") && buffer.contains("svc.txt"),
        "mark event delivered: {buffer}"
    );
    assert!(
        buffer.contains("custom"),
        "notify event delivered: {buffer}"
    );

    // The mark is DURABLE (SQLite), visible via libra dirty --list.
    let list = run_libra_command(&["--json", "dirty", "--list"], p);
    let json = parse_json_stdout(&list);
    assert!(
        json["data"]["entries"]
            .as_array()
            .is_some_and(|a| a.iter().any(|e| e["path"] == "svc.txt")),
        "service mark persisted: {json}"
    );

    // §7.10 fault row: kill -9, the mark survives, a restart reclaims the
    // lock and comes up cleanly.
    drop(guard); // SIGKILL
    std::thread::sleep(std::time::Duration::from_millis(300));
    let list = run_libra_command(&["--json", "dirty", "--list"], p);
    let json = parse_json_stdout(&list);
    assert!(
        json["data"]["entries"]
            .as_array()
            .is_some_and(|a| a.iter().any(|e| e["path"] == "svc.txt")),
        "mark survives kill -9: {json}"
    );
    let (guard2, base_url2, _token2) = spawn_service(p);
    assert!(!base_url2.is_empty(), "restart reclaimed the stale lock");
    drop(guard2);
    // status after shutdown reports not running (dead pid → stale) and exits 1.
    std::thread::sleep(std::time::Duration::from_millis(300));
    let status = run_libra_command(&["service", "status"], p);
    assert_eq!(status.status.code(), Some(1), "stopped service exits 1");
}

/// W1 §C.4.1.1: scope-less dirty-mark requests are rejected (409) in a
/// multi-worktree repository — the dirty cache is per-worktree and the
/// caller's scope is unknown. A corrupt registry rejects too (fail closed).
#[test]
#[serial]
fn dirty_mark_rejected_in_multi_worktree_repo() {
    let repo = service_repo();
    let main = repo.path();
    let wt_root = tempdir().expect("wt root");
    let wt = wt_root.path().join("svc-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let (_guard, base_url, token) = spawn_service(main);
    let client = reqwest::blocking::Client::new();
    let refused = client
        .post(format!("{base_url}/api/service/dirty/mark"))
        .header("x-libra-service-token", token.clone())
        .json(&serde_json::json!({ "paths": ["f.txt"] }))
        .send()
        .expect("request");
    assert_eq!(
        refused.status().as_u16(),
        409,
        "multi-worktree scope-less mark is refused"
    );
    let body = refused.text().unwrap_or_default();
    assert!(body.contains("worktree"), "actionable message: {body}");

    // Corrupt registry: fail closed too.
    fs::write(main.join(".libra/worktrees.json"), "{not json").expect("corrupt registry");
    let corrupt = client
        .post(format!("{base_url}/api/service/dirty/mark"))
        .header("x-libra-service-token", token.clone())
        .json(&serde_json::json!({ "paths": ["f.txt"] }))
        .send()
        .expect("request");
    assert_eq!(
        corrupt.status().as_u16(),
        409,
        "corrupt registry fails closed"
    );

    // Schema-corrupt registry (valid JSON, but the entry is missing the
    // required `is_main`/`locked` fields the real schema persists): the
    // parser mirrors the persisted `WorktreeState`, so this fails closed
    // too instead of counting the entry as a lone main worktree.
    fs::write(
        main.join(".libra/worktrees.json"),
        r#"{"worktrees":[{"path":"/repo"}]}"#,
    )
    .expect("schema-corrupt registry");
    let schema_corrupt = client
        .post(format!("{base_url}/api/service/dirty/mark"))
        .header("x-libra-service-token", token.clone())
        .json(&serde_json::json!({ "paths": ["f.txt"] }))
        .send()
        .expect("request");
    assert_eq!(
        schema_corrupt.status().as_u16(),
        409,
        "schema-corrupt registry (missing required fields) fails closed"
    );

    // Deserializable-but-corrupt shapes: an empty worktree list and a sole
    // non-main entry both lack the main entry the real loader requires —
    // neither is a validated single-main state, so both fail closed.
    for (shape, label) in [
        (r#"{"worktrees":[]}"#, "empty worktree list"),
        (
            r#"{"worktrees":[{"path":"/repo","is_main":false,"locked":false,"lock_reason":null}]}"#,
            "sole non-main entry",
        ),
    ] {
        fs::write(main.join(".libra/worktrees.json"), shape).expect("corrupt-shape registry");
        let refused = client
            .post(format!("{base_url}/api/service/dirty/mark"))
            .header("x-libra-service-token", token.clone())
            .json(&serde_json::json!({ "paths": ["f.txt"] }))
            .send()
            .expect("request");
        assert_eq!(refused.status().as_u16(), 409, "{label} fails closed");
    }
}

/// §C.11 W1 / §C.12 named regression `linked_service_dirty_mark_is_scoped`:
/// a scope-carrying dirty-mark lands in the worktree the CALLER names, not in
/// the service process's own scope — and an unknown scope is refused rather
/// than silently creating rows no worktree will ever read.
///
/// The dirty cache is per-worktree, so a service that marked its own scope
/// would tell the wrong worktree its files changed while leaving the real one
/// stale — the reason a scope-less request is still refused here.
#[test]
fn linked_service_dirty_mark_is_scoped() {
    let repo = service_repo();
    let main = repo.path();
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("svc-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let worktree_id = std::fs::read_to_string(wt.join(".libra").join("worktree_id"))
        .expect("the linked worktree records its id")
        .trim()
        .to_string();
    let epoch = registered_epoch(main, &worktree_id);
    let repo_id = repository_id(main);
    std::fs::write(wt.join("scoped.txt"), "scoped\n").expect("write");

    let (_guard, base_url, token) = spawn_service(main);
    let client = reqwest::blocking::Client::new();

    // Scope-LESS in a multi-worktree repository: still refused.
    let ambiguous = client
        .post(format!("{base_url}/api/service/dirty/mark"))
        .header("x-libra-service-token", token.clone())
        .json(&serde_json::json!({ "paths": ["scoped.txt"] }))
        .send()
        .expect("request");
    assert_eq!(
        ambiguous.status().as_u16(),
        409,
        "a scope-less mark stays refused once a linked worktree exists"
    );

    // An unknown scope is refused rather than marked into nowhere.
    let unknown = client
        .post(format!("{base_url}/api/service/dirty/mark"))
        .header("x-libra-service-token", token.clone())
        .json(&serde_json::json!({
            "paths": ["scoped.txt"],
            "scope": {
                "kind": "linked",
                "repo_id": repo_id,
                "worktree_id": "wt-does-not-exist",
                "workdir": wt.to_string_lossy(),
                "epoch": epoch,
            },
        }))
        .send()
        .expect("request");
    assert_eq!(unknown.status().as_u16(), 409, "unknown scope refused");

    // The named scope is accepted...
    let scoped = client
        .post(format!("{base_url}/api/service/dirty/mark"))
        .header("x-libra-service-token", token.clone())
        .json(&serde_json::json!({
            "paths": ["scoped.txt"],
            "scope": {
                "kind": "linked",
                "repo_id": repo_id,
                "worktree_id": worktree_id,
                "workdir": wt.to_string_lossy(),
                "epoch": epoch,
            },
        }))
        .send()
        .expect("request");
    assert_eq!(
        scoped.status().as_u16(),
        200,
        "a scope-carrying mark is accepted: {}",
        scoped.text().unwrap_or_default()
    );

    // ...and the mark is visible in THAT worktree and nowhere else.
    let linked_cached = run_libra_command(&["--json", "dirty", "--list"], &wt);
    assert_cli_success(&linked_cached, "dirty --list in the linked worktree");
    let linked_body = String::from_utf8_lossy(&linked_cached.stdout);
    assert!(
        linked_body.contains("scoped.txt"),
        "the mark landed in the named worktree: {linked_body}"
    );
    let main_cached = run_libra_command(&["--json", "dirty", "--list"], main);
    assert_cli_success(&main_cached, "dirty --list in main");
    let main_body = String::from_utf8_lossy(&main_cached.stdout);
    assert!(
        !main_body.contains("scoped.txt"),
        "and main's dirty cache did not gain it: {main_body}"
    );

    // CONCURRENT requests for different scopes must not cross. A scope held as
    // process state across an await would let the later request's scope answer
    // for the earlier one's mark.
    std::fs::write(main.join("main-only.txt"), "main\n").expect("write");
    std::fs::write(wt.join("linked-only.txt"), "linked\n").expect("write");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut handles = Vec::new();
    for (scope, path) in [
        (
            serde_json::json!({"kind": "main", "repo_id": repo_id}),
            "main-only.txt",
        ),
        (
            serde_json::json!({
                "kind": "linked",
                "repo_id": repo_id,
                "worktree_id": worktree_id,
                "workdir": wt.to_string_lossy(),
                "epoch": epoch,
            }),
            "linked-only.txt",
        ),
    ] {
        let base_url = base_url.clone();
        let token = token.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let client = reqwest::blocking::Client::new();
            barrier.wait();
            client
                .post(format!("{base_url}/api/service/dirty/mark"))
                .header("x-libra-service-token", token)
                .json(&serde_json::json!({ "paths": [path], "scope": scope }))
                .send()
                .expect("request")
                .status()
                .as_u16()
        }));
    }
    for handle in handles {
        assert_eq!(handle.join().expect("thread"), 200, "concurrent mark");
    }

    let linked_after = run_libra_command(&["--json", "dirty", "--list"], &wt);
    assert_cli_success(&linked_after, "dirty --list in the linked worktree");
    let linked_after = String::from_utf8_lossy(&linked_after.stdout);
    let main_after = run_libra_command(&["--json", "dirty", "--list"], main);
    assert_cli_success(&main_after, "dirty --list in main");
    let main_after = String::from_utf8_lossy(&main_after.stdout);
    assert!(
        linked_after.contains("linked-only.txt") && !linked_after.contains("main-only.txt"),
        "the linked worktree got only its own mark: {linked_after}"
    );
    assert!(
        main_after.contains("main-only.txt") && !main_after.contains("linked-only.txt"),
        "and main got only its own: {main_after}"
    );
}

// ---------------------------------------------------------------------------
// §C.12 named regressions for the worktree-scoped advisory stores (W1).
//
// The dirty cache, the layer registry and the sparse view are all per-worktree
// state that used to be repository-global. Each name below is one the plan
// requires by name; each asserts the isolation directly rather than through an
// umbrella test, so a regression names the store it broke.
// ---------------------------------------------------------------------------

/// A repo with `feature`, plus a linked worktree on its own branch. Returns
/// `(repo, wt_parent, wt_path)` — both tempdirs must outlive the test.
fn repo_with_linked(branch: &str) -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
    let repo = create_committed_repo_via_cli();
    let main = repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], main),
        "feature branch",
    );
    let parent = tempfile::tempdir().expect("wt parent");
    assert_cli_success(
        &run_libra_command(&["branch", branch, "feature"], main),
        "branch for the worktree",
    );
    let wt = parent.path().join(branch);
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap(), branch], main),
        "worktree add",
    );
    (repo, parent, wt)
}

/// §C.12 `linked_check_dirty_does_not_prune_other_scope`: verifying one
/// worktree's cached dirty set must not delete another's rows. The cache is
/// advisory and over-reports; pruning across scopes would make a worktree
/// silently under-report the files it has actually changed.
#[test]
fn linked_check_dirty_does_not_prune_other_scope() {
    let (repo, _parent, wt) = repo_with_linked("dirty-wt");
    let main = repo.path();

    // Each worktree marks a path that exists ONLY in its own tree.
    fs::write(main.join("main-dirty.txt"), "main\n").unwrap();
    fs::write(wt.join("linked-dirty.txt"), "linked\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["dirty", "main-dirty.txt"], main),
        "mark in main",
    );
    assert_cli_success(
        &run_libra_command(&["dirty", "linked-dirty.txt"], &wt),
        "mark in the linked worktree",
    );

    // `--check-dirty` in main verifies and prunes MAIN's cache. The linked
    // worktree's rows must survive — its path does not even exist here.
    assert_cli_success(
        &run_libra_command(&["status", "--check-dirty"], main),
        "check-dirty in main",
    );

    let linked = run_libra_command(&["dirty", "--list"], &wt);
    assert_cli_success(&linked, "dirty --list in the linked worktree");
    let linked_body = String::from_utf8_lossy(&linked.stdout);
    assert!(
        linked_body.contains("linked-dirty.txt"),
        "main's check-dirty must not prune the linked worktree's rows: {linked_body}"
    );
}

/// §C.12 `service_dirty_mark_scope_mismatch_rejected`: a scope the registry
/// does not agree with is refused, not marked. Two mismatches are covered —
/// an id nothing is registered under, and a registered id paired with the
/// wrong path (ids are path-derived and reused, so the path is what catches a
/// client that has drifted onto a different worktree).
#[test]
fn service_dirty_mark_scope_mismatch_rejected() {
    let (repo, _parent, wt) = repo_with_linked("mismatch-wt");
    let main = repo.path();
    let worktree_id = fs::read_to_string(wt.join(".libra").join("worktree_id"))
        .expect("the linked worktree records its id")
        .trim()
        .to_string();
    let epoch = registered_epoch(main, &worktree_id);
    let repo_id = repository_id(main);
    fs::write(wt.join("m.txt"), "m\n").unwrap();

    let (_guard, base_url, token) = spawn_service(main);
    let client = reqwest::blocking::Client::new();

    for (label, scope) in [
        (
            "an unregistered id",
            serde_json::json!({
                "kind": "linked",
                "repo_id": repo_id,
                "worktree_id": "wt-not-registered",
                "workdir": wt.to_string_lossy(),
                "epoch": epoch,
            }),
        ),
        (
            "a registered id at the wrong path",
            serde_json::json!({
                "kind": "linked",
                "repo_id": repo_id,
                "worktree_id": worktree_id,
                "workdir": main.to_string_lossy(),
                "epoch": epoch,
            }),
        ),
    ] {
        let response = client
            .post(format!("{base_url}/api/service/dirty/mark"))
            .header("x-libra-service-token", token.clone())
            .json(&serde_json::json!({ "paths": ["m.txt"], "scope": scope }))
            .send()
            .expect("request");
        assert_eq!(
            response.status().as_u16(),
            409,
            "{label} must be refused, not marked"
        );
    }

    // Nothing was written to either scope.
    for (dir, label) in [(main, "main"), (wt.as_path(), "the linked worktree")] {
        let listed = run_libra_command(&["dirty", "--list"], dir);
        assert_cli_success(&listed, "dirty --list");
        assert!(
            !String::from_utf8_lossy(&listed.stdout).contains("m.txt"),
            "a refused mark left no rows in {label}"
        );
    }
}

/// §C.4.1.1 composite scope: a request must prove WHICH REPOSITORY it means.
///
/// A watcher that has drifted onto another checkout — or was started against
/// one and reconnected to another service — would otherwise mark paths in a
/// repository it never looked at. `repo_id` is required, not optional: an
/// optional proof is no proof.
#[test]
fn service_dirty_mark_requires_matching_repo_id() {
    let repo = service_repo();
    let main = repo.path();
    std::fs::write(main.join("r.txt"), "r\n").expect("write");
    let (_guard, base_url, token) = spawn_service(main);
    let client = reqwest::blocking::Client::new();

    // Omitted entirely: the body does not deserialize, so it is a 4xx.
    let omitted = client
        .post(format!("{base_url}/api/service/dirty/mark"))
        .header("x-libra-service-token", token.clone())
        .json(&serde_json::json!({ "paths": ["r.txt"], "scope": {"kind": "main"} }))
        .send()
        .expect("request");
    assert!(
        omitted.status().is_client_error(),
        "a scope without repo_id is refused, not defaulted: {}",
        omitted.status()
    );

    // Present but another repository's: refused with a message that says so.
    let wrong = client
        .post(format!("{base_url}/api/service/dirty/mark"))
        .header("x-libra-service-token", token.clone())
        .json(&serde_json::json!({
            "paths": ["r.txt"],
            "scope": {"kind": "main", "repo_id": "some-other-repository"},
        }))
        .send()
        .expect("request");
    assert_eq!(wrong.status().as_u16(), 409, "cross-repository request");

    // Nothing was marked by either attempt.
    let listed = run_libra_command(&["dirty", "--list"], main);
    assert_cli_success(&listed, "dirty --list");
    assert!(
        !String::from_utf8_lossy(&listed.stdout).contains("r.txt"),
        "a refused request writes nothing"
    );

    // The right one works, so the assertions above are not vacuous.
    let accepted = client
        .post(format!("{base_url}/api/service/dirty/mark"))
        .header("x-libra-service-token", token)
        .json(&serde_json::json!({
            "paths": ["r.txt"],
            "scope": {"kind": "main", "repo_id": repository_id(main)},
        }))
        .send()
        .expect("request");
    assert_eq!(
        accepted.status().as_u16(),
        200,
        "the correct repo_id is accepted"
    );
}

/// §C.4.1.1 service fence: a request carrying the epoch of a PREVIOUS
/// registration is refused, even though the id and the path are identical.
///
/// Instance ids are path-derived and paths obviously repeat, so a worktree
/// removed and re-added in place looks exactly like its predecessor to a
/// watcher that cached its identity. Without the fence, a request in flight
/// across the re-add marks the successor's cache — reporting files dirty in a
/// worktree that never touched them.
#[test]
fn service_dirty_mark_stale_epoch_rejected() {
    let (repo, _parent, wt) = repo_with_linked("fence-wt");
    let main = repo.path();
    let worktree_id = fs::read_to_string(wt.join(".libra").join("worktree_id"))
        .expect("the linked worktree records its id")
        .trim()
        .to_string();
    let stale_epoch = registered_epoch(main, &worktree_id);
    let repo_id = repository_id(main);

    // Remove and re-add at the SAME path: same id, same path, new generation.
    assert_cli_success(
        &run_libra_command(
            &["worktree", "remove", "--delete-dir", wt.to_str().unwrap()],
            main,
        ),
        "worktree remove",
    );
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap(), "fence-wt"], main),
        "worktree re-add",
    );
    let fresh_epoch = registered_epoch(main, &worktree_id);
    assert_ne!(
        stale_epoch, fresh_epoch,
        "the re-add is a new registration generation"
    );

    fs::write(wt.join("fenced.txt"), "fenced\n").unwrap();
    let (_guard, base_url, token) = spawn_service(main);
    let client = reqwest::blocking::Client::new();

    let stale = client
        .post(format!("{base_url}/api/service/dirty/mark"))
        .header("x-libra-service-token", token.clone())
        .json(&serde_json::json!({
            "paths": ["fenced.txt"],
            "scope": {
                "kind": "linked",
                "repo_id": repo_id,
                "worktree_id": worktree_id,
                "workdir": wt.to_string_lossy(),
                "epoch": stale_epoch,
            },
        }))
        .send()
        .expect("request");
    assert_eq!(
        stale.status().as_u16(),
        409,
        "a request fenced on the previous registration is refused"
    );
    let listed = run_libra_command(&["dirty", "--list"], &wt);
    assert_cli_success(&listed, "dirty --list");
    assert!(
        !String::from_utf8_lossy(&listed.stdout).contains("fenced.txt"),
        "and it wrote nothing to the successor's cache"
    );

    // The CURRENT generation is accepted.
    let fresh = client
        .post(format!("{base_url}/api/service/dirty/mark"))
        .header("x-libra-service-token", token)
        .json(&serde_json::json!({
            "paths": ["fenced.txt"],
            "scope": {
                "kind": "linked",
                "repo_id": repo_id,
                "worktree_id": worktree_id,
                "workdir": wt.to_string_lossy(),
                "epoch": fresh_epoch,
            },
        }))
        .send()
        .expect("request");
    assert_eq!(
        fresh.status().as_u16(),
        200,
        "the current fence is accepted"
    );
}

/// §C.12 `linked_layer_registration_isolated`: a layer registered in one
/// worktree is not visible in another. The same name and destination may exist
/// independently per worktree.
#[test]
fn linked_layer_registration_isolated() {
    let (repo, parent, wt) = repo_with_linked("layer-wt");
    let main = repo.path();
    let source = parent.path().join("overlay-src");
    fs::create_dir_all(&source).expect("source dir");
    fs::write(source.join("overlaid.txt"), "overlay\n").unwrap();

    assert_cli_success(
        &run_libra_command(
            &[
                "layer",
                "add",
                "shared-name",
                "--source",
                source.to_str().unwrap(),
            ],
            main,
        ),
        "layer add in main",
    );

    let listed_linked = run_libra_command(&["layer", "list"], &wt);
    assert_cli_success(&listed_linked, "layer list in the linked worktree");
    assert!(
        !String::from_utf8_lossy(&listed_linked.stdout).contains("shared-name"),
        "main's layer is not registered in the linked worktree"
    );

    // The SAME name registers independently there.
    assert_cli_success(
        &run_libra_command(
            &[
                "layer",
                "add",
                "shared-name",
                "--source",
                source.to_str().unwrap(),
            ],
            &wt,
        ),
        "the same layer name registers independently in the linked worktree",
    );
    let listed_main = run_libra_command(&["layer", "list"], main);
    assert_cli_success(&listed_main, "layer list in main");
    assert!(
        String::from_utf8_lossy(&listed_main.stdout).contains("shared-name"),
        "and main still has its own"
    );
}

/// §C.12 `linked_layer_status_reads_only_current_scope`: `layer status`
/// reports the acting worktree's registrations and materialized paths only.
#[test]
fn linked_layer_status_reads_only_current_scope() {
    let (repo, parent, wt) = repo_with_linked("status-wt");
    let main = repo.path();
    let source = parent.path().join("status-src");
    fs::create_dir_all(&source).expect("source dir");
    fs::write(source.join("only-main.txt"), "x\n").unwrap();

    assert_cli_success(
        &run_libra_command(
            &[
                "layer",
                "add",
                "main-layer",
                "--source",
                source.to_str().unwrap(),
            ],
            main,
        ),
        "layer add in main",
    );

    let status_linked = run_libra_command(&["layer", "status"], &wt);
    assert_cli_success(&status_linked, "layer status in the linked worktree");
    assert!(
        !String::from_utf8_lossy(&status_linked.stdout).contains("main-layer"),
        "the linked worktree's status shows only its own scope"
    );
    let status_main = run_libra_command(&["layer", "list"], main);
    assert_cli_success(&status_main, "layer list in main");
    assert!(
        String::from_utf8_lossy(&status_main.stdout).contains("main-layer"),
        "and main's own registration is there: {}",
        String::from_utf8_lossy(&status_main.stdout)
    );
}

/// §C.12 `linked_layer_apply_remove_scoped`: `apply` materializes only the
/// acting worktree's layers, and `remove` unregisters only there.
#[test]
fn linked_layer_apply_remove_scoped() {
    let (repo, parent, wt) = repo_with_linked("apply-wt");
    let main = repo.path();
    let source = parent.path().join("apply-src");
    fs::create_dir_all(source.join("dst")).expect("source dir");
    fs::write(source.join("dst").join("applied.txt"), "applied\n").unwrap();

    assert_cli_success(
        &run_libra_command(
            &[
                "layer",
                "add",
                "apply-layer",
                "--source",
                source.to_str().unwrap(),
            ],
            main,
        ),
        "layer add in main",
    );
    assert_cli_success(
        &run_libra_command(&["layer", "apply"], main),
        "apply in main",
    );
    assert!(
        main.join("dst").join("applied.txt").exists(),
        "main materialized its overlay"
    );

    // The linked worktree has no layers: apply must materialize nothing there.
    assert_cli_success(
        &run_libra_command(&["layer", "apply"], &wt),
        "apply in the linked worktree",
    );
    assert!(
        !wt.join("dst").join("applied.txt").exists(),
        "the linked worktree materialized nothing — main's layer is not its own"
    );

    // Removing in main leaves nothing behind for the other scope to trip over.
    assert_cli_success(
        &run_libra_command(&["layer", "remove", "apply-layer"], main),
        "remove in main",
    );
    assert!(
        !main.join("dst").join("applied.txt").exists(),
        "remove took the materialized file with it"
    );
}

/// §C.12 `linked_layer_unapply_does_not_detach_other_worktree`: unapplying in
/// one worktree leaves another worktree's materialized overlay in place.
#[test]
fn linked_layer_unapply_does_not_detach_other_worktree() {
    let (repo, parent, wt) = repo_with_linked("unapply-wt");
    let main = repo.path();
    let source = parent.path().join("unapply-src");
    fs::create_dir_all(source.join("dst")).expect("source dir");
    fs::write(source.join("dst").join("kept.txt"), "kept\n").unwrap();

    for dir in [main, wt.as_path()] {
        assert_cli_success(
            &run_libra_command(
                &["layer", "add", "both", "--source", source.to_str().unwrap()],
                dir,
            ),
            "layer add",
        );
        assert_cli_success(&run_libra_command(&["layer", "apply"], dir), "layer apply");
    }
    assert!(main.join("dst").join("kept.txt").exists(), "main applied");
    assert!(
        wt.join("dst").join("kept.txt").exists(),
        "the linked worktree applied"
    );

    assert_cli_success(
        &run_libra_command(&["layer", "unapply"], main),
        "unapply in main",
    );
    assert!(
        !main.join("dst").join("kept.txt").exists(),
        "main's materialized file is gone"
    );
    assert!(
        wt.join("dst").join("kept.txt").exists(),
        "the linked worktree's is untouched — unapply is scoped"
    );
}

/// §C.12 `linked_layer_add_guard_scoped`: the guard that refuses a
/// destination already owned by a layer consults the ACTING worktree's
/// ownership. Another worktree owning the same destination must not block a
/// registration here.
#[test]
fn linked_layer_add_guard_scoped() {
    let (repo, parent, wt) = repo_with_linked("guard-wt");
    let main = repo.path();
    let source = parent.path().join("guard-src");
    fs::create_dir_all(&source).expect("source dir");
    fs::write(source.join("g.txt"), "g\n").unwrap();

    assert_cli_success(
        &run_libra_command(
            &[
                "layer",
                "add",
                "guarded",
                "--source",
                source.to_str().unwrap(),
            ],
            main,
        ),
        "layer add in main",
    );

    // The same name in the OTHER worktree is allowed: registrations are scoped.
    assert_cli_success(
        &run_libra_command(
            &[
                "layer",
                "add",
                "guarded",
                "--source",
                source.to_str().unwrap(),
            ],
            &wt,
        ),
        "the same destination registers in the linked worktree",
    );

    // The SAME name again in the SAME worktree is refused — the guard reads
    // this scope's registrations, and they now contain it.
    let refused = run_libra_command(
        &[
            "layer",
            "add",
            "guarded",
            "--source",
            source.to_str().unwrap(),
        ],
        main,
    );
    assert!(
        !refused.status.success(),
        "re-registering a name in the SAME worktree is refused: {}",
        String::from_utf8_lossy(&refused.stdout)
    );
}

/// lore.md 2.4: `clean -f` must NOT delete a materialized layer overlay.
///
/// An overlay file is untracked by construction, so it looks exactly like the
/// debris `clean` exists to remove — and only a re-apply could bring it back.
/// The protection is the layer-exclusion snapshot, which a FRESH process has to
/// load before it enumerates: `clean` never did, so it saw an empty snapshot
/// and deleted the file. This test runs the real `clean -f` in a separate
/// process, which is the only way to catch that.
#[test]
fn clean_force_preserves_materialized_layer_overlay() {
    let repo = create_committed_repo_via_cli();
    let main = repo.path();
    let source = tempfile::tempdir().expect("overlay source");
    std::fs::create_dir_all(source.path().join("dst")).expect("source tree");
    std::fs::write(source.path().join("dst").join("overlay.txt"), "overlay\n")
        .expect("overlay file");

    assert_cli_success(
        &run_libra_command(
            &[
                "layer",
                "add",
                "protected",
                "--source",
                source.path().to_str().unwrap(),
            ],
            main,
        ),
        "layer add",
    );
    assert_cli_success(&run_libra_command(&["layer", "apply"], main), "layer apply");
    let overlay = main.join("dst").join("overlay.txt");
    assert!(overlay.exists(), "the overlay is materialized");

    // Ordinary untracked debris, to prove `clean` still does its job.
    std::fs::write(main.join("debris.txt"), "junk\n").expect("debris");

    let cleaned = run_libra_command(&["clean", "-f", "-d"], main);
    assert_cli_success(&cleaned, "clean -f -d");

    assert!(
        overlay.exists(),
        "clean -f must not delete the materialized layer overlay: {}",
        String::from_utf8_lossy(&cleaned.stdout)
    );
    assert!(
        !main.join("debris.txt").exists(),
        "and it must still remove ordinary untracked files"
    );

    // `-x` too. The ignore engine's `IncludeIgnored` policy deliberately stops
    // consulting layers (it exists for force-ADD, where the staging guard is
    // the backstop) — but here the stake is deletion, so `clean` applies the
    // protection itself regardless of policy.
    std::fs::write(main.join("more-debris.txt"), "junk\n").expect("debris");
    let cleaned_x = run_libra_command(&["clean", "-f", "-d", "-x"], main);
    assert_cli_success(&cleaned_x, "clean -f -d -x");
    assert!(
        overlay.exists(),
        "clean -fdx must not delete the overlay either: {}",
        String::from_utf8_lossy(&cleaned_x.stdout)
    );
    assert!(
        !main.join("more-debris.txt").exists(),
        "and -x still removes ordinary files"
    );
}

/// lore.md 2.4 / §C.11 W1: a materialized layer overlay cannot reach history,
/// even through PLUMBING.
///
/// `add` has its own staging guard, but `update-index --add` stages a path
/// directly with no layer check — so the last gate has to be at commit
/// publication. Overlay content lives outside the repository, so committing it
/// publishes something the repository cannot reproduce.
#[test]
fn layer_overlay_cannot_be_committed_through_update_index() {
    let repo = create_committed_repo_via_cli();
    let main = repo.path();
    let source = tempfile::tempdir().expect("overlay source");
    std::fs::create_dir_all(source.path().join("dst")).expect("source tree");
    std::fs::write(source.path().join("dst").join("secret.txt"), "local only\n")
        .expect("overlay file");

    assert_cli_success(
        &run_libra_command(
            &[
                "layer",
                "add",
                "guarded",
                "--source",
                source.path().to_str().unwrap(),
            ],
            main,
        ),
        "layer add",
    );
    assert_cli_success(&run_libra_command(&["layer", "apply"], main), "layer apply");
    let overlay = main.join("dst").join("secret.txt");
    assert!(overlay.exists(), "the overlay is materialized");

    // Plumbing stages it directly — this is allowed to succeed; the guard is at
    // the commit.
    let staged = run_libra_command(&["update-index", "--add", "dst/secret.txt"], main);
    if !staged.status.success() {
        // Some builds refuse earlier, which is also acceptable.
        return;
    }

    let committed = run_libra_command(&["commit", "-m", "sneak", "--no-verify"], main);
    assert!(
        !committed.status.success(),
        "a commit staging a layer-owned overlay must be REFUSED"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&committed.stdout),
        String::from_utf8_lossy(&committed.stderr)
    );
    assert!(
        combined.contains("layer overlay"),
        "and the refusal must say why: {combined}"
    );

    // And the overlay is still on disk — the refusal changed nothing.
    assert!(overlay.exists(), "the refusal did not touch the file");

    // The FULL plumbing route around `commit`: write-tree → commit-tree →
    // update-ref reaches a publishable branch without ever calling `commit`, so
    // the guard has to be at the tree too.
    let tree = run_libra_command(&["write-tree"], main);
    assert!(
        !tree.status.success(),
        "write-tree must refuse an index staging a layer overlay"
    );
    let tree_out = format!(
        "{}{}",
        String::from_utf8_lossy(&tree.stdout),
        String::from_utf8_lossy(&tree.stderr)
    );
    assert!(
        tree_out.contains("layer overlay"),
        "and say why: {tree_out}"
    );
    assert!(overlay.exists(), "still untouched");
}

/// The same protection for a NESTED overlay, which the flat case cannot catch.
///
/// `clean -d` removes a directory TREE. The scan used to decide about a
/// directory before recursing into it, so `dst` was queued for removal while
/// `dst/nested/overlay.txt` was still undiscovered — and the tree took the
/// protected file with it. Covers `-fd`, `-fdx` and `-fdX`.
#[test]
fn clean_force_preserves_nested_layer_overlay() {
    let repo = create_committed_repo_via_cli();
    let main = repo.path();
    let source = tempfile::tempdir().expect("overlay source");
    let deep = source.path().join("dst").join("nested");
    std::fs::create_dir_all(&deep).expect("source tree");
    std::fs::write(deep.join("overlay.txt"), "deep overlay\n").expect("overlay file");

    assert_cli_success(
        &run_libra_command(
            &[
                "layer",
                "add",
                "deep",
                "--source",
                source.path().to_str().unwrap(),
            ],
            main,
        ),
        "layer add",
    );
    assert_cli_success(&run_libra_command(&["layer", "apply"], main), "layer apply");
    let overlay = main.join("dst").join("nested").join("overlay.txt");
    assert!(overlay.exists(), "the nested overlay is materialized");

    for flags in [
        vec!["clean", "-f", "-d"],
        vec!["clean", "-f", "-d", "-x"],
        vec!["clean", "-f", "-d", "-X"],
    ] {
        let label = flags.join(" ");
        let out = run_libra_command(&flags, main);
        assert_cli_success(&out, &label);
        assert!(
            overlay.exists(),
            "`{label}` must not remove the tree holding a nested overlay: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

/// §C.12 `linked_sparse_view_patterns_isolated`: sparse patterns are
/// per-worktree — one worktree's filter must not gate another's files.
#[test]
fn linked_sparse_view_patterns_isolated() {
    let (repo, _parent, wt) = repo_with_linked("sparse-wt");
    let main = repo.path();

    assert_cli_success(
        &run_libra_command(&["sparse-view", "set", "only-main/*"], main),
        "sparse set in main",
    );

    let linked = run_libra_command(&["sparse-view", "list"], &wt);
    assert_cli_success(&linked, "sparse list in the linked worktree");
    assert!(
        !String::from_utf8_lossy(&linked.stdout).contains("only-main"),
        "main's patterns are not the linked worktree's"
    );
    let linked_status = run_libra_command(&["sparse-view", "status"], &wt);
    assert_cli_success(&linked_status, "sparse status in the linked worktree");
    assert!(
        !String::from_utf8_lossy(&linked_status.stdout).contains("enabled: true"),
        "and setting patterns in main did not enable the view there: {}",
        String::from_utf8_lossy(&linked_status.stdout)
    );
}

/// §C.12 `linked_sparse_view_clear_does_not_disable_other_worktree`: clearing
/// one worktree's view leaves another's enabled with its patterns intact.
#[test]
fn linked_sparse_view_clear_does_not_disable_other_worktree() {
    let (repo, _parent, wt) = repo_with_linked("clear-wt");
    let main = repo.path();

    assert_cli_success(
        &run_libra_command(&["sparse-view", "set", "kept/*"], &wt),
        "sparse set in the linked worktree",
    );
    assert_cli_success(
        &run_libra_command(&["sparse-view", "set", "dropped/*"], main),
        "sparse set in main",
    );
    assert_cli_success(
        &run_libra_command(&["sparse-view", "clear"], main),
        "sparse clear in main",
    );

    let linked = run_libra_command(&["sparse-view", "list"], &wt);
    assert_cli_success(&linked, "sparse list in the linked worktree");
    assert!(
        String::from_utf8_lossy(&linked.stdout).contains("kept/*"),
        "main's clear left the linked worktree's patterns alone: {}",
        String::from_utf8_lossy(&linked.stdout)
    );
    let main_list = run_libra_command(&["sparse-view", "list"], main);
    assert_cli_success(&main_list, "sparse list in main");
    assert!(
        !String::from_utf8_lossy(&main_list.stdout).contains("dropped/*"),
        "and main's own patterns really were cleared"
    );
}

/// §C.12 `linked_hydrate_sparse_gate_uses_current_scope`: the hydrate gate
/// reads the ACTING worktree's sparse state. A view enabled in one worktree
/// must not gate hydrate in another.
#[test]
fn linked_hydrate_sparse_gate_uses_current_scope() {
    let (repo, _parent, wt) = repo_with_linked("hydrate-wt");
    let main = repo.path();

    // Main enables a view that matches nothing in the tree.
    assert_cli_success(
        &run_libra_command(&["sparse-view", "set", "nothing-matches/*"], main),
        "sparse set in main",
    );

    // The linked worktree has no view, so its hydrate is ungated.
    let linked = run_libra_command(&["hydrate"], &wt);
    let linked_body = format!(
        "{}{}",
        String::from_utf8_lossy(&linked.stdout),
        String::from_utf8_lossy(&linked.stderr)
    );
    assert!(
        !linked_body.contains("nothing-matches"),
        "the linked worktree's hydrate is not gated by main's view: {linked_body}"
    );
    assert!(
        linked.status.success() || !linked_body.contains("sparse"),
        "and it is not refused for a sparse reason: {linked_body}"
    );
}

/// plan-20260714 W1: a request PARKED on the worktree registry lock must not
/// starve unrelated database work in the service process.
///
/// This pins the invariant behind a real defect. sqlx returns a pooled
/// connection by SPAWNING a task, and a spawn from inside a poll lands in
/// that worker's non-stealable LIFO slot; sea-orm pins SQLite pools to ONE
/// connection. Taking the blocking `flock` inline on a worker therefore
/// stranded the connection-return task, and the OTHER request burned the
/// full sqlx acquire timeout waiting for a connection that could never come
/// back — surfacing as a CONFIDENTLY WRONG `409 this service serves
/// repository ''` (a swallowed database error), or as a client timeout.
///
/// Unlike a thread race, this drives the parking DETERMINISTICALLY: an
/// external holder owns the lock for a fixed window, so both requests must
/// wait for it and then succeed.
#[test]
fn service_marks_survive_an_externally_held_registry_lock() {
    let repo = service_repo();
    let main = repo.path();
    let repo_id = repository_id(main);
    std::fs::write(main.join("held-a.txt"), "a\n").expect("write");
    std::fs::write(main.join("held-b.txt"), "b\n").expect("write");

    // The service signals HERE — immediately before it takes the registry
    // lock — so the hold window below starts when the handlers have really
    // arrived, not merely when the requests were sent.
    let rendezvous = main.join(".libra").join("test-rendezvous.log");
    let (_guard, base_url, token) = spawn_service_with_env(
        main,
        None,
        &[(
            "LIBRA_TEST_RENDEZVOUS",
            format!("service-registry-lock={}", rendezvous.display()),
        )],
    );

    // An EXTERNAL holder owns the registry lock for a bounded window.
    let lock_path = main.join(".libra").join("worktrees.lock");
    let holder = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .expect("open the registry lock");
    holder.lock().expect("hold the registry lock");

    // Two scoped marks issued WHILE the lock is held: each must wait for the
    // lock and then succeed — never answer from a starved database read.
    let mut handles = Vec::new();
    for path in ["held-a.txt", "held-b.txt"] {
        let base_url = base_url.clone();
        let token = token.clone();
        let repo_id = repo_id.clone();
        handles.push(std::thread::spawn(move || {
            let response = reqwest::blocking::Client::new()
                .post(format!("{base_url}/api/service/dirty/mark"))
                .header("x-libra-service-token", token)
                .json(&serde_json::json!({
                    "paths": [path],
                    "scope": { "kind": "main", "repo_id": repo_id },
                }))
                .send()
                .expect("request completes rather than timing out");
            (
                response.status().as_u16(),
                response.text().unwrap_or_default(),
                std::time::Instant::now(),
            )
        }));
    }

    // Wait for BOTH handlers to reach the acquisition point.
    let waited_from = std::time::Instant::now();
    loop {
        let arrivals = std::fs::read_to_string(&rendezvous)
            .map(|text| text.lines().count())
            .unwrap_or(0);
        if arrivals >= 2 {
            break;
        }
        assert!(
            waited_from.elapsed() < std::time::Duration::from_secs(60),
            "only {arrivals}/2 handlers reached the registry-lock acquisition \
             point — the rendezvous seam is not wired, so this test cannot \
             prove contention"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // Both handlers are now blocked on the lock. Hold it a while longer, and
    // remember exactly WHEN it was released.
    std::thread::sleep(std::time::Duration::from_secs(2));
    let released_at = std::time::Instant::now();
    drop(holder);

    for handle in handles {
        let (status, body, finished_at) = handle.join().expect("thread");
        assert_eq!(
            status, 200,
            "a mark that merely WAITED for the registry lock must succeed; a \
             starved identity read used to answer 409 with an empty repo id: {body}"
        );
        // Self-check against a VACUOUS pass: a response that completed BEFORE
        // the lock was released never waited on it, so the run proved nothing
        // about contention. Unlike a wall-clock threshold, this cannot be
        // satisfied by a handler that was simply slow to start.
        assert!(
            finished_at >= released_at,
            "the mark completed before the external lock was released — it \
             never contended for the registry lock, so this test did not \
             exercise the deadlock it guards"
        );
    }
}

/// plan-20260714 W1: a FAILED repository-identity read is a server error,
/// never a repository mismatch.
///
/// The handler used to collapse the failure into an empty string with
/// `.ok().flatten()…unwrap_or_default()` and answer `409 this service serves
/// repository ''` — a confident WRONG verdict that made a real deadlock
/// unreadable for weeks. The fault seam exists precisely because every
/// fixture has a healthy database, so this branch is otherwise untestable.
#[test]
fn service_reports_identity_read_failure_as_server_error() {
    let repo = service_repo();
    let main = repo.path();
    let repo_id = repository_id(main);
    std::fs::write(main.join("faulted.txt"), "x\n").expect("write");

    let (_guard, base_url, token) = spawn_service_with_fault(main, Some("service-repo-identity"));
    let response = reqwest::blocking::Client::new()
        .post(format!("{base_url}/api/service/dirty/mark"))
        .header("x-libra-service-token", token)
        .json(&serde_json::json!({
            "paths": ["faulted.txt"],
            "scope": { "kind": "main", "repo_id": repo_id },
        }))
        .send()
        .expect("request");

    let status = response.status().as_u16();
    let body = response.text().unwrap_or_default();
    assert_eq!(
        status, 500,
        "an identity-read failure is a SERVER error, not a scope mismatch: {body}"
    );
    assert!(
        !body.contains("serves repository ''"),
        "the old swallowed-error wording must not come back: {body}"
    );
    assert!(
        body.contains("cannot read this repository's identity"),
        "the failure keeps its diagnostic: {body}"
    );
}

/// plan-20260714 W1 UPGRADE path: a repository that has linked worktrees must
/// still be able to restart its OWN service over a legacy (version-1) record.
///
/// Version-1 records carry no `repoId`, and with linked-worktree evidence an
/// unattributable record is refused. Taken literally that bricks the upgrade:
/// a service killed by the very install that shipped the stamping leaves a v1
/// `service.json` behind, and every later `libra service run` would refuse to
/// start until a human deleted the file. A v1 record does name its writer's
/// `workingDir`, though, and `service` is a REPOSITORY-level surface — so a
/// working dir that still resolves to this repository's storage proves
/// ownership without proving which worktree wrote it, which is all the
/// repository policy needs. A working dir belonging elsewhere proves nothing
/// and must still be refused.
#[test]
fn service_restarts_over_its_own_legacy_record_in_a_linked_repository() {
    let repo = service_repo();
    let main = repo.path();

    // Linked-worktree evidence: without it the legacy record would be
    // adoptable under the OLD rule too, and this test would prove nothing.
    let linked = main.join("linked-wt");
    let added = run_libra_command(
        &["worktree", "add", linked.to_str().expect("utf-8 path")],
        main,
    );
    assert!(
        added.status.success(),
        "worktree add must succeed: {}{}",
        String::from_utf8_lossy(&added.stdout),
        String::from_utf8_lossy(&added.stderr)
    );

    let service_dir = main.join(".libra").join("service");
    std::fs::create_dir_all(&service_dir).expect("service dir");
    let info_path = service_dir.join("service.json");
    let legacy = |working_dir: &str| {
        serde_json::to_vec_pretty(&serde_json::json!({
            "version": 1,
            "mode": "service",
            "pid": u32::MAX,
            "baseUrl": "http://127.0.0.1:1",
            "workingDir": working_dir,
            "startedAt": "2026-01-01T00:00:00Z",
        }))
        .expect("serialize")
    };

    // A legacy record whose working dir is NOT this repository stays
    // ambiguous — the adoption is keyed on proof, not on the version alone.
    let elsewhere = legacy("/somewhere/else");
    std::fs::write(&info_path, &elsewhere).expect("plant a foreign legacy record");
    let refused = run_libra_command(&["service", "run", "--port", "0"], main);
    assert!(
        !refused.status.success(),
        "a legacy record from an unresolvable working dir must still be refused: {}",
        String::from_utf8_lossy(&refused.stdout)
    );
    assert_eq!(
        std::fs::read(&info_path).expect("still there"),
        elsewhere,
        "the refusal must leave the unattributable record byte-identical"
    );

    // This repository's OWN legacy record — written from the LINKED worktree,
    // the case the repository policy exists for — is adopted and restamped.
    std::fs::write(&info_path, legacy(linked.to_str().expect("utf-8 path")))
        .expect("plant our own legacy record");
    let (_guard, _base_url, _token) = spawn_service(main);
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&info_path).expect("service.json")).expect("json");
    assert_eq!(
        written["repoId"].as_str(),
        Some(repository_id(main).as_str()),
        "the restart adopts the legacy record and stamps this repository: {written}"
    );
    assert!(
        written["version"].as_u64().unwrap_or(0) >= 2,
        "the adopted record is rewritten at the current version: {written}"
    );
}

/// §C.12 named regression, END-TO-END half: `libra service run` must consult
/// the repository-policy takeover gate before it touches the token or the
/// info file, and must STAMP its own record with the repository identity.
///
/// Before this, the service wrote an unstamped version-1 `service.json` and
/// never called the gate, so an unlocked control record left by ANOTHER
/// repository's service was silently overwritten — the advisory lock
/// arbitrates liveness, not ownership.
#[test]
fn service_startup_refuses_a_foreign_stale_control_file() {
    let repo = service_repo();
    let main = repo.path();

    // A stale record from a DIFFERENT repository, with a dead pid.
    let service_dir = main.join(".libra").join("service");
    std::fs::create_dir_all(&service_dir).expect("service dir");
    let info_path = service_dir.join("service.json");
    let foreign = serde_json::json!({
        "version": 2,
        "mode": "service",
        "pid": u32::MAX,
        "baseUrl": "http://127.0.0.1:1",
        "workingDir": "/somewhere/else",
        "startedAt": "2026-01-01T00:00:00Z",
        "repoId": "00000000-0000-4000-8000-00000000dead",
    });
    let foreign_bytes = serde_json::to_vec_pretty(&foreign).expect("serialize");
    std::fs::write(&info_path, &foreign_bytes).expect("plant foreign control file");

    // The service must REFUSE to start rather than reclaim it.
    let refused = run_libra_command(&["service", "run", "--port", "0"], main);
    assert!(
        !refused.status.success(),
        "startup must refuse a foreign control record: {}",
        String::from_utf8_lossy(&refused.stdout)
    );
    assert_eq!(
        std::fs::read(&info_path).expect("still there"),
        foreign_bytes,
        "the refusal must leave the foreign record byte-identical"
    );

    // With the foreign record gone, startup succeeds AND stamps its own
    // repository identity (an unstamped record is indistinguishable from a
    // legacy file, which is what let a foreign one be overwritten).
    std::fs::remove_file(&info_path).expect("remove foreign record");
    let (_guard, _base_url, _token) = spawn_service(main);
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&info_path).expect("service.json")).expect("json");
    assert_eq!(
        written["repoId"].as_str(),
        Some(repository_id(main).as_str()),
        "the service stamps its own repository identity: {written}"
    );
    assert!(
        written["version"].as_u64().unwrap_or(0) >= 2,
        "a stamped record is version 2 or newer: {written}"
    );
}
