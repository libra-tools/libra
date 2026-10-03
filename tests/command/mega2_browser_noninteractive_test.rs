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

fn create_entry_receipt(commit_id: &str) -> String {
    envelope(json!({
        "commit_id": commit_id,
        "new_oid": "oid-1",
        "path": "/sub",
        "cl_link": null,
    }))
}

/// The rule fixture's receipt carries an ESC in `commit_id`, so R2 proves the
/// human summary sanitizes server strings.
fn create_dir_ok(_method: &str, _path: &str) -> (u16, String) {
    (200, create_entry_receipt("commit-\u{1b}[31m-1"))
}

fn create_dir_clean_ok(_method: &str, _path: &str) -> (u16, String) {
    (200, create_entry_receipt("commit-1"))
}

const CREATE_DIR_CASE: OpCase = OpCase {
    op_args: &["--create-dir", "sub", "/"],
    method: "POST",
    route: "/api/v1/create-entry",
    path: "/api/v1/create-entry",
    ok: create_dir_ok,
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

/// `status` with `body` only for this operation's own method and path;
/// anything else gets an unexpected 418, so a request to the wrong endpoint
/// cannot satisfy R4a or a server-failure fixture.
fn fail_on(
    case: &OpCase,
    status: u16,
    body: &'static str,
) -> impl Fn(&str, &str) -> (u16, String) + Send + 'static {
    let (method, path) = (case.method, case.path);
    move |m: &str, p: &str| {
        if m == method && p == path {
            (status, body.to_string())
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
    let mock = MockMega2::start(fail_on(case, 500, ""));
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
    let mock = MockMega2::start(fail_on(case, 500, ""));
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

fn r7_output(case: &OpCase, mock: &MockMega2) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    run(
        dir.path(),
        &argv(&mock.url(), case, &["--machine"], &["--list"]),
        &[],
    )
}

/// R7 ①: combined with another operation flag (`--list`), the call fails
/// with `LBR-CLI-002`.
fn rule_r7_code(case: &OpCase) {
    let mock = MockMega2::start(case.ok);
    let output = r7_output(case, &mock);
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-CLI-002");
}

/// R7 ②: the refusal sends no request.
fn rule_r7_no_request(case: &OpCase) {
    let mock = MockMega2::start(case.ok);
    let output = r7_output(case, &mock);
    assert!(!output.status.success());
    assert!(mock.records().is_empty(), "records: {:?}", mock.records());
}

fn r8_output(case: &OpCase, mock: &MockMega2) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    run(
        dir.path(),
        &argv(&mock.url(), case, &["--machine"], &["--ref", "v1"]),
        &[],
    )
}

/// R8 ①: a write operation with `--ref` fails with `LBR-CLI-002`.
fn rule_r8_code(case: &OpCase) {
    let mock = MockMega2::start(case.ok);
    let output = r8_output(case, &mock);
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-CLI-002");
}

/// R8 ②: the refusal sends no request.
fn rule_r8_no_request(case: &OpCase) {
    let mock = MockMega2::start(case.ok);
    let output = r8_output(case, &mock);
    assert!(!output.status.success());
    assert!(mock.records().is_empty(), "records: {:?}", mock.records());
}

/// Writes `content` to a token file in `dir` and returns its path.
fn token_file(dir: &Path, content: &str) -> String {
    let path = dir.join("token");
    std::fs::write(&path, content).expect("write token file");
    path.to_string_lossy().to_string()
}

fn authorizations(mock: &MockMega2) -> Vec<Option<String>> {
    mock.records()
        .into_iter()
        .map(|record| record.authorization)
        .collect()
}

/// R9a: with `--token-file`, `LIBRA_MEGA2_TOKEN` and `--token` all set, the
/// one request carries exactly one `Bearer` with the token file's content.
fn rule_r9a(case: &OpCase) {
    let mock = MockMega2::start(case.ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let file = token_file(dir.path(), "filetok");
    let output = run(
        dir.path(),
        &argv(
            &mock.url(),
            case,
            &["--json"],
            &["--token-file", &file, "--token", "flagtok"],
        ),
        &[("LIBRA_MEGA2_TOKEN", "envtok")],
    );
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(
        authorizations(&mock),
        vec![Some("Bearer filetok".to_string())]
    );
}

/// R9b: with no token source the write is anonymous.
fn rule_r9b(case: &OpCase) {
    let mock = MockMega2::start(case.ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(dir.path(), &argv(&mock.url(), case, &["--json"], &[]), &[]);
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(authorizations(&mock), vec![None]);
}

/// R9c: a rejected token (401) never appears in stdout or stderr.
fn rule_r9c(case: &OpCase) {
    let mock = MockMega2::start(|_method: &str, _path: &str| (401, String::new()));
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(
        dir.path(),
        &argv(
            &mock.url(),
            case,
            &["--json"],
            &["--token", "r9c-secret-token"],
        ),
        &[],
    );
    assert!(!output.status.success());
    let combined = format!("{}{}", text(&output.stdout), text(&output.stderr));
    assert!(
        !combined.contains("r9c-secret-token"),
        "token echoed: {combined}"
    );
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
    let mock = MockMega2::start(fail_on(&LIST_CASE, 500, ""));
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

// ---- create_dir (MN-03, MN-11): R1, R1b, R2, R3, R4a, R4b, R7, R8, R9a, R9b, R9c ----

op_rule!(op_rule_create_dir_r1, rule_r1, CREATE_DIR_CASE);
op_rule!(op_rule_create_dir_r1b, rule_r1b, CREATE_DIR_CASE);
op_rule!(op_rule_create_dir_r2, rule_r2, CREATE_DIR_CASE);
op_rule!(op_rule_create_dir_r3, rule_r3, CREATE_DIR_CASE);
op_rule!(op_rule_create_dir_r4a, rule_r4a, CREATE_DIR_CASE);
op_rule!(op_rule_create_dir_r4b, rule_r4b, CREATE_DIR_CASE);
op_rule!(op_rule_create_dir_r7_code, rule_r7_code, CREATE_DIR_CASE);
op_rule!(
    op_rule_create_dir_r7_no_request,
    rule_r7_no_request,
    CREATE_DIR_CASE
);
op_rule!(op_rule_create_dir_r8_code, rule_r8_code, CREATE_DIR_CASE);
op_rule!(
    op_rule_create_dir_r8_no_request,
    rule_r8_no_request,
    CREATE_DIR_CASE
);
op_rule!(op_rule_create_dir_r9a, rule_r9a, CREATE_DIR_CASE);
op_rule!(op_rule_create_dir_r9b, rule_r9b, CREATE_DIR_CASE);
op_rule!(op_rule_create_dir_r9c, rule_r9c, CREATE_DIR_CASE);

// ---- create_dir (MN-03): operation-specific behavior ----

/// Requests as `(method, path and query, Authorization, parsed JSON body)`.
fn request_tuples(mock: &MockMega2) -> Vec<(String, String, Option<String>, Option<Value>)> {
    mock.records()
        .into_iter()
        .map(|record| {
            let body = record
                .body
                .map(|body| serde_json::from_str(&body).expect("request body is JSON"));
            (record.method, record.target, record.authorization, body)
        })
        .collect()
}

/// AC-1: one anonymous POST with the directory body.
#[test]
fn create_dir_request_record() {
    let mock = MockMega2::start(create_dir_clean_ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--create-dir",
            "sub",
            "/src",
            "--json",
        ],
        &[],
    );
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(
        request_tuples(&mock),
        vec![(
            "POST".to_string(),
            "/api/v1/create-entry".to_string(),
            None,
            Some(json!({
                "is_directory": true,
                "name": "sub",
                "path": "/src",
                "content": null,
                "skip_build": true,
            })),
        )]
    );
}

/// AC-2: `target` comes from local input, `receipt` is the server's receipt.
#[test]
fn create_dir_json_payload() {
    let mock = MockMega2::start(create_dir_clean_ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--create-dir",
            "sub",
            "/src",
            "--json",
        ],
        &[],
    );
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(
        envelope["data"],
        json!({
            "operation": "create-dir",
            "server": mock.url(),
            "target": {"parent": "/src", "name": "sub", "path": "/src/sub"},
            "receipt": {"commit_id": "commit-1", "new_oid": "oid-1", "path": "/sub", "cl_link": null},
        })
    );
}

/// AC-3: the human summary is one sanitized line.
#[test]
fn create_dir_human_output() {
    let mock = MockMega2::start(create_dir_clean_ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--create-dir",
            "sub",
            "/src",
        ],
        &[],
    );
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        "created directory /src/sub (commit commit-1)\n"
    );
}

fn dotdot_name_output(mock: &MockMega2) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--create-dir",
            "..",
            "/",
            "--machine",
        ],
        &[],
    )
}

/// G15: NAME `..` (delegated to `validate_entry_name`) fails with `LBR-CLI-002`.
#[test]
fn create_dir_rejects_dotdot_name_code() {
    let mock = MockMega2::start(create_dir_clean_ok);
    let output = dotdot_name_output(&mock);
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-CLI-002");
}

/// G16: the same refusal sends no request.
#[test]
fn create_dir_rejects_dotdot_name_no_request() {
    let mock = MockMega2::start(create_dir_clean_ok);
    let output = dotdot_name_output(&mock);
    assert!(!output.status.success());
    assert!(mock.records().is_empty(), "records: {:?}", mock.records());
}

/// mega2 answers a duplicate directory with HTTP 500 today.
fn duplicate_output() -> Output {
    let mock = MockMega2::start(fail_on(&CREATE_DIR_CASE, 500, ""));
    let dir = tempfile::tempdir().expect("tempdir");
    run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--create-dir",
            "sub",
            "/",
            "--machine",
        ],
        &[],
    )
}

/// G17: a duplicate (server 500) fails with `LBR-NET-002`.
#[test]
fn create_dir_duplicate_code() {
    let output = duplicate_output();
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-NET-002");
}

/// G18: the same failure carries `details.http_status` 500.
#[test]
fn create_dir_duplicate_http_status() {
    let output = duplicate_output();
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["details"]["http_status"], 500);
}

/// Serves one connection: reads the request head, then answers 200 with a
/// body cut short of its `Content-Length` and closes.
fn serve_truncated_once() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind raw mock");
    let addr = listener.local_addr().expect("raw mock addr");
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept()
            && read_request(&mut stream).is_some()
        {
            let partial = r#"{"req_result":true,"data":{"commit_id":"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{partial}",
                partial.len() + 64
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://{addr}")
}

/// A connection that drops while the receipt is read leaves the write's
/// outcome unknown; the envelope reports the status that arrived, not a
/// transport class.
#[test]
fn create_dir_receipt_read_drop_reports_http_status() {
    let server = serve_truncated_once();
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &server,
            "--create-dir",
            "sub",
            "/",
            "--machine",
        ],
        &[],
    );
    assert!(!output.status.success());
    let envelope = error_envelope(&output);
    assert_eq!(
        (&envelope["error_code"], &envelope["details"]),
        (
            &json!("LBR-NET-001"),
            &json!({"method": "POST", "route": "/api/v1/create-entry", "http_status": 200}),
        )
    );
}

// ---- write credentials (MN-11): source matrix on create_dir ----

/// G4: only `LIBRA_MEGA2_TOKEN` set.
#[test]
fn create_dir_token_from_env_only() {
    let mock = MockMega2::start(create_dir_clean_ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--create-dir",
            "sub",
            "/",
            "--json",
        ],
        &[("LIBRA_MEGA2_TOKEN", "envtok")],
    );
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(
        authorizations(&mock),
        vec![Some("Bearer envtok".to_string())]
    );
}

/// G5: only `--token` given.
#[test]
fn create_dir_token_from_flag_only() {
    let mock = MockMega2::start(create_dir_clean_ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--create-dir",
            "sub",
            "/",
            "--json",
            "--token",
            "flagtok",
        ],
        &[],
    );
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(
        authorizations(&mock),
        vec![Some("Bearer flagtok".to_string())]
    );
}

/// G6: human mode with `--token-file` sends the file's token.
#[test]
fn create_dir_token_human_mode_sends_token_file() {
    let mock = MockMega2::start(create_dir_clean_ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let file = token_file(dir.path(), "filetok");
    let output = run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--create-dir",
            "sub",
            "/",
            "--token-file",
            &file,
        ],
        &[],
    );
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(
        authorizations(&mock),
        vec![Some("Bearer filetok".to_string())]
    );
}

/// G7: human mode, same token file, server 401 — the token never appears.
#[test]
fn create_dir_token_human_mode_401_never_echoes() {
    let mock = MockMega2::start(|_method: &str, _path: &str| (401, String::new()));
    let dir = tempfile::tempdir().expect("tempdir");
    let file = token_file(dir.path(), "filetok");
    let output = run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--create-dir",
            "sub",
            "/",
            "--token-file",
            &file,
        ],
        &[],
    );
    assert!(!output.status.success());
    let combined = format!("{}{}", text(&output.stdout), text(&output.stderr));
    assert!(!combined.contains("filetok"), "token echoed: {combined}");
}

/// AC-1: a read operation refuses token flags with the credentials message.
#[test]
fn list_token_refusal_message() {
    let mock = MockMega2::start(list_ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--json",
            "--list",
            "/",
            "--token",
            "secret",
        ],
        &[],
    );
    assert!(!output.status.success());
    assert_eq!(
        error_envelope(&output)["message"],
        "mega2 browser: read operations take no credentials; --token/--token-file only apply to write operations"
    );
}

/// A missing `--token-file` fails before any request and never falls back to
/// `LIBRA_MEGA2_TOKEN`.
#[test]
fn create_dir_missing_token_file_sends_nothing() {
    let mock = MockMega2::start(create_dir_clean_ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir
        .path()
        .join("no-such-token")
        .to_string_lossy()
        .to_string();
    let output = run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--create-dir",
            "sub",
            "/",
            "--json",
            "--token-file",
            &missing,
        ],
        &[("LIBRA_MEGA2_TOKEN", "envtok")],
    );
    assert_eq!(
        (output.status.success(), mock.records()),
        (false, Vec::new()),
        "stderr: {}",
        text(&output.stderr)
    );
}

/// An empty `--token-file` fails before any request and never falls back to
/// `LIBRA_MEGA2_TOKEN`.
#[test]
fn create_dir_empty_token_file_sends_nothing() {
    let mock = MockMega2::start(create_dir_clean_ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let empty = token_file(dir.path(), "");
    let output = run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--create-dir",
            "sub",
            "/",
            "--json",
            "--token-file",
            &empty,
        ],
        &[("LIBRA_MEGA2_TOKEN", "envtok")],
    );
    assert_eq!(
        (output.status.success(), mock.records()),
        (false, Vec::new()),
        "stderr: {}",
        text(&output.stderr)
    );
}

// ---- delete_dir (MN-04): R1, R1b, R2, R3, R4a, R4b, R7, R8, R9a, R9b, R9c ----

fn delete_entry_receipt(commit_id: &str) -> String {
    envelope(json!({
        "commit_id": commit_id,
        "path": "/sub",
        "cl_link": null,
    }))
}

/// The rule fixture's receipt carries an ESC in `commit_id`, so R2 proves the
/// human summary sanitizes server strings.
fn delete_dir_ok(_method: &str, _path: &str) -> (u16, String) {
    (200, delete_entry_receipt("commit-\u{1b}[31m-1"))
}

fn delete_dir_clean_ok(_method: &str, _path: &str) -> (u16, String) {
    (200, delete_entry_receipt("commit-1"))
}

const DELETE_DIR_CASE: OpCase = OpCase {
    op_args: &["--delete-dir", "sub", "/"],
    method: "POST",
    route: "/api/v1/delete-entry",
    path: "/api/v1/delete-entry",
    ok: delete_dir_ok,
};

op_rule!(op_rule_delete_dir_r1, rule_r1, DELETE_DIR_CASE);
op_rule!(op_rule_delete_dir_r1b, rule_r1b, DELETE_DIR_CASE);
op_rule!(op_rule_delete_dir_r2, rule_r2, DELETE_DIR_CASE);
op_rule!(op_rule_delete_dir_r3, rule_r3, DELETE_DIR_CASE);
op_rule!(op_rule_delete_dir_r4a, rule_r4a, DELETE_DIR_CASE);
op_rule!(op_rule_delete_dir_r4b, rule_r4b, DELETE_DIR_CASE);
op_rule!(op_rule_delete_dir_r7_code, rule_r7_code, DELETE_DIR_CASE);
op_rule!(
    op_rule_delete_dir_r7_no_request,
    rule_r7_no_request,
    DELETE_DIR_CASE
);
op_rule!(op_rule_delete_dir_r8_code, rule_r8_code, DELETE_DIR_CASE);
op_rule!(
    op_rule_delete_dir_r8_no_request,
    rule_r8_no_request,
    DELETE_DIR_CASE
);
op_rule!(op_rule_delete_dir_r9a, rule_r9a, DELETE_DIR_CASE);
op_rule!(op_rule_delete_dir_r9b, rule_r9b, DELETE_DIR_CASE);
op_rule!(op_rule_delete_dir_r9c, rule_r9c, DELETE_DIR_CASE);

// ---- delete_dir (MN-04): operation-specific behavior ----

/// AC-1: one anonymous POST with the TUI's delete body: the normalized parent,
/// the name and `skip_build`, without `is_directory` or `author_username`.
#[test]
fn delete_dir_request_record() {
    let mock = MockMega2::start(delete_dir_clean_ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--delete-dir",
            "sub",
            "/src/",
            "--json",
        ],
        &[],
    );
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(
        request_tuples(&mock),
        vec![(
            "POST".to_string(),
            "/api/v1/delete-entry".to_string(),
            None,
            Some(json!({"path": "/src", "name": "sub", "skip_build": true})),
        )]
    );
}

/// AC-2: `target` comes from local input, `receipt` is the server's receipt.
#[test]
fn delete_dir_json_payload() {
    let mock = MockMega2::start(delete_dir_clean_ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--delete-dir",
            "sub",
            "/src",
            "--json",
        ],
        &[],
    );
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(
        envelope["data"],
        json!({
            "operation": "delete-dir",
            "server": mock.url(),
            "target": {"parent": "/src", "name": "sub", "path": "/src/sub"},
            "receipt": {"commit_id": "commit-1", "path": "/sub", "cl_link": null},
        })
    );
}

/// AC-3: the human summary is one sanitized line.
#[test]
fn delete_dir_human_output() {
    let mock = MockMega2::start(delete_dir_clean_ok);
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--delete-dir",
            "sub",
            "/src",
        ],
        &[],
    );
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        "deleted directory /src/sub (commit commit-1)\n"
    );
}

/// `--delete-dir NAME /` under `--machine` against `mock`.
fn delete_dir_machine_output(mock: &MockMega2, name: &str) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    run(
        dir.path(),
        &[
            "mega2",
            "browser",
            "--server",
            &mock.url(),
            "--delete-dir",
            name,
            "/",
            "--machine",
        ],
        &[],
    )
}

/// G14: NAME `..` (delegated to `validate_entry_name`) fails with `LBR-CLI-002`.
#[test]
fn delete_dir_rejects_dotdot_name_code() {
    let mock = MockMega2::start(delete_dir_clean_ok);
    let output = delete_dir_machine_output(&mock, "..");
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-CLI-002");
}

/// G15: the same refusal sends no request.
#[test]
fn delete_dir_rejects_dotdot_name_no_request() {
    let mock = MockMega2::start(delete_dir_clean_ok);
    let output = delete_dir_machine_output(&mock, "..");
    assert!(!output.status.success());
    assert!(mock.records().is_empty(), "records: {:?}", mock.records());
}

/// mega2 matches a delete by mode: a file named NAME is HTTP 400.
const FILE_TARGET: &str =
    r#"{"req_result":false,"data":null,"err_message":"'sub' is not a directory"}"#;

/// No entry named NAME under the parent is HTTP 404.
const MISSING_TARGET: &str =
    r#"{"req_result":false,"data":null,"err_message":"entry 'sub' not found under /"}"#;

/// G16: a file target (server 400) fails with `LBR-CLI-003`.
#[test]
fn delete_dir_file_target_code() {
    let mock = MockMega2::start(fail_on(&DELETE_DIR_CASE, 400, FILE_TARGET));
    let output = delete_dir_machine_output(&mock, "sub");
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-CLI-003");
}

/// G17: the same failure carries `details.http_status` 400.
#[test]
fn delete_dir_file_target_http_status() {
    let mock = MockMega2::start(fail_on(&DELETE_DIR_CASE, 400, FILE_TARGET));
    let output = delete_dir_machine_output(&mock, "sub");
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["details"]["http_status"], 400);
}

/// G18: a missing target (server 404) fails with `LBR-NET-002`.
#[test]
fn delete_dir_missing_target_code() {
    let mock = MockMega2::start(fail_on(&DELETE_DIR_CASE, 404, MISSING_TARGET));
    let output = delete_dir_machine_output(&mock, "sub");
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-NET-002");
}

/// G19: the same failure carries `details.http_status` 404.
#[test]
fn delete_dir_missing_target_http_status() {
    let mock = MockMega2::start(fail_on(&DELETE_DIR_CASE, 404, MISSING_TARGET));
    let output = delete_dir_machine_output(&mock, "sub");
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["details"]["http_status"], 404);
}

// ---- move_dir (MN-08): R1, R1b, R2, R3, R4a, R4b, R7, R8, R9a, R9b, R9c ----

fn move_entry_receipt(commit_id: &str) -> String {
    envelope(json!({
        "commit_id": commit_id,
        "from_path": "/a",
        "to_path": "/b/a",
        "cl_link": null,
    }))
}

/// The rule fixture's receipt carries an ESC in `commit_id`, so R2 proves the
/// human summary sanitizes server strings.
fn move_dir_ok(_method: &str, _path: &str) -> (u16, String) {
    (200, move_entry_receipt("commit-\u{1b}[31m-1"))
}

fn move_dir_clean_ok(_method: &str, _path: &str) -> (u16, String) {
    (
        200,
        envelope(json!({
            "commit_id": "commit-1",
            "from_path": "/src/sub",
            "to_path": "/dst/sub",
            "cl_link": null,
        })),
    )
}

const MOVE_DIR_CASE: OpCase = OpCase {
    op_args: &["--move-dir", "a", "/b", "/"],
    method: "POST",
    route: "/api/v1/move-entry",
    path: "/api/v1/move-entry",
    ok: move_dir_ok,
};

op_rule!(op_rule_move_dir_r1, rule_r1, MOVE_DIR_CASE);
op_rule!(op_rule_move_dir_r1b, rule_r1b, MOVE_DIR_CASE);
op_rule!(op_rule_move_dir_r2, rule_r2, MOVE_DIR_CASE);
op_rule!(op_rule_move_dir_r3, rule_r3, MOVE_DIR_CASE);
op_rule!(op_rule_move_dir_r4a, rule_r4a, MOVE_DIR_CASE);
op_rule!(op_rule_move_dir_r4b, rule_r4b, MOVE_DIR_CASE);
op_rule!(op_rule_move_dir_r7_code, rule_r7_code, MOVE_DIR_CASE);
op_rule!(
    op_rule_move_dir_r7_no_request,
    rule_r7_no_request,
    MOVE_DIR_CASE
);
op_rule!(op_rule_move_dir_r8_code, rule_r8_code, MOVE_DIR_CASE);
op_rule!(
    op_rule_move_dir_r8_no_request,
    rule_r8_no_request,
    MOVE_DIR_CASE
);
op_rule!(op_rule_move_dir_r9a, rule_r9a, MOVE_DIR_CASE);
op_rule!(op_rule_move_dir_r9b, rule_r9b, MOVE_DIR_CASE);
op_rule!(op_rule_move_dir_r9c, rule_r9c, MOVE_DIR_CASE);

// ---- move_dir (MN-08): operation-specific behavior ----

/// `--move-dir NAME PARENT-PATH PATH` plus `extra` against `mock`.
fn move_dir_output(mock: &MockMega2, name: &str, to_parent: &str, extra: &[&str]) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    let url = mock.url();
    let mut args = vec![
        "mega2",
        "browser",
        "--server",
        &url,
        "--move-dir",
        name,
        to_parent,
    ];
    args.extend_from_slice(extra);
    run(dir.path(), &args, &[])
}

/// AC-1: one anonymous POST with both normalized parents and the kept name.
#[test]
fn move_dir_request_record() {
    let mock = MockMega2::start(move_dir_clean_ok);
    let output = move_dir_output(&mock, "sub", "/dst//", &["/src/", "--json"]);
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(
        request_tuples(&mock),
        vec![(
            "POST".to_string(),
            "/api/v1/move-entry".to_string(),
            None,
            Some(json!({
                "from_path": "/src",
                "from_name": "sub",
                "to_path": "/dst",
                "to_name": "sub",
                "skip_build": true,
            })),
        )]
    );
}

/// AC-2: `target` (source and destination) comes from local input, `receipt`
/// is the server's receipt.
#[test]
fn move_dir_json_payload() {
    let mock = MockMega2::start(move_dir_clean_ok);
    let output = move_dir_output(&mock, "sub", "/dst", &["/src", "--json"]);
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(
        envelope["data"],
        json!({
            "operation": "move-dir",
            "server": mock.url(),
            "target": {
                "from": {"parent": "/src", "name": "sub", "path": "/src/sub"},
                "to": {"parent": "/dst", "name": "sub", "path": "/dst/sub"},
            },
            "receipt": {"commit_id": "commit-1", "from_path": "/src/sub", "to_path": "/dst/sub", "cl_link": null},
        })
    );
}

/// AC-3: the human summary is one sanitized line.
#[test]
fn move_dir_human_output() {
    let mock = MockMega2::start(move_dir_clean_ok);
    let output = move_dir_output(&mock, "sub", "/dst", &["/src"]);
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        "moved directory /src/sub -> /dst/sub (commit commit-1)\n"
    );
}

/// G14: an unrooted PARENT-PATH (delegated to `normalize_path`) fails with
/// `LBR-CLI-003`.
#[test]
fn move_dir_rejects_unrooted_parent_code() {
    let mock = MockMega2::start(move_dir_clean_ok);
    let output = move_dir_output(&mock, "sub", "rel/dir", &["/", "--machine"]);
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-CLI-003");
}

/// G15: the same refusal sends no request.
#[test]
fn move_dir_rejects_unrooted_parent_no_request() {
    let mock = MockMega2::start(move_dir_clean_ok);
    let output = move_dir_output(&mock, "sub", "rel/dir", &["/", "--machine"]);
    assert!(!output.status.success());
    assert!(mock.records().is_empty(), "records: {:?}", mock.records());
}

/// G16: NAME `..` (delegated to `validate_entry_name`) fails with `LBR-CLI-002`.
#[test]
fn move_dir_rejects_dotdot_name_code() {
    let mock = MockMega2::start(move_dir_clean_ok);
    let output = move_dir_output(&mock, "..", "/b", &["/", "--machine"]);
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-CLI-002");
}

/// G17: the same refusal sends no request.
#[test]
fn move_dir_rejects_dotdot_name_no_request() {
    let mock = MockMega2::start(move_dir_clean_ok);
    let output = move_dir_output(&mock, "..", "/b", &["/", "--machine"]);
    assert!(!output.status.success());
    assert!(mock.records().is_empty(), "records: {:?}", mock.records());
}

/// mega2 refuses a move onto an existing name (any mode) with HTTP 400.
const EXISTING_DESTINATION: &str =
    r#"{"req_result":false,"data":null,"err_message":"'a' already exists under /b"}"#;

/// G18: an existing destination (server 400) fails with `LBR-CLI-003`.
#[test]
fn move_dir_existing_destination_code() {
    let mock = MockMega2::start(fail_on(&MOVE_DIR_CASE, 400, EXISTING_DESTINATION));
    let output = move_dir_output(&mock, "a", "/b", &["/", "--machine"]);
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-CLI-003");
}

/// G19: the same failure carries `details.http_status` 400.
#[test]
fn move_dir_existing_destination_http_status() {
    let mock = MockMega2::start(fail_on(&MOVE_DIR_CASE, 400, EXISTING_DESTINATION));
    let output = move_dir_output(&mock, "a", "/b", &["/", "--machine"]);
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["details"]["http_status"], 400);
}

// ---- rename_dir (MN-12): R1, R1b, R2, R3, R4a, R4b, R7, R8, R9a, R9b, R9c ----

/// The rule fixture's receipt carries an ESC in `commit_id`, so R2 proves the
/// human summary sanitizes server strings.
fn rename_dir_ok(_method: &str, _path: &str) -> (u16, String) {
    (
        200,
        envelope(json!({
            "commit_id": "commit-\u{1b}[31m-1",
            "from_path": "/a",
            "to_path": "/b",
            "cl_link": null,
        })),
    )
}

fn rename_dir_clean_ok(_method: &str, _path: &str) -> (u16, String) {
    (
        200,
        envelope(json!({
            "commit_id": "commit-1",
            "from_path": "/src/old",
            "to_path": "/src/new",
            "cl_link": null,
        })),
    )
}

const RENAME_DIR_CASE: OpCase = OpCase {
    op_args: &["--rename-dir", "a", "b", "/"],
    method: "POST",
    route: "/api/v1/move-entry",
    path: "/api/v1/move-entry",
    ok: rename_dir_ok,
};

op_rule!(op_rule_rename_dir_r1, rule_r1, RENAME_DIR_CASE);
op_rule!(op_rule_rename_dir_r1b, rule_r1b, RENAME_DIR_CASE);
op_rule!(op_rule_rename_dir_r2, rule_r2, RENAME_DIR_CASE);
op_rule!(op_rule_rename_dir_r3, rule_r3, RENAME_DIR_CASE);
op_rule!(op_rule_rename_dir_r4a, rule_r4a, RENAME_DIR_CASE);
op_rule!(op_rule_rename_dir_r4b, rule_r4b, RENAME_DIR_CASE);
op_rule!(op_rule_rename_dir_r7_code, rule_r7_code, RENAME_DIR_CASE);
op_rule!(
    op_rule_rename_dir_r7_no_request,
    rule_r7_no_request,
    RENAME_DIR_CASE
);
op_rule!(op_rule_rename_dir_r8_code, rule_r8_code, RENAME_DIR_CASE);
op_rule!(
    op_rule_rename_dir_r8_no_request,
    rule_r8_no_request,
    RENAME_DIR_CASE
);
op_rule!(op_rule_rename_dir_r9a, rule_r9a, RENAME_DIR_CASE);
op_rule!(op_rule_rename_dir_r9b, rule_r9b, RENAME_DIR_CASE);
op_rule!(op_rule_rename_dir_r9c, rule_r9c, RENAME_DIR_CASE);

// ---- rename_dir (MN-12): operation-specific behavior ----

/// `--rename-dir NAME NEW-NAME PATH` plus `extra` against `mock`.
fn rename_dir_output(mock: &MockMega2, name: &str, new_name: &str, extra: &[&str]) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    let url = mock.url();
    let mut args = vec![
        "mega2",
        "browser",
        "--server",
        &url,
        "--rename-dir",
        name,
        new_name,
    ];
    args.extend_from_slice(extra);
    run(dir.path(), &args, &[])
}

/// AC-1: one anonymous same-parent POST to move-entry.
#[test]
fn rename_dir_request_record() {
    let mock = MockMega2::start(rename_dir_clean_ok);
    let output = rename_dir_output(&mock, "old", "new", &["/src/", "--json"]);
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(
        request_tuples(&mock),
        vec![(
            "POST".to_string(),
            "/api/v1/move-entry".to_string(),
            None,
            Some(json!({
                "from_path": "/src",
                "from_name": "old",
                "to_path": "/src",
                "to_name": "new",
                "skip_build": true,
            })),
        )]
    );
}

/// AC-2: `target.to.parent` is `target.from.parent`; `receipt` is the
/// server's receipt.
#[test]
fn rename_dir_json_payload() {
    let mock = MockMega2::start(rename_dir_clean_ok);
    let output = rename_dir_output(&mock, "old", "new", &["/src", "--json"]);
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(
        envelope["data"],
        json!({
            "operation": "rename-dir",
            "server": mock.url(),
            "target": {
                "from": {"parent": "/src", "name": "old", "path": "/src/old"},
                "to": {"parent": "/src", "name": "new", "path": "/src/new"},
            },
            "receipt": {"commit_id": "commit-1", "from_path": "/src/old", "to_path": "/src/new", "cl_link": null},
        })
    );
}

/// AC-3: the human summary is one sanitized line.
#[test]
fn rename_dir_human_output() {
    let mock = MockMega2::start(rename_dir_clean_ok);
    let output = rename_dir_output(&mock, "old", "new", &["/src"]);
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        "renamed directory /src/old -> /src/new (commit commit-1)\n"
    );
}

/// G14: NAME `..` (delegated to `validate_entry_name`) fails with `LBR-CLI-002`.
#[test]
fn rename_dir_rejects_dotdot_name_code() {
    let mock = MockMega2::start(rename_dir_clean_ok);
    let output = rename_dir_output(&mock, "..", "b", &["/", "--machine"]);
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-CLI-002");
}

/// G15: the same refusal sends no request.
#[test]
fn rename_dir_rejects_dotdot_name_no_request() {
    let mock = MockMega2::start(rename_dir_clean_ok);
    let output = rename_dir_output(&mock, "..", "b", &["/", "--machine"]);
    assert!(!output.status.success());
    assert!(mock.records().is_empty(), "records: {:?}", mock.records());
}

/// G16: NEW-NAME `..` (delegated to `validate_entry_name`) fails with
/// `LBR-CLI-002`.
#[test]
fn rename_dir_rejects_dotdot_new_name_code() {
    let mock = MockMega2::start(rename_dir_clean_ok);
    let output = rename_dir_output(&mock, "a", "..", &["/", "--machine"]);
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-CLI-002");
}

/// G17: the same refusal sends no request.
#[test]
fn rename_dir_rejects_dotdot_new_name_no_request() {
    let mock = MockMega2::start(rename_dir_clean_ok);
    let output = rename_dir_output(&mock, "a", "..", &["/", "--machine"]);
    assert!(!output.status.success());
    assert!(mock.records().is_empty(), "records: {:?}", mock.records());
}

/// mega2 refuses a move whose source and destination are the same (HTTP 400).
const SAME_NAME: &str =
    r#"{"req_result":false,"data":null,"err_message":"source and destination are the same: /a"}"#;

/// G18: NEW-NAME equal to NAME (server 400) fails with `LBR-CLI-003`.
#[test]
fn rename_dir_same_name_code() {
    let mock = MockMega2::start(fail_on(&RENAME_DIR_CASE, 400, SAME_NAME));
    let output = rename_dir_output(&mock, "a", "a", &["/", "--machine"]);
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-CLI-003");
}

/// G19: the same failure carries `details.http_status` 400.
#[test]
fn rename_dir_same_name_http_status() {
    let mock = MockMega2::start(fail_on(&RENAME_DIR_CASE, 400, SAME_NAME));
    let output = rename_dir_output(&mock, "a", "a", &["/", "--machine"]);
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["details"]["http_status"], 400);
}

// ---- list_tags (MN-05): R1, R1b, R2, R3, R4a, R4b, R5a, R5b, R6, R7, R10a, R10b ----

fn tags_page(total: u64, items: Value) -> String {
    envelope(json!({"total": total, "items": items}))
}

/// The rule fixture's tagger carries an ESC, so R2 proves the human output
/// sanitizes server strings.
fn list_tags_ok(_method: &str, _path: &str) -> (u16, String) {
    (
        200,
        tags_page(
            1,
            json!([{
                "name": "v1",
                "tag_id": "t-1",
                "object_id": "o-1",
                "object_type": "commit",
                "tagger": "Ann \u{1b}[31m<ann@example.com>",
                "message": "first",
                "created_at": "2026-10-01T00:00:00Z",
            }]),
        ),
    )
}

const LIST_TAGS_CASE: OpCase = OpCase {
    op_args: &["--list-tags"],
    method: "GET",
    route: "/api/v1/tags/list",
    path: "/api/v1/tags/list",
    ok: list_tags_ok,
};

fn r10a_output(case: &OpCase, mock: &MockMega2) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    run(
        dir.path(),
        &argv(&mock.url(), case, &["--machine"], &["/x"]),
        &[],
    )
}

/// R10a ①: a tag operation with a PATH other than `/` fails with `LBR-CLI-002`.
fn rule_r10a_code(case: &OpCase) {
    let mock = MockMega2::start(case.ok);
    let output = r10a_output(case, &mock);
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-CLI-002");
}

/// R10a ②: the refusal sends no request.
fn rule_r10a_no_request(case: &OpCase) {
    let mock = MockMega2::start(case.ok);
    let output = r10a_output(case, &mock);
    assert!(!output.status.success());
    assert!(mock.records().is_empty(), "records: {:?}", mock.records());
}

fn r10b_output(case: &OpCase, mock: &MockMega2) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    run(
        dir.path(),
        &argv(&mock.url(), case, &["--machine"], &["--ref", "v1"]),
        &[],
    )
}

/// R10b ①: a read tag operation with `--ref` fails with `LBR-CLI-002`.
fn rule_r10b_code(case: &OpCase) {
    let mock = MockMega2::start(case.ok);
    let output = r10b_output(case, &mock);
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-CLI-002");
}

/// R10b ②: the refusal sends no request.
fn rule_r10b_no_request(case: &OpCase) {
    let mock = MockMega2::start(case.ok);
    let output = r10b_output(case, &mock);
    assert!(!output.status.success());
    assert!(mock.records().is_empty(), "records: {:?}", mock.records());
}

op_rule!(op_rule_list_tags_r1, rule_r1, LIST_TAGS_CASE);
op_rule!(op_rule_list_tags_r1b, rule_r1b, LIST_TAGS_CASE);
op_rule!(op_rule_list_tags_r2, rule_r2, LIST_TAGS_CASE);
op_rule!(op_rule_list_tags_r3, rule_r3, LIST_TAGS_CASE);
op_rule!(op_rule_list_tags_r4a, rule_r4a, LIST_TAGS_CASE);
op_rule!(op_rule_list_tags_r4b, rule_r4b, LIST_TAGS_CASE);
op_rule!(op_rule_list_tags_r5a_code, rule_r5a_code, LIST_TAGS_CASE);
op_rule!(
    op_rule_list_tags_r5a_no_request,
    rule_r5a_no_request,
    LIST_TAGS_CASE
);
op_rule!(op_rule_list_tags_r5b_code, rule_r5b_code, LIST_TAGS_CASE);
op_rule!(
    op_rule_list_tags_r5b_no_request,
    rule_r5b_no_request,
    LIST_TAGS_CASE
);
op_rule!(op_rule_list_tags_r6, rule_r6, LIST_TAGS_CASE);
op_rule!(op_rule_list_tags_r7_code, rule_r7_code, LIST_TAGS_CASE);
op_rule!(
    op_rule_list_tags_r7_no_request,
    rule_r7_no_request,
    LIST_TAGS_CASE
);
op_rule!(op_rule_list_tags_r10a_code, rule_r10a_code, LIST_TAGS_CASE);
op_rule!(
    op_rule_list_tags_r10a_no_request,
    rule_r10a_no_request,
    LIST_TAGS_CASE
);
op_rule!(op_rule_list_tags_r10b_code, rule_r10b_code, LIST_TAGS_CASE);
op_rule!(
    op_rule_list_tags_r10b_no_request,
    rule_r10b_no_request,
    LIST_TAGS_CASE
);

// ---- list_tags (MN-05): operation-specific behavior ----

/// `--list-tags` plus `extra` against `mock`.
fn list_tags_output(mock: &MockMega2, extra: &[&str]) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    let url = mock.url();
    let mut args = vec!["mega2", "browser", "--server", &url, "--list-tags"];
    args.extend_from_slice(extra);
    run(dir.path(), &args, &[])
}

/// AC-1: explicit paging is sent as given, with the root path selector.
#[test]
fn list_tags_request_record_explicit_paging() {
    let mock = MockMega2::start(list_tags_ok);
    let output = list_tags_output(&mock, &["--page", "3", "--per-page", "50", "--json"]);
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(
        request_tuples(&mock),
        vec![(
            "GET".to_string(),
            "/api/v1/tags/list?page=3&per_page=50&path=%2F".to_string(),
            None,
            None,
        )]
    );
}

/// AC-2: without `--page`/`--per-page` the TUI panel's defaults are sent.
#[test]
fn list_tags_request_record_default_paging() {
    let mock = MockMega2::start(list_tags_ok);
    let output = list_tags_output(&mock, &["--json"]);
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(
        request_tuples(&mock),
        vec![(
            "GET".to_string(),
            "/api/v1/tags/list?page=1&per_page=20&path=%2F".to_string(),
            None,
            None,
        )]
    );
}

/// A page of two tags, one with ESC and BEL in its tagger and message.
fn list_tags_hostile_ok(_method: &str, _path: &str) -> (u16, String) {
    (
        200,
        tags_page(
            5,
            json!([
                {
                    "name": "v2",
                    "tag_id": "t-2",
                    "object_id": "o-2",
                    "object_type": "tag",
                    "tagger": "Ann\u{1b}[31m\u{7}",
                    "message": "release\u{7}notes\u{1b}[0m",
                    "created_at": "2026-10-02T00:00:00Z",
                },
                {
                    "name": "v1",
                    "tag_id": "o-1",
                    "object_id": "o-1",
                    "object_type": "commit",
                    "tagger": "",
                    "message": "",
                    "created_at": "2026-10-01T00:00:00Z",
                },
            ]),
        ),
    )
}

/// AC-3: the payload carries the paging, the server's total, `has_next`
/// (`page * per_page < total`) and every item field verbatim.
#[test]
fn list_tags_json_payload() {
    let mock = MockMega2::start(list_tags_hostile_ok);
    let output = list_tags_output(&mock, &["--page", "2", "--per-page", "2", "--json"]);
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("stdout is JSON");
    assert_eq!(
        envelope["data"],
        json!({
            "operation": "list-tags",
            "server": mock.url(),
            "path": "/",
            "page": 2,
            "per_page": 2,
            "total": 5,
            "has_next": true,
            "items": [
                {
                    "name": "v2",
                    "tag_id": "t-2",
                    "object_id": "o-2",
                    "object_type": "tag",
                    "tagger": "Ann\u{1b}[31m\u{7}",
                    "message": "release\u{7}notes\u{1b}[0m",
                    "created_at": "2026-10-02T00:00:00Z",
                },
                {
                    "name": "v1",
                    "tag_id": "o-1",
                    "object_id": "o-1",
                    "object_type": "commit",
                    "tagger": "",
                    "message": "",
                    "created_at": "2026-10-01T00:00:00Z",
                },
            ],
        })
    );
}

/// AC-4: one line per tag, the message indented on the next line, then the
/// paging line; control characters are rendered as `?`.
#[test]
fn list_tags_human_output_is_sanitized() {
    let mock = MockMega2::start(list_tags_hostile_ok);
    let output = list_tags_output(&mock, &[]);
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        "v2  tag  Ann?[31m?\n    release?notes?[0m\nv1  commit  \npage 1 · per_page 20 · total 5\n"
    );
}

/// Runs `args` (after `--server <url>`) under `--machine` against `mock`.
fn tag_paging_output(mock: &MockMega2, args: &[&str]) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    let url = mock.url();
    let mut argv = vec!["mega2", "browser", "--server", &url];
    argv.extend_from_slice(args);
    argv.push("--machine");
    run(dir.path(), &argv, &[])
}

/// G18: `--page 0` (delegated to `validate_pagination`) fails with `LBR-CLI-002`.
#[test]
fn list_tags_rejects_page_zero_code() {
    let mock = MockMega2::start(list_tags_ok);
    let output = tag_paging_output(&mock, &["--list-tags", "--page", "0"]);
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-CLI-002");
}

/// G19: the same refusal sends no request.
#[test]
fn list_tags_rejects_page_zero_no_request() {
    let mock = MockMega2::start(list_tags_ok);
    let output = tag_paging_output(&mock, &["--list-tags", "--page", "0"]);
    assert!(!output.status.success());
    assert!(mock.records().is_empty(), "records: {:?}", mock.records());
}

/// G20: `--per-page 101` fails with `LBR-CLI-002`.
#[test]
fn list_tags_rejects_per_page_101_code() {
    let mock = MockMega2::start(list_tags_ok);
    let output = tag_paging_output(&mock, &["--list-tags", "--per-page", "101"]);
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-CLI-002");
}

/// G21: the same refusal sends no request.
#[test]
fn list_tags_rejects_per_page_101_no_request() {
    let mock = MockMega2::start(list_tags_ok);
    let output = tag_paging_output(&mock, &["--list-tags", "--per-page", "101"]);
    assert!(!output.status.success());
    assert!(mock.records().is_empty(), "records: {:?}", mock.records());
}

/// G22: `--page` without `--list-tags` fails with `LBR-CLI-002`.
#[test]
fn list_tags_page_requires_list_tags_code() {
    let mock = MockMega2::start(list_tags_ok);
    let output = tag_paging_output(&mock, &["--page", "2"]);
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-CLI-002");
}

/// G23: the same refusal sends no request.
#[test]
fn list_tags_page_requires_list_tags_no_request() {
    let mock = MockMega2::start(list_tags_ok);
    let output = tag_paging_output(&mock, &["--page", "2"]);
    assert!(!output.status.success());
    assert!(mock.records().is_empty(), "records: {:?}", mock.records());
}

/// G24: `--per-page` without `--list-tags` fails with `LBR-CLI-002`.
#[test]
fn list_tags_per_page_requires_list_tags_code() {
    let mock = MockMega2::start(list_tags_ok);
    let output = tag_paging_output(&mock, &["--per-page", "50"]);
    assert!(!output.status.success());
    assert_eq!(error_envelope(&output)["error_code"], "LBR-CLI-002");
}

/// G25: the same refusal sends no request.
#[test]
fn list_tags_per_page_requires_list_tags_no_request() {
    let mock = MockMega2::start(list_tags_ok);
    let output = tag_paging_output(&mock, &["--per-page", "50"]);
    assert!(!output.status.success());
    assert!(mock.records().is_empty(), "records: {:?}", mock.records());
}
