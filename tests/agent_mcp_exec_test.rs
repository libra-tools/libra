//! Read-worker executor gates (plan-20261019 MC-29).

use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::Path,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

fn isolated_command(args: &[&str], cwd: &Path, extra: &[(&str, &str)]) -> Command {
    let home = cwd.join(".libra-test-home");
    let config_home = home.join(".config");
    fs::create_dir_all(&config_home).expect("isolated config dir");
    let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env(
            "LIBRA_CONFIG_GLOBAL_DB",
            home.join(".libra").join("config.db"),
        )
        .env(
            "LIBRA_CONFIG_SYSTEM_DB",
            home.join(".libra").join("system-config.db"),
        )
        .env("LIBRA_HOME", home.join(".libra"))
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("LIBRA_TEST", "1");
    for (key, value) in extra {
        command.env(key, value);
    }
    command
}

fn init_repo(object_format: Option<&str>) -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("tempdir");
    let mut args = vec!["init"];
    if let Some(format) = object_format {
        args.extend(["--object-format", format]);
    }
    let output = isolated_command(&args, repo.path(), &[])
        .output()
        .expect("run init");
    assert!(
        output.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    repo
}

fn run_mcp(cwd: &Path, stdin_body: &str, extra: &[(&str, &str)]) -> Output {
    let mut child = isolated_command(&["agent", "mcp", "--stdio"], cwd, extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn libra agent mcp");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin_body.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait for mcp")
}

fn session(calls: &str) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{{}},\"clientInfo\":{{\"name\":\"test\",\"version\":\"0\"}}}}}}\n{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}}\n{calls}\n"
    )
}

fn response_with_id(stdout: &str, id: i64) -> Value {
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|message| message.get("id") == Some(&Value::from(id)))
        .expect("jsonrpc response")
}

fn tool_call(id: i64, name: &str, arguments: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{name}","arguments":{arguments}}}}}"#
    )
}

fn block_env() -> Vec<(&'static str, &'static str)> {
    vec![
        ("LIBRA_MCP_TEST_BLOCK", "1"),
        ("LIBRA_MCP_TEST_READ_DEADLINE_MS", "2000"),
    ]
}

#[test]
fn blocking_read_times_out() {
    let repo = init_repo(None);
    let pid_file = repo.path().join("block.pid");
    let call = tool_call(
        2,
        "libra_test_block",
        &format!(r#"{{"pid_file":"{}"}}"#, pid_file.display()),
    );
    let started = Instant::now();
    let output = run_mcp(repo.path(), &session(&call), &block_env());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "block call took {:?}",
        started.elapsed()
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let message = response_with_id(&stdout, 2);
    assert_eq!(
        message["result"]["structuredContent"]["stable_code"],
        "LBR-AGENT-036"
    );
}

#[test]
fn timed_out_worker_is_killed() {
    let repo = init_repo(None);
    let pid_file = repo.path().join("block.pid");
    let call = tool_call(
        2,
        "libra_test_block",
        &format!(r#"{{"pid_file":"{}"}}"#, pid_file.display()),
    );
    let output = run_mcp(repo.path(), &session(&call), &block_env());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pid: u32 = fs::read_to_string(&pid_file)
        .expect("pid file")
        .trim()
        .parse()
        .expect("pid");
    let started = Instant::now();
    while pid_exists(pid) && started.elapsed() < Duration::from_secs(1) {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!pid_exists(pid), "worker pid {pid} is still alive");
}

#[test]
#[cfg(unix)]
fn deadline_reply_does_not_wait_for_stdout_holder() {
    let repo = init_repo(None);
    let pid_file = repo.path().join("block.pid");
    let call = tool_call(
        2,
        "libra_test_block",
        &format!(r#"{{"pid_file":"{}"}}"#, pid_file.display()),
    );
    let mut extra = block_env();
    extra.push(("LIBRA_MCP_TEST_HOLD_STDOUT", "1"));
    let mut child = isolated_command(&["agent", "mcp", "--stdio"], repo.path(), &extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn libra agent mcp");
    let mcp_pid = child.id();
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(session(&call).as_bytes())
        .expect("write stdin");
    let started = Instant::now();
    let waiter = thread::spawn(move || child.wait_with_output());
    while !waiter.is_finished() && started.elapsed() < Duration::from_secs(5) {
        thread::sleep(Duration::from_millis(20));
    }
    if !waiter.is_finished() {
        let _ = Command::new("kill")
            .args(["-9", &mcp_pid.to_string()])
            .status();
        let _ = waiter.join();
        panic!("deadline reply blocked for {:?}", started.elapsed());
    }
    let output = waiter.join().expect("waiter").expect("mcp output");
    let hold_path = repo.path().join("block.pid.hold");
    let hold_pid = fs::read_to_string(&hold_path)
        .expect("hold pid")
        .trim()
        .parse::<i32>()
        .expect("hold pid number");
    let holder = KillHold(hold_pid);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "reply took {:?}",
        started.elapsed()
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let message = response_with_id(&stdout, 2);
    assert_eq!(
        message["result"]["structuredContent"]["stable_code"],
        "LBR-AGENT-036"
    );
    assert!(
        pid_exists(holder.0 as u32),
        "stdout holder {} died with the worker",
        holder.0
    );
}

#[cfg(unix)]
struct KillHold(i32);

#[cfg(unix)]
impl Drop for KillHold {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-9", &self.0.to_string()])
            .status();
    }
}

#[cfg(unix)]
fn pid_exists(pid: u32) -> bool {
    // SAFETY: signal 0 only queries whether the process exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(windows)]
fn pid_exists(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, STILL_ACTIVE},
        System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };

    // SAFETY: open a query-only handle, inspect the exit code, then close it.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut exit_code = 0u32;
        let queried = GetExitCodeProcess(handle, &mut exit_code);
        let _ = CloseHandle(handle);
        queried != 0 && exit_code == STILL_ACTIVE as u32
    }
}

#[test]
fn slots_return_after_workers_killed() {
    let repo = init_repo(None);
    let mut calls = String::new();
    for id in 2..=5 {
        let pid_file = repo.path().join(format!("block-{id}.pid"));
        calls.push_str(&tool_call(
            id,
            "libra_test_block",
            &format!(r#"{{"pid_file":"{}"}}"#, pid_file.display()),
        ));
        calls.push('\n');
    }
    calls.push_str(&tool_call(6, "libra_test_sleep", r#"{"ms":10}"#));
    let started = Instant::now();
    let output = run_mcp(repo.path(), &session(&calls), &block_env());
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "concurrent blocks took {:?}",
        started.elapsed()
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    for id in 2..=5 {
        let message = response_with_id(&stdout, id);
        assert_eq!(
            message["result"]["structuredContent"]["stable_code"],
            "LBR-AGENT-036"
        );
    }
    let sleep = response_with_id(&stdout, 6);
    assert_eq!(sleep["result"]["isError"], false);
    assert_eq!(sleep["result"]["structuredContent"]["slept_ms"], 10);
}

#[cfg(target_os = "linux")]
#[test]
fn read_worker_runtime_is_bounded_on_large_hosts() {
    let repo = init_repo(Some("sha256"));
    let output = run_mcp(
        repo.path(),
        &session(&tool_call(2, "libra_test_hash_kind", "{}")),
        &[("TOKIO_WORKER_THREADS", "64")],
    );
    assert!(
        output.status.success(),
        "MCP server failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let message = response_with_id(&stdout, 2);
    assert_eq!(
        message["result"]["structuredContent"]["hash_kind"], "sha256",
        "MCP_WORKER_RUNTIME: high host thread count must leave room for the read-only repository connection: {message}"
    );
    assert_eq!(message["result"]["isError"], false);
}

#[test]
fn read_worker_uses_repo_hash_kind() {
    let repo = init_repo(Some("sha256"));
    let output = run_mcp(
        repo.path(),
        &session(&tool_call(2, "libra_test_hash_kind", "{}")),
        &[],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let message = response_with_id(&stdout, 2);
    assert_eq!(
        message["result"]["structuredContent"]["hash_kind"],
        "sha256"
    );
}

fn db_path(repo: &Path) -> std::path::PathBuf {
    repo.join(".libra").join("libra.db")
}

fn max_schema(db: &Path) -> i64 {
    let output = Command::new("python3")
        .args([
            "-c",
            "import sqlite3,sys; con=sqlite3.connect(sys.argv[1]); print(con.execute('SELECT MAX(version) FROM schema_versions').fetchone()[0])",
            db.to_str().expect("utf8 db"),
        ])
        .output()
        .expect("python schema");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf8")
        .trim()
        .parse()
        .expect("version")
}

fn rollback_schema(db: &Path) {
    let output = Command::new("python3")
        .args([
            "-c",
            "import sqlite3,sys; con=sqlite3.connect(sys.argv[1]); con.execute('DELETE FROM schema_versions WHERE version=(SELECT MAX(version) FROM schema_versions)'); con.commit()",
            db.to_str().expect("utf8 db"),
        ])
        .output()
        .expect("python rollback");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn read_worker_refuses_outdated_schema() {
    let repo = init_repo(None);
    let db = db_path(repo.path());
    rollback_schema(&db);
    let output = run_mcp(
        repo.path(),
        &session(&tool_call(2, "libra_test_hash_kind", "{}")),
        &[],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let message = response_with_id(&stdout, 2);
    assert_eq!(
        message["result"]["structuredContent"]["stable_code"],
        "LBR-AGENT-037"
    );
}

#[test]
fn read_worker_does_not_migrate() {
    let repo = init_repo(None);
    let db = db_path(repo.path());
    rollback_schema(&db);
    let before = max_schema(&db);
    let _ = run_mcp(
        repo.path(),
        &session(&tool_call(2, "libra_test_hash_kind", "{}")),
        &[],
    );
    assert_eq!(max_schema(&db), before);
}

#[test]
fn read_worker_does_not_repair_object_index() {
    let repo = init_repo(None);
    let marker_dir = repo.path().join(".libra").join("object-index-repair");
    fs::create_dir_all(&marker_dir).expect("marker dir");
    let marker = marker_dir.join("keep");
    fs::write(&marker, b"keep-this-marker").expect("marker");
    let before = fs::read(&marker).expect("read");
    let modified = fs::metadata(&marker)
        .expect("meta")
        .modified()
        .expect("mtime");
    thread::sleep(Duration::from_millis(20));
    let output = run_mcp(
        repo.path(),
        &session(&tool_call(2, "libra_test_hash_kind", "{}")),
        &[],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&marker).expect("after"), before);
    assert_eq!(
        fs::metadata(&marker)
            .expect("meta")
            .modified()
            .expect("mtime"),
        modified
    );
}

#[test]
fn read_worker_skips_upgrade_check() {
    let repo = init_repo(None);
    let upgrade = repo
        .path()
        .join(".libra-test-home")
        .join(".libra")
        .join("upgrade");
    fs::create_dir_all(&upgrade).expect("upgrade dir");
    let sentinel = upgrade.join("sentinel");
    fs::write(&sentinel, b"untouched").expect("sentinel");
    let before = fs::read(&sentinel).expect("read");
    let modified = fs::metadata(&sentinel)
        .expect("meta")
        .modified()
        .expect("mtime");
    let names_before: Vec<_> = fs::read_dir(&upgrade)
        .expect("dir")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    thread::sleep(Duration::from_millis(20));
    let output = run_mcp(
        repo.path(),
        &session(&tool_call(2, "libra_test_hash_kind", "{}")),
        &[],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&sentinel).expect("after"), before);
    assert_eq!(
        fs::metadata(&sentinel)
            .expect("meta")
            .modified()
            .expect("mtime"),
        modified
    );
    let names_after: Vec<_> = fs::read_dir(&upgrade)
        .expect("dir")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(names_before, names_after);
}

fn memory_env(peak: &Path) -> Vec<(&str, String)> {
    vec![
        ("LIBRA_MCP_TEST_WORKER_MEM_MB", "256".to_string()),
        ("LIBRA_MCP_TEST_PEAK_RSS_FILE", peak.display().to_string()),
    ]
}

fn run_mcp_owned(cwd: &Path, stdin_body: &str, extra: &[(&str, String)]) -> Output {
    let pairs: Vec<(&str, &str)> = extra
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    run_mcp(cwd, stdin_body, &pairs)
}

fn descendant_pids(root: u32) -> Vec<u32> {
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        let path = format!("/proc/{pid}/task/{pid}/children");
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        for token in text.split_whitespace() {
            if let Ok(child) = token.parse::<u32>() {
                found.push(child);
                stack.push(child);
            }
        }
    }
    found
}

#[test]
fn worker_memory_limit_is_tool_error() {
    let repo = init_repo(None);
    let pid_file = repo.path().join("alloc.pid");
    let peak = repo.path().join("peak");
    let env = memory_env(&peak);
    let call = tool_call(
        2,
        "libra_test_alloc",
        &format!(r#"{{"mb":1024,"pid_file":"{}"}}"#, pid_file.display()),
    );
    let output = run_mcp_owned(repo.path(), &session(&call), &env);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let message = response_with_id(&stdout, 2);
    assert_eq!(
        message["result"]["structuredContent"]["stable_code"], "LBR-AGENT-028",
        "{message}"
    );
}

#[test]
fn worker_memory_limit_reason() {
    let repo = init_repo(None);
    let pid_file = repo.path().join("alloc.pid");
    let peak = repo.path().join("peak");
    let call = tool_call(
        2,
        "libra_test_alloc",
        &format!(r#"{{"mb":1024,"pid_file":"{}"}}"#, pid_file.display()),
    );
    let output = run_mcp_owned(repo.path(), &session(&call), &memory_env(&peak));
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let message = response_with_id(&stdout, 2);
    assert_eq!(
        message["result"]["structuredContent"]["reason"],
        "worker_memory_limit"
    );
}

#[test]
fn worker_over_memory_is_killed() {
    let repo = init_repo(None);
    let pid_file = repo.path().join("alloc.pid");
    let peak = repo.path().join("peak");
    let call = tool_call(
        2,
        "libra_test_alloc",
        &format!(r#"{{"mb":1024,"pid_file":"{}"}}"#, pid_file.display()),
    );
    let output = run_mcp_owned(repo.path(), &session(&call), &memory_env(&peak));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pid: u32 = fs::read_to_string(&pid_file)
        .expect("pid")
        .trim()
        .parse()
        .expect("pid");
    let started = Instant::now();
    while pid_exists(pid) && started.elapsed() < Duration::from_secs(1) {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!pid_exists(pid), "worker pid {pid} is still alive");
}

#[test]
fn worker_peak_memory_bounded() {
    let repo = init_repo(None);
    let pid_file = repo.path().join("alloc.pid");
    let peak_file = repo.path().join("peak");
    let call = tool_call(
        2,
        "libra_test_alloc",
        &format!(r#"{{"mb":1024,"pid_file":"{}"}}"#, pid_file.display()),
    );
    let output = run_mcp_owned(repo.path(), &session(&call), &memory_env(&peak_file));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let peak: u64 = fs::read_to_string(&peak_file)
        .expect("peak")
        .trim()
        .parse()
        .expect("peak bytes");
    let limit = 256 * 1024 * 1024;
    let tolerance = 128 * 1024 * 1024;
    assert!(
        peak <= limit + tolerance,
        "peak rss {peak} exceeds limit plus tolerance"
    );
}

#[test]
fn slots_return_after_memory_kills() {
    let repo = init_repo(None);
    let peak = repo.path().join("peak");
    let mut calls = String::new();
    for id in 2..=5 {
        let pid_file = repo.path().join(format!("alloc-{id}.pid"));
        calls.push_str(&tool_call(
            id,
            "libra_test_alloc",
            &format!(r#"{{"mb":1024,"pid_file":"{}"}}"#, pid_file.display()),
        ));
        calls.push('\n');
    }
    calls.push_str(&tool_call(6, "libra_test_sleep", r#"{"ms":10}"#));
    let output = run_mcp_owned(repo.path(), &session(&calls), &memory_env(&peak));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    for id in 2..=5 {
        let message = response_with_id(&stdout, id);
        assert_eq!(
            message["result"]["structuredContent"]["stable_code"], "LBR-AGENT-028",
            "{message}"
        );
    }
    let sleep = response_with_id(&stdout, 6);
    assert_eq!(sleep["result"]["isError"], false, "{sleep}");
}

#[test]
fn read_worker_status_spawns_no_helpers() {
    let repo = init_repo(None);
    let pause_file = repo.path().join("pause.pid");
    let call = tool_call(2, "libra_test_status", "{}");
    let mut child = isolated_command(&["agent", "mcp", "--stdio"], repo.path(), &[])
        .env("LIBRA_MCP_TEST_STATUS_PAUSE", "1")
        .env(
            "LIBRA_MCP_TEST_STATUS_PAUSE_FILE",
            pause_file.display().to_string(),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut guard = OwnedPauseCleanup::arm(child);
    stdin.write_all(session(&call).as_bytes()).expect("write");
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        guard.observe_children();
        if let Ok(text) = fs::read_to_string(&pause_file)
            && text.trim().parse::<u32>().is_ok()
        {
            break;
        }
        if guard.parent_exited().expect("wait") {
            drop(stdin);
            panic!("mcp exited before the status pause file existed");
        }
        thread::sleep(Duration::from_millis(20));
    }
    drop(stdin);
    guard.observe_children();
    let text = match fs::read_to_string(&pause_file) {
        Ok(text) => text,
        Err(error) => panic!("status pause file was not written: {error}"),
    };
    let worker: u32 = match text.trim().parse() {
        Ok(pid) => pid,
        Err(error) => panic!("pause text {text:?} could not be parsed: {error}"),
    };
    guard
        .bind_worker(worker)
        .unwrap_or_else(|error| panic!("pause pid {worker} is not an owned child: {error}"));
    assert!(
        descendant_pids(worker).is_empty(),
        "read worker {worker} spawned helpers: {:?}",
        descendant_pids(worker)
    );
    let proof = guard.finish();
    assert!(
        proof.checked_worker_after_parent_wait,
        "parent wait was treated as worker exit"
    );
    assert!(
        proof.worker_not_live,
        "owned worker {worker} was still live after cleanup; reaped={} zombie={}",
        proof.worker_reaped, proof.worker_zombie
    );
    #[cfg(unix)]
    assert!(
        proof.own_group,
        "owned worker {worker} was not the leader of its own process group"
    );
}

struct ProcessView {
    start: u64,
    state_zombie: bool,
    pgrp: u32,
    ppid: u32,
}

#[derive(Clone, Copy)]
struct OwnedWorker {
    pid: u32,
    start: u64,
    own_group: bool,
}

struct PauseCleanupProof {
    own_group: bool,
    worker_not_live: bool,
    worker_reaped: bool,
    worker_zombie: bool,
    checked_worker_after_parent_wait: bool,
}

struct OwnedPauseCleanup {
    child: Option<Child>,
    parent_pid: u32,
    parent_start: u64,
    observed: HashMap<u32, u64>,
    worker: Option<OwnedWorker>,
    finished: bool,
}

impl OwnedPauseCleanup {
    fn arm(child: Child) -> Self {
        let parent_pid = child.id();
        let parent_start = process_view(parent_pid)
            .map(|view| view.start)
            .unwrap_or_else(|| panic!("mcp parent {parent_pid} has no process identity"));
        Self {
            child: Some(child),
            parent_pid,
            parent_start,
            observed: HashMap::new(),
            worker: None,
            finished: false,
        }
    }

    fn observe_children(&mut self) {
        for pid in child_pids(self.parent_pid) {
            if pid <= 1 {
                continue;
            }
            if let Some(view) = process_view(pid) {
                #[cfg(not(windows))]
                if view.ppid != self.parent_pid {
                    continue;
                }
                self.observed.entry(pid).or_insert(view.start);
            }
        }
    }

    fn parent_exited(&mut self) -> std::io::Result<bool> {
        Ok(self
            .child
            .as_mut()
            .expect("parent handle")
            .try_wait()?
            .is_some())
    }

    fn bind_worker(&mut self, pid: u32) -> Result<(), String> {
        if pid <= 1 {
            return Err("refusing a privileged pid".to_string());
        }
        let Some(seen) = self.observed.get(&pid).copied() else {
            return Err(
                "pid was not in the parent's child list before the pause file was accepted"
                    .to_string(),
            );
        };
        let Some(view) = process_view(pid) else {
            return Err("pid disappeared before its identity was checked".to_string());
        };
        if view.start != seen {
            return Err("pid was reused before it could be bound".to_string());
        }
        #[cfg(not(windows))]
        if view.ppid != self.parent_pid {
            return Err(format!(
                "ppid {} is not the mcp parent {}",
                view.ppid, self.parent_pid
            ));
        }
        self.worker = Some(OwnedWorker {
            pid,
            start: seen,
            own_group: view.pgrp == pid && pid > 1,
        });
        Ok(())
    }

    fn finish(mut self) -> PauseCleanupProof {
        let worker = self.worker;
        let proof = self.reap(worker);
        self.finished = true;
        proof
    }

    fn reap(&mut self, worker: Option<OwnedWorker>) -> PauseCleanupProof {
        let own_group = worker.as_ref().is_some_and(|worker| worker.own_group);
        let mut targets: Vec<(u32, u64)> = self
            .observed
            .iter()
            .map(|(pid, start)| (*pid, *start))
            .filter(|(pid, _)| *pid != self.parent_pid && *pid > 1)
            .collect();
        if let Some(worker) = &worker {
            targets.retain(|(pid, _)| *pid != worker.pid);
            targets.insert(0, (worker.pid, worker.start));
        }
        for (pid, start) in &targets {
            signal_verified(*pid, *start);
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        if let Some(worker) = &worker {
            while same_live_identity(worker.pid, worker.start) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
        }
        if let Some(child) = self.child.as_mut()
            && process_view(self.parent_pid)
                .is_some_and(|view| view.start == self.parent_start && !view.state_zombie)
        {
            let _ = child.kill();
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
        let after = worker.as_ref().map(|worker| {
            let view = process_view(worker.pid);
            match view {
                None => (true, false, true),
                Some(view) if view.start != worker.start => (true, false, true),
                Some(view) if view.state_zombie => (false, true, true),
                Some(_) => (false, false, false),
            }
        });
        let (worker_reaped, worker_zombie, worker_not_live) =
            after.unwrap_or((false, false, false));
        PauseCleanupProof {
            own_group,
            worker_not_live,
            worker_reaped,
            worker_zombie,
            checked_worker_after_parent_wait: worker.is_some(),
        }
    }
}

impl Drop for OwnedPauseCleanup {
    fn drop(&mut self) {
        if !self.finished {
            let worker = self.worker;
            let _ = self.reap(worker);
        }
    }
}

fn same_live_identity(pid: u32, start: u64) -> bool {
    process_view(pid).is_some_and(|view| view.start == start && !view.state_zombie)
}

fn signal_verified(pid: u32, start: u64) {
    let Some(view) = process_view(pid) else {
        return;
    };
    if pid <= 1 || view.start != start || view.state_zombie {
        return;
    }
    #[cfg(unix)]
    {
        // SAFETY: the pid was observed as a child of this test's mcp parent and
        // still has that start token. A private group is signaled only when the
        // pid is that group's leader. Otherwise only this pid is signaled.
        unsafe {
            if view.pgrp == pid && view.pgrp > 1 {
                libc::kill(-(pid as libc::pid_t), libc::SIGTERM);
            } else {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{
                GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
                TerminateProcess,
            },
        };
        // SAFETY: query-and-terminate handle for the verified pid; closed on every path.
        unsafe {
            let handle = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                0,
                pid,
            );
            if handle.is_null() {
                return;
            }
            let mut creation = windows_sys::Win32::Foundation::FILETIME {
                dwLowDateTime: 0,
                dwHighDateTime: 0,
            };
            let mut exit_time = creation;
            let mut kernel = creation;
            let mut user = creation;
            let ok = GetProcessTimes(
                handle,
                &mut creation,
                &mut exit_time,
                &mut kernel,
                &mut user,
            );
            let current =
                ((creation.dwHighDateTime as u64) << 32) | (creation.dwLowDateTime as u64);
            if ok != 0 && current == start {
                let _ = TerminateProcess(handle, 1);
            }
            let _ = CloseHandle(handle);
        }
    }
}

#[cfg(target_os = "linux")]
fn child_pids(parent: u32) -> Vec<u32> {
    let mut found = Vec::new();
    let Ok(tasks) = fs::read_dir(format!("/proc/{parent}/task")) else {
        return found;
    };
    for task in tasks.flatten() {
        let Ok(text) = fs::read_to_string(task.path().join("children")) else {
            continue;
        };
        for token in text.split_whitespace() {
            if let Ok(pid) = token.parse::<u32>() {
                found.push(pid);
            }
        }
    }
    found
}

#[cfg(target_os = "macos")]
fn child_pids(parent: u32) -> Vec<u32> {
    let mut buffer = vec![0i32; 8192];
    let bytes = unsafe {
        libc::proc_listallpids(
            buffer.as_mut_ptr().cast(),
            (buffer.len() * std::mem::size_of::<i32>()) as i32,
        )
    };
    if bytes <= 0 {
        return Vec::new();
    }
    let count = (bytes as usize) / std::mem::size_of::<i32>();
    buffer
        .into_iter()
        .take(count)
        .filter_map(|pid| u32::try_from(pid).ok())
        .filter(|pid| process_view(*pid).is_some_and(|view| view.ppid == parent))
        .collect()
}

#[cfg(windows)]
fn child_pids(parent: u32) -> Vec<u32> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32, Process32First, Process32Next,
            TH32CS_SNAPPROCESS,
        },
    };
    let mut found = Vec::new();
    // SAFETY: process snapshot is closed before return. Entries are only read
    // after a successful Process32First/Next call.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot.is_null() {
            return found;
        }
        let mut entry: PROCESSENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32>() as u32;
        if Process32First(snapshot, &mut entry) != 0 {
            loop {
                if entry.th32ParentProcessID == parent {
                    found.push(entry.th32ProcessID);
                }
                if Process32Next(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    found
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn child_pids(_parent: u32) -> Vec<u32> {
    Vec::new()
}

#[cfg(target_os = "linux")]
fn process_view(pid: u32) -> Option<ProcessView> {
    if pid <= 1 {
        return None;
    }
    let raw = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = raw.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?;
    let ppid = fields.next()?.parse().ok()?;
    let pgrp = fields.next()?.parse().ok()?;
    let start = fields.nth(16)?.parse().ok()?;
    Some(ProcessView {
        start,
        state_zombie: state.starts_with('Z'),
        pgrp,
        ppid,
    })
}

#[cfg(target_os = "macos")]
fn process_view(pid: u32) -> Option<ProcessView> {
    if pid <= 1 {
        return None;
    }
    let pid_i = i32::try_from(pid).ok()?;
    let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    // SAFETY: the kernel writes a full `proc_bsdinfo` into this sized buffer.
    // `assume_init` follows only an exact-size read.
    let read = unsafe {
        libc::proc_pidinfo(
            pid_i,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    let info = unsafe { info.assume_init() };
    if info.pbi_pid != pid || info.pbi_start_tvusec >= 1_000_000 {
        return None;
    }
    let start = info
        .pbi_start_tvsec
        .checked_mul(1_000_000)?
        .checked_add(info.pbi_start_tvusec)?;
    Some(ProcessView {
        start,
        state_zombie: info.pbi_status == libc::SZOMB,
        pgrp: info.pbi_pgid,
        ppid: info.pbi_ppid,
    })
}

#[cfg(windows)]
fn process_view(pid: u32) -> Option<ProcessView> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FILETIME, STILL_ACTIVE},
        System::Threading::{
            GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    };
    if pid <= 1 {
        return None;
    }
    // SAFETY: query-only handle, closed on every path.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut creation = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut exit_time = creation;
        let mut kernel = creation;
        let mut user = creation;
        let timed = GetProcessTimes(
            handle,
            &mut creation,
            &mut exit_time,
            &mut kernel,
            &mut user,
        );
        let mut exit_code = 0u32;
        let queried = GetExitCodeProcess(handle, &mut exit_code);
        let _ = CloseHandle(handle);
        if timed == 0 || queried == 0 {
            return None;
        }
        Some(ProcessView {
            start: ((creation.dwHighDateTime as u64) << 32) | (creation.dwLowDateTime as u64),
            state_zombie: exit_code != STILL_ACTIVE as u32,
            pgrp: 0,
            ppid: 0,
        })
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn process_view(_pid: u32) -> Option<ProcessView> {
    None
}

#[test]
fn read_worker_status_tree_gone_after_timeout() {
    let repo = init_repo(None);
    let pause_file = repo.path().join("pause.pid");
    let owned = vec![
        ("LIBRA_MCP_TEST_STATUS_PAUSE", "1".to_string()),
        ("LIBRA_MCP_TEST_READ_DEADLINE_MS", "2000".to_string()),
        (
            "LIBRA_MCP_TEST_STATUS_PAUSE_FILE",
            pause_file.display().to_string(),
        ),
    ];
    let call = tool_call(2, "libra_test_status", "{}");
    let output = run_mcp_owned(repo.path(), &session(&call), &owned);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let message = response_with_id(&stdout, 2);
    assert_eq!(
        message["result"]["structuredContent"]["stable_code"], "LBR-AGENT-036",
        "{message}"
    );
    let worker: u32 = fs::read_to_string(&pause_file)
        .expect("pause pid")
        .trim()
        .parse()
        .expect("pid");
    let started = Instant::now();
    while (pid_exists(worker) || !descendant_pids(worker).is_empty())
        && started.elapsed() < Duration::from_secs(1)
    {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!pid_exists(worker), "worker {worker} still exists");
    assert!(descendant_pids(worker).is_empty());
}
