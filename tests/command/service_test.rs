//! Integration tests for `libra service` (lore.md §1.11): loopback-only
//! headless service, notification v1, token-gated dirty-mark ingestion, and
//! the §7.10 kill-9 fault row (marks persist; restart reclaims the lock).
//!
//! **Layer:** L1 — deterministic, loopback networking only.

use std::process::{Child, Stdio};

use super::*;

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
    let child = base_libra_command(&["service", "run", "--port", "0"], p)
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
