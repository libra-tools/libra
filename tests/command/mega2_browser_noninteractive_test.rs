//! plan-20261001 (MN-02 onward): non-interactive `libra mega2 browser`
//! operations.
//!
//! Every case drives the real binary with a cleared environment, stdin from
//! `/dev/null` (the R1b case keeps an open, silent pipe instead) and piped
//! stdout/stderr, against a loopback mock that records each request as
//! `(method, path and query, Authorization or none, body or empty)`.
//!
//! The ADR-MN-08 class rules are written once as `rule_*` functions and
//! instantiated per operation by `op_rule!` as `op_rule_<op>_<rule>` tests;
//! local-rejection rules split into a `_code` and a `_no_request` test.

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::Path,
    process::{Command, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

/// One request as the mock received it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Recorded {
    method: String,
    target: String,
    authorization: Option<String>,
    body: Option<String>,
}

/// Decides the response for `(method, path without query)`.
type Responder = Box<dyn Fn(&str, &str) -> (u16, String) + Send + 'static>;

/// Loopback mock of the mega2 routes: records every request in arrival order.
struct MockMega2 {
    addr: SocketAddr,
    records: Arc<Mutex<Vec<Recorded>>>,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl MockMega2 {
    fn start(responder: impl Fn(&str, &str) -> (u16, String) + Send + 'static) -> Self {
        let responder: Responder = Box::new(responder);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
        listener.set_nonblocking(true).expect("nonblocking mock");
        let addr = listener.local_addr().expect("mock addr");
        let records = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let records_clone = Arc::clone(&records);
        let stop_clone = Arc::clone(&stop);
        let join = thread::spawn(move || {
            while !stop_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).expect("blocking connection");
                        let Some(recorded) = read_request(&mut stream) else {
                            continue;
                        };
                        let path = recorded
                            .target
                            .split('?')
                            .next()
                            .unwrap_or_default()
                            .to_string();
                        let (status, body) = responder(&recorded.method, &path);
                        records_clone.lock().expect("records lock").push(recorded);
                        let reason = if (200..300).contains(&status) {
                            "OK"
                        } else {
                            "Error"
                        };
                        let response = format!(
                            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
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
            records,
            stop,
            join: Some(join),
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn records(&self) -> Vec<Recorded> {
        self.records.lock().expect("records lock").clone()
    }
}

impl Drop for MockMega2 {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Reads one request (head plus `Content-Length` body).
fn read_request(stream: &mut TcpStream) -> Option<Recorded> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..n]);
        let Some(head_end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&bytes[..head_end]).to_string();
        let mut lines = head.split("\r\n");
        let mut request_line = lines.next().unwrap_or_default().split_whitespace();
        let method = request_line.next().unwrap_or_default().to_string();
        let target = request_line.next().unwrap_or_default().to_string();
        let mut authorization = None;
        let mut content_length = 0usize;
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("authorization") {
                    authorization = Some(value.trim().to_string());
                } else if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
        }
        let body_start = head_end + 4;
        if bytes.len() < body_start + content_length {
            continue;
        }
        let body = (content_length > 0).then(|| {
            String::from_utf8_lossy(&bytes[body_start..body_start + content_length]).to_string()
        });
        return Some(Recorded {
            method,
            target,
            authorization,
            body,
        });
    }
}

fn envelope(data: Value) -> String {
    json!({"req_result": true, "data": data, "err_message": ""}).to_string()
}

fn tree_ok() -> String {
    envelope(json!({"tree_items": [
        {"name": "zeta.txt", "path": "/", "content_type": "file"},
        {"name": "alpha", "path": "/", "content_type": "directory"},
    ]}))
}

/// Starts the binary with a cleared environment plus `env`.
fn command(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
    command
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir)
        .env("LANG", "C")
        .env("LC_ALL", "C");
    for (key, value) in env {
        command.env(key, value);
    }
    command
}

/// Runs with stdin from `/dev/null` and piped stdout/stderr.
fn run(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    command(dir, args, env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn libra")
}

/// Runs with stdin as an open pipe that never receives data; `None` when the
/// process is still running after `limit` (it is then killed).
fn run_with_silent_stdin(dir: &Path, args: &[&str], limit: Duration) -> Option<Output> {
    let mut child = command(dir, args, &[])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn libra");
    let stdin = child.stdin.take();
    let started = Instant::now();
    loop {
        if child.try_wait().expect("poll libra").is_some() {
            drop(stdin);
            return Some(child.wait_with_output().expect("collect libra output"));
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            drop(stdin);
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

/// The JSON error envelope on stderr (the whole stream in machine modes, the
/// trailing JSON line in human mode).
fn error_envelope(output: &Output) -> Value {
    let stderr = text(&output.stderr);
    stderr
        .lines()
        .rev()
        .find_map(|line| {
            serde_json::from_str::<Value>(line.trim())
                .ok()
                .filter(|v| v.get("error_code").is_some())
        })
        .or_else(|| serde_json::from_str::<Value>(stderr.trim()).ok())
        .unwrap_or_else(|| panic!("no JSON error envelope on stderr: {stderr}"))
}

// ---- operation descriptors ----

/// How to invoke one operation and what its single request looks like.
struct OpCase {
    /// argv after `--server <url>`, selecting the operation.
    op_args: &'static [&'static str],
    method: &'static str,
    /// Route template reported in error details.
    route: &'static str,
    /// Concrete request path (without query) for `op_args`.
    path: &'static str,
    ok: fn(&str, &str) -> (u16, String),
}

fn list_ok(_method: &str, _path: &str) -> (u16, String) {
    (200, tree_ok())
}

const LIST_CASE: OpCase = OpCase {
    op_args: &["--list", "/"],
    method: "GET",
    route: "/api/v1/tree",
    path: "/api/v1/tree",
    ok: list_ok,
};

fn argv<'a>(
    server: &'a str,
    case: &'a OpCase,
    mode: &[&'a str],
    extra: &[&'a str],
) -> Vec<&'a str> {
    let mut args = vec!["mega2", "browser", "--server", server];
    args.extend_from_slice(case.op_args);
    args.extend_from_slice(mode);
    args.extend_from_slice(extra);
    args
}

/// 500 only for this operation's own method and path; anything else gets an
/// unexpected 418, so a request to the wrong endpoint cannot satisfy R4a.
fn fail_500_on(case: &OpCase) -> impl Fn(&str, &str) -> (u16, String) + Send + 'static {
    let (method, path) = (case.method, case.path);
    move |m: &str, p: &str| {
        if m == method && p == path {
            (500, String::new())
        } else {
            (418, String::new())
        }
    }
}

// ---- ADR-MN-08 class rules, written once ----

/// R1: human mode, stdin `/dev/null`, piped outputs → exit 0 (the TTY gate
/// would fail a call that strayed into the TUI).
fn rule_r1(case: &OpCase) {
    let mock = MockMega2::start(case.ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(dir.path(), &argv(&mock.url(), case, &[], &[]), &[]);
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
}

/// R1b: stdin is an open pipe that never receives data → the call ends
/// within 5 seconds (reading stdin would block until killed).
fn rule_r1b(case: &OpCase) {
    let mock = MockMega2::start(case.ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run_with_silent_stdin(
        dir.path(),
        &argv(&mock.url(), case, &[], &[]),
        Duration::from_secs(5),
    );
    assert!(
        output.is_some(),
        "the call read stdin or hung past 5 seconds"
    );
}

/// R2: human success output carries no ESC byte.
fn rule_r2(case: &OpCase) {
    let mock = MockMega2::start(case.ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(dir.path(), &argv(&mock.url(), case, &[], &[]), &[]);
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert!(
        !output.stdout.contains(&0x1b),
        "stdout carries ESC: {:?}",
        text(&output.stdout)
    );
}

/// R3: human success under `--quiet` prints nothing on stdout.
fn rule_r3(case: &OpCase) {
    let mock = MockMega2::start(case.ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(dir.path(), &argv(&mock.url(), case, &["--quiet"], &[]), &[]);
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert!(
        output.stdout.is_empty(),
        "stdout: {:?}",
        text(&output.stdout)
    );
}

/// R4a: a 500 on the operation's route yields `details` with its method and route.
fn rule_r4a(case: &OpCase) {
    let mock = MockMega2::start(fail_500_on(case));
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(
        dir.path(),
        &argv(&mock.url(), case, &["--machine"], &[]),
        &[],
    );
    assert!(!output.status.success());
    assert_eq!(
        error_envelope(&output)["details"],
        json!({"method": case.method, "route": case.route, "http_status": 500})
    );
}

/// R4b: a 500 is not retried — exactly one request.
fn rule_r4b(case: &OpCase) {
    let mock = MockMega2::start(fail_500_on(case));
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(
        dir.path(),
        &argv(&mock.url(), case, &["--machine"], &[]),
        &[],
    );
    assert!(!output.status.success());
    assert_eq!(mock.records().len(), 1, "records: {:?}", mock.records());
}

fn r5a_output(case: &OpCase, mock: &MockMega2) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    run(
        dir.path(),
        &argv(&mock.url(), case, &["--json"], &["--token", "secret"]),
        &[],
    )
}

/// R5a ①: `--json` with `--token` fails with `LBR-CLI-002`.
fn rule_r5a_code(case: &OpCase) {
    let mock = MockMega2::start(case.ok);
    let output = r5a_output(case, &mock);
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-CLI-002");
}

/// R5a ②: the refusal sends no request.
fn rule_r5a_no_request(case: &OpCase) {
    let mock = MockMega2::start(case.ok);
    let output = r5a_output(case, &mock);
    assert!(!output.status.success());
    assert!(mock.records().is_empty(), "records: {:?}", mock.records());
}

fn r5b_output(case: &OpCase, mock: &MockMega2) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    let token_file = dir.path().join("token");
    std::fs::write(&token_file, "filetok").expect("write token file");
    let token_file = token_file.to_string_lossy().to_string();
    run(
        dir.path(),
        &argv(&mock.url(), case, &[], &["--token-file", &token_file]),
        &[],
    )
}

/// R5b ①: human mode with `--token-file` fails with `LBR-CLI-002`.
fn rule_r5b_code(case: &OpCase) {
    let mock = MockMega2::start(case.ok);
    let output = r5b_output(case, &mock);
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-CLI-002");
}

/// R5b ②: the refusal sends no request.
fn rule_r5b_no_request(case: &OpCase) {
    let mock = MockMega2::start(case.ok);
    let output = r5b_output(case, &mock);
    assert!(!output.status.success());
    assert!(mock.records().is_empty(), "records: {:?}", mock.records());
}

/// R6: a read operation sends no Authorization even with `LIBRA_MEGA2_TOKEN` set.
fn rule_r6(case: &OpCase) {
    let mock = MockMega2::start(case.ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(
        dir.path(),
        &argv(&mock.url(), case, &["--json"], &[]),
        &[("LIBRA_MEGA2_TOKEN", "envtok")],
    );
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    let authorizations: Vec<Option<String>> = mock
        .records()
        .into_iter()
        .map(|record| record.authorization)
        .collect();
    assert_eq!(authorizations, vec![None]);
}

/// One `op_rule_<op>_<rule>` test per applicable rule (ADR-MN-08).
macro_rules! op_rule {
    ($name:ident, $rule:ident, $case:expr) => {
        #[test]
        fn $name() {
            $rule(&$case);
        }
    };
}

// ---- list (MN-02): R1, R1b, R2, R3, R4a, R4b, R5a, R5b, R6 ----

op_rule!(op_rule_list_r1, rule_r1, LIST_CASE);
op_rule!(op_rule_list_r1b, rule_r1b, LIST_CASE);
op_rule!(op_rule_list_r2, rule_r2, LIST_CASE);
op_rule!(op_rule_list_r3, rule_r3, LIST_CASE);
op_rule!(op_rule_list_r4a, rule_r4a, LIST_CASE);
op_rule!(op_rule_list_r4b, rule_r4b, LIST_CASE);
op_rule!(op_rule_list_r5a_code, rule_r5a_code, LIST_CASE);
op_rule!(op_rule_list_r5a_no_request, rule_r5a_no_request, LIST_CASE);
op_rule!(op_rule_list_r5b_code, rule_r5b_code, LIST_CASE);
op_rule!(op_rule_list_r5b_no_request, rule_r5b_no_request, LIST_CASE);
op_rule!(op_rule_list_r6, rule_r6, LIST_CASE);

// ---- list (MN-02): operation-specific behavior ----

/// AC-1: `--list <PATH> --ref <REF>` sends exactly one anonymous GET.
#[test]
fn list_request_record() {
    let mock = MockMega2::start(list_ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--list",
            "/src/pkg",
            "--ref",
            "v1.2",
            "--json",
        ],
        &[],
    );
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(
        mock.records(),
        vec![Recorded {
            method: "GET".to_string(),
            target: "/api/v1/tree?path=%2Fsrc%2Fpkg&refs=v1.2".to_string(),
            authorization: None,
            body: None,
        }]
    );
}

/// AC-2: human `--list` prints `<kind>  <name>` per entry in listing order.
#[test]
fn list_human_output() {
    let mock = MockMega2::start(list_ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(
        dir.path(),
        &["mega2", "browser", "--server", &mock.url(), "--list"],
        &[],
    );
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "dir  alpha\nfile  zeta.txt\n");
}

/// AC-3: the `--list --json` payload is the listing plus `operation`.
#[test]
fn list_json_payload() {
    let mock = MockMega2::start(list_ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--list",
            "--json",
        ],
        &[],
    );
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(
        envelope["data"],
        json!({
            "operation": "list",
            "server": mock.url(),
            "ref": null,
            "path": "/",
            "items": [
                {"name": "alpha", "content_type": "directory"},
                {"name": "zeta.txt", "content_type": "file"},
            ],
        })
    );
}

/// AC-4: bare `--json` and `--list --json` print the same bytes.
#[test]
fn list_bare_json_matches_list_json() {
    let mock = MockMega2::start(list_ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let bare = run(
        dir.path(),
        &["mega2", "browser", "--server", &mock.url(), "--json"],
        &[],
    );
    let list = run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--list",
            "--json",
        ],
        &[],
    );
    assert!(bare.status.success() && list.status.success());
    assert_eq!(text(&bare.stdout), text(&list.stdout));
}

/// AC-7: a failed `--list --machine` leaves stdout empty.
#[test]
fn list_failure_leaves_stdout_empty() {
    let mock = MockMega2::start(fail_500_on(&LIST_CASE));
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--list",
            "--machine",
        ],
        &[],
    );
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stdout: {:?}",
        text(&output.stdout)
    );
}
