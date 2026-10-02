//! plan-20260912 MB-03: the public `libra mega2 browser` CLI contract.
//!
//! Every case drives the real binary against a loopback mock of the mega2
//! `/api/v1/tree` route from a scratch directory that is **not** a repository,
//! proving the command needs no repository, database or configuration state.

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    path::Path,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

/// Minimal blocking HTTP mock for the tree route: fixed status + body, counts
/// requests and remembers the request target (path + query) of the last call.
struct MockTreeServer {
    addr: SocketAddr,
    requests: Arc<AtomicUsize>,
    last_target: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl MockTreeServer {
    fn start(status: u16, body: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(AtomicUsize::new(0));
        let last_target = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let requests_clone = Arc::clone(&requests);
        let last_clone = Arc::clone(&last_target);
        let stop_clone = Arc::clone(&stop);
        let join = thread::spawn(move || {
            let reason = if status == 200 {
                "OK"
            } else {
                "Internal Server Error"
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            while !stop_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .expect("blocking mock connection");
                        let mut buf = [0u8; 8192];
                        let _ = stream.read(&mut buf);
                        let request = String::from_utf8_lossy(&buf);
                        let target = request
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .map(str::to_string);
                        *last_clone.lock().expect("target lock") = target;
                        requests_clone.fetch_add(1, Ordering::SeqCst);
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            requests,
            last_target,
            stop,
            join: Some(join),
        }
    }

    fn ok(tree_items: serde_json::Value) -> Self {
        let body = serde_json::json!({
            "req_result": true,
            "data": {"tree_items": tree_items},
            "err_message": "",
        })
        .to_string();
        Self::start(200, body)
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    fn last_target(&self) -> Option<String> {
        self.last_target.lock().expect("target lock").clone()
    }
}

impl Drop for MockTreeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn libra(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_libra"))
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .output()
        .expect("failed to spawn libra binary")
}

fn stdout_json(output: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not JSON ({err}); stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

#[test]
fn json_mode_defaults_emit_documented_schema_and_mutate_nothing() {
    let server = MockTreeServer::ok(serde_json::json!([
        {"name": "file.txt", "path": "/", "content_type": "file"},
        {"name": "beta-dir", "path": "/", "content_type": "directory"},
    ]));
    let workdir = tempfile::tempdir().expect("tempdir");

    let output = libra(
        workdir.path(),
        &["mega2", "browser", "--server", &server.url(), "--json"],
    );

    assert!(
        output.status.success(),
        "expected success; stderr={}",
        stderr(&output)
    );
    let json = stdout_json(&output);
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "mega2 browser");
    assert_eq!(json["data"]["server"], server.url());
    assert_eq!(json["data"]["ref"], serde_json::Value::Null);
    assert_eq!(json["data"]["path"], "/");
    // Deterministic directory-first ordering is inherited from MB-01.
    assert_eq!(json["data"]["items"][0]["name"], "beta-dir");
    assert_eq!(json["data"]["items"][0]["content_type"], "directory");
    assert_eq!(json["data"]["items"][1]["name"], "file.txt");
    assert_eq!(json["data"]["items"][1]["content_type"], "file");

    // Exactly one bounded request, to the fixed route with an encoded path.
    assert_eq!(server.requests(), 1, "one fetch per invocation");
    let target = server.last_target().expect("request target");
    assert!(
        target.starts_with("/api/v1/tree?"),
        "unexpected target: {target}"
    );
    assert!(target.contains("path=%2F"), "unexpected target: {target}");

    // Working outside a repository must leave the directory untouched.
    let leftovers: Vec<_> = std::fs::read_dir(workdir.path())
        .expect("read workdir")
        .collect();
    assert!(
        leftovers.is_empty(),
        "mega2 browser wrote local state: {leftovers:?}"
    );
}

#[test]
fn machine_mode_emits_single_ndjson_line() {
    let server = MockTreeServer::ok(serde_json::json!([]));
    let workdir = tempfile::tempdir().expect("tempdir");

    let output = libra(
        workdir.path(),
        &["mega2", "browser", "--server", &server.url(), "--machine"],
    );
    assert!(output.status.success(), "stderr={}", stderr(&output));

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "machine mode is one NDJSON line: {stdout}");
    let json: serde_json::Value = serde_json::from_str(lines[0]).expect("ndjson parses");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "mega2 browser");
    assert_eq!(server.requests(), 1);
}

#[test]
fn path_and_ref_are_forwarded_to_the_tree_route() {
    let server = MockTreeServer::ok(serde_json::json!([
        {"name": "inner.txt", "path": "/sub/dir", "content_type": "file"},
    ]));
    let workdir = tempfile::tempdir().expect("tempdir");

    let output = libra(
        workdir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &server.url(),
            "/sub/dir",
            "--ref",
            "v1.2",
            "--json",
        ],
    );
    assert!(output.status.success(), "stderr={}", stderr(&output));
    let json = stdout_json(&output);
    assert_eq!(json["data"]["path"], "/sub/dir");
    assert_eq!(json["data"]["ref"], "v1.2");
    assert_eq!(json["data"]["items"][0]["name"], "inner.txt");

    let target = server.last_target().expect("request target");
    assert!(target.contains("path=%2Fsub%2Fdir"), "target={target}");
    assert!(target.contains("refs=v1.2"), "target={target}");
}

#[test]
fn relative_path_is_rejected_before_any_request() {
    let server = MockTreeServer::ok(serde_json::json!([]));
    let workdir = tempfile::tempdir().expect("tempdir");

    let output = libra(
        workdir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &server.url(),
            "relative",
            "--json",
        ],
    );
    assert!(!output.status.success());
    assert_eq!(server.requests(), 0, "invalid invocation sends no request");
    let err = stderr(&output);
    assert!(err.contains("rooted"), "unexpected stderr: {err}");
}

#[test]
fn human_mode_without_a_tty_is_refused_without_network() {
    let server = MockTreeServer::ok(serde_json::json!([]));
    let workdir = tempfile::tempdir().expect("tempdir");

    let output = libra(
        workdir.path(),
        &["mega2", "browser", "--server", &server.url()],
    );
    assert!(!output.status.success(), "human mode must require a TTY");
    assert_eq!(server.requests(), 0, "TTY refusal happens before the fetch");
    let err = stderr(&output);
    assert!(
        err.contains("terminal") || err.contains("TTY"),
        "unexpected stderr: {err}"
    );
}

#[test]
fn server_url_rejections_are_stable_and_make_no_request() {
    let server = MockTreeServer::ok(serde_json::json!([]));
    let workdir = tempfile::tempdir().expect("tempdir");

    let cases: &[(&str, &str)] = &[
        ("http://example.com", "loopback"),
        ("ftp://example.com", "https"),
        ("https://user:secret@example.com", "credentials"),
        ("https://example.com?x=1", "query"),
    ];
    for (bad, needle) in cases {
        let output = libra(
            workdir.path(),
            &["mega2", "browser", "--server", bad, "--json"],
        );
        assert!(!output.status.success(), "expected rejection for {bad}");
        let err = stderr(&output);
        assert!(
            err.contains(needle),
            "stderr for {bad} should mention {needle}: {err}"
        );
        assert!(
            !err.contains("secret"),
            "credentials must never be echoed: {err}"
        );
    }
    // Only the valid loopback URL from other tests may have been used here.
    assert_eq!(server.requests(), 0);
}

/// plan-20261001 MN-10: a `--server` value that does not parse is rejected
/// before the credential check, so the JSON error envelope must not echo it.
#[test]
fn malformed_server_url_is_not_echoed() {
    let workdir = tempfile::tempdir().expect("tempdir");

    let output = libra(
        workdir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            "https://user:MARKER@mega2.example.com:notaport",
            "--json",
        ],
    );
    assert!(!output.status.success(), "unparsable URL must be refused");
    let err = stderr(&output);
    assert!(!err.contains("MARKER"), "raw URL was echoed: {err}");
    let envelope: serde_json::Value = serde_json::from_str(err.trim())
        .unwrap_or_else(|e| panic!("stderr is not one JSON envelope ({e}): {err}"));
    assert_eq!(
        envelope["error_code"], "LBR-CLI-003",
        "envelope: {envelope}"
    );
}

/// plan-20261001 MN-01 AC-2: a failed Mega2 request carries machine-readable
/// details in the stderr JSON error envelope, so automation never has to parse
/// the message to tell HTTP statuses apart.
#[test]
fn json_error_envelope_carries_http_details() {
    let failing = MockTreeServer::start(500, "SECRET-BODY".to_string());
    let workdir = tempfile::tempdir().expect("tempdir");

    let output = libra(
        workdir.path(),
        &["mega2", "browser", "--server", &failing.url(), "--machine"],
    );
    assert!(!output.status.success(), "HTTP 500 must fail");
    let err = stderr(&output);
    let envelope: serde_json::Value = serde_json::from_str(err.trim())
        .unwrap_or_else(|e| panic!("stderr is not one JSON envelope ({e}): {err}"));
    assert_eq!(
        envelope["details"],
        serde_json::json!({"method": "GET", "route": "/api/v1/tree", "http_status": 500}),
        "envelope: {envelope}"
    );
}

#[test]
fn http_and_schema_failures_are_reported_without_body_leakage() {
    let workdir = tempfile::tempdir().expect("tempdir");

    let failing = MockTreeServer::start(500, "SECRET-BODY".to_string());
    let output = libra(
        workdir.path(),
        &["mega2", "browser", "--server", &failing.url(), "--json"],
    );
    assert!(!output.status.success());
    let err = stderr(&output);
    assert!(err.contains("500"), "unexpected stderr: {err}");
    assert!(!err.contains("SECRET-BODY"), "response body leaked: {err}");

    let schema = MockTreeServer::start(
        200,
        serde_json::json!({"req_result": false, "data": null}).to_string(),
    );
    let output = libra(
        workdir.path(),
        &["mega2", "browser", "--server", &schema.url(), "--json"],
    );
    assert!(!output.status.success());
    assert!(
        !stderr(&output).is_empty(),
        "schema failure must explain itself"
    );
}

#[test]
fn missing_server_flag_is_a_usage_error() {
    let workdir = tempfile::tempdir().expect("tempdir");
    let output = libra(workdir.path(), &["mega2", "browser", "--json"]);
    assert!(!output.status.success());
    let err = stderr(&output);
    assert!(
        err.contains("--server") || err.contains("required"),
        "unexpected stderr: {err}"
    );
}

#[test]
fn help_surfaces_document_the_single_browser_subcommand() {
    let workdir = tempfile::tempdir().expect("tempdir");

    let parent = libra(workdir.path(), &["mega2", "--help"]);
    assert!(parent.status.success());
    let parent_help = String::from_utf8_lossy(&parent.stdout);
    assert!(parent_help.contains("EXAMPLES:"), "{parent_help}");
    assert!(parent_help.contains("browser"), "{parent_help}");
    assert!(!parent_help.contains("mkdir"), "{parent_help}");

    let browser = libra(workdir.path(), &["mega2", "browser", "--help"]);
    assert!(browser.status.success());
    let browser_help = String::from_utf8_lossy(&browser.stdout);
    assert!(browser_help.contains("--server"), "{browser_help}");
    assert!(browser_help.contains("--ref"), "{browser_help}");
    assert!(browser_help.contains("EXAMPLES:"), "{browser_help}");
}
