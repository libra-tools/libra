//! Read-tool subprocess executor (plan-20261019 MC-29).
//!
//! Each read tool runs in `libra agent mcp --read-worker`. The parent replies
//! with `LBR-AGENT-036` at the read deadline and kills the worker's process
//! group. The concurrency slot is released when that exit is observed, not
//! when the stdout reader finishes.

use std::{
    io::{Read, Write},
    path::Path,
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use super::{output, protocol, tools};
use crate::utils::error::{CliError, CliResult};

const READ_SLOTS: usize = 4;
const REAP_BUDGET: Duration = Duration::from_secs(1);

struct SlotGate {
    free: Mutex<usize>,
    cv: Condvar,
}

impl SlotGate {
    fn acquire(&self) -> SlotGuard<'_> {
        let mut free = self
            .free
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while *free == 0 {
            free = self
                .cv
                .wait(free)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *free -= 1;
        SlotGuard { gate: self }
    }

    fn release(&self) {
        let mut free = self
            .free
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *free += 1;
        self.cv.notify_one();
    }
}

struct SlotGuard<'a> {
    gate: &'a SlotGate,
}

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        self.gate.release();
    }
}

fn running_as_libra_binary() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().starts_with("libra"))
        })
        .unwrap_or(false)
}

fn invoke_locally(name: &str, arguments: &Value) -> Result<Value, String> {
    match tools::invoke_in_worker(name, arguments) {
        Ok(structured) => Ok(protocol::tool_success_result(&structured)),
        Err(message) => Ok(protocol::tool_error_result(
            "LBR-AGENT-027",
            &message,
            false,
        )),
    }
}

fn slots() -> &'static SlotGate {
    static SLOTS: std::sync::OnceLock<SlotGate> = std::sync::OnceLock::new();
    SLOTS.get_or_init(|| SlotGate {
        free: Mutex::new(READ_SLOTS),
        cv: Condvar::new(),
    })
}

pub fn is_read_worker_argv(argv: &[std::ffi::OsString]) -> bool {
    let mut tokens = argv.iter().skip(1).filter_map(|arg| arg.to_str());
    tokens.next() == Some("agent")
        && tokens.next() == Some("mcp")
        && tokens.any(|token| token == "--read-worker")
}

pub fn read_deadline() -> Duration {
    #[cfg(debug_assertions)]
    if std::env::var("LIBRA_TEST").ok().as_deref() == Some("1")
        && let Ok(raw) = std::env::var("LIBRA_MCP_TEST_READ_DEADLINE_MS")
        && let Ok(ms) = raw.parse::<u64>()
        && ms > 0
    {
        return Duration::from_millis(ms);
    }
    output::READ_DEADLINE
}

/// Run one read tool in a worker process. The returned value is a tool result.
pub fn execute_read_tool(name: &str, arguments: &Value) -> Result<Value, String> {
    let is_libra = running_as_libra_binary();
    // Library callers inside a test binary are not the `libra` executable.
    // The stdio server is, and that is the process which spawns workers.
    if !is_libra {
        return invoke_locally(name, arguments);
    }
    let slot = slots().acquire();
    let exe =
        std::env::current_exe().map_err(|_| "read worker binary is not available".to_string())?;
    let mut command = Command::new(exe);
    command
        .arg("agent")
        .arg("mcp")
        .arg("--read-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(any(target_os = "linux", windows))]
    let memory_limit = worker_memory_limit();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        #[cfg(target_os = "linux")]
        {
            // `RLIMIT_AS` counts every mapping, including the image and each
            // thread stack. The resident budget alone aborts the worker before
            // `main`, and a ceiling the runtime can fill returns `EAGAIN` from
            // the next thread. The ceiling leaves mapping room above the
            // budget. The resident sampler enforces `memory_limit`.
            let bytes = address_space_ceiling(memory_limit);
            // SAFETY: `pre_exec` runs in the forked child before `exec`.
            // `setrlimit` only changes that child's address-space ceiling.
            unsafe {
                command.pre_exec(move || {
                    let limit = libc::rlimit {
                        rlim_cur: bytes,
                        rlim_max: bytes,
                    };
                    if libc::setrlimit(libc::RLIMIT_AS, &limit) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
    }
    let mut child = command
        .spawn()
        .map_err(|_| "read worker could not be started".to_string())?;
    #[cfg(windows)]
    if !assign_windows_memory_job(child.id(), memory_limit) {
        let _ = child.kill();
        let _ = child.wait();
        drop(slot);
        return Err("read worker memory limit could not be applied".to_string());
    }
    let request = json!({"name": name, "arguments": arguments});
    let bytes = serde_json::to_vec(&request)
        .map_err(|_| "read worker request could not be encoded".to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&bytes)
            .map_err(|_| "read worker request could not be written".to_string())?;
        stdin
            .write_all(b"\n")
            .map_err(|_| "read worker request could not be written".to_string())?;
    }
    wait_for_worker(child, slot)
}

fn wait_for_worker(
    mut child: std::process::Child,
    slot: SlotGuard<'static>,
) -> Result<Value, String> {
    let deadline = read_deadline();
    let started = Instant::now();
    let watch_stop = Arc::new(AtomicBool::new(false));
    let peak_rss = Arc::new(AtomicU64::new(0));
    let sample_stop = Arc::clone(&watch_stop);
    let sample_peak = Arc::clone(&peak_rss);
    let sample_pid = child.id();
    let sample_limit = worker_memory_limit();
    // The sampler and reaper must not observe the same PID on opposite sides
    // of reaping, or a reused process-group ID could be signaled.
    let process_identity_gate = Arc::new(Mutex::new(()));
    let sample_identity_gate = Arc::clone(&process_identity_gate);
    std::thread::spawn(move || {
        sample_resident_memory(
            sample_pid,
            sample_limit,
            sample_stop,
            sample_peak,
            sample_identity_gate,
        )
    });
    // Drain stdout while the worker runs. A full pipe would stall a large
    // result, and the parent would then mistake that stall for the deadline.
    let stdout = child.stdout.take();
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = stdout {
            let _ = pipe.read_to_end(&mut bytes);
        }
        let _ = sender.send(bytes);
    });
    loop {
        let observation = {
            let _identity = process_identity_gate
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let observation = child.try_wait();
            if !matches!(&observation, Ok(None)) {
                watch_stop.store(true, Ordering::Relaxed);
            }
            observation
        };
        match observation {
            Ok(Some(status)) => {
                // The worker is gone, so the slot is free before any join.
                // The reply only waits on the bounded channel, never on join.
                drop(slot);
                finish_memory_watch(&watch_stop, &peak_rss);
                let outcome = match receiver.recv_timeout(REAP_BUDGET) {
                    Ok(bytes) if status.success() => parse_worker_result(&bytes),
                    Ok(_) if exited_for_memory_limit(&status) => Ok(memory_limit_result()),
                    Ok(_) => Ok(protocol::tool_error_result(
                        "LBR-AGENT-037",
                        "read worker failed before producing a result",
                        false,
                    )),
                    Err(_) if exited_for_memory_limit(&status) => Ok(memory_limit_result()),
                    Err(_) => {
                        let message = if status.success() {
                            "read worker output did not close"
                        } else {
                            "read worker failed before producing a result"
                        };
                        Ok(protocol::tool_error_result("LBR-AGENT-037", message, false))
                    }
                };
                std::thread::spawn(move || {
                    let _ = reader.join();
                });
                return outcome;
            }
            Ok(None) if started.elapsed() >= deadline => {
                let identity_guard = process_identity_gate
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                finish_memory_watch(&watch_stop, &peak_rss);
                drop(identity_guard);
                detach_after_deadline(child, slot, reader);
                return Ok(protocol::tool_error_result(
                    "LBR-AGENT-036",
                    "read tool exceeded the 30 s deadline",
                    true,
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                finish_memory_watch(&watch_stop, &peak_rss);
                detach_after_deadline(child, slot, reader);
                return Ok(protocol::tool_error_result(
                    "LBR-AGENT-037",
                    "read worker could not be observed",
                    false,
                ));
            }
        }
    }
}

fn detach_after_deadline(
    mut child: std::process::Child,
    slot: SlotGuard<'static>,
    reader: std::thread::JoinHandle<()>,
) {
    signal_worker(&mut child);
    let started = Instant::now();
    while started.elapsed() < REAP_BUDGET {
        match child.try_wait() {
            Ok(Some(_)) => {
                // Confirmed exit. Do not join the stdout reader on this path:
                // a descendant outside the killed process group can keep the
                // pipe open and block `read_to_end` past the reply bound.
                drop(slot);
                std::thread::spawn(move || {
                    let _ = reader.join();
                });
                return;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            // `try_wait` failed, so the exit is unconfirmed. The reaper keeps
            // the slot until `wait` observes it.
            Err(_) => break,
        }
    }
    // The reply must not wait on a worker stuck in uninterruptible I/O.
    // The slot stays taken only until `wait` observes the exit. The stdout
    // reader is joined after that, so a descendant holding the pipe cannot
    // keep the slot.
    std::thread::spawn(move || {
        let _ = child.wait();
        drop(slot);
        std::thread::spawn(move || {
            let _ = reader.join();
        });
    });
}

fn signal_worker(child: &mut std::process::Child) {
    #[cfg(unix)]
    // SAFETY: `child.id()` is the worker started with `process_group(0)`, so
    // its process-group id is its pid. A negative pid asks `kill` to signal
    // that group. The child is still unreaped here.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
}

/// Address-space ceiling passed to `setrlimit(RLIMIT_AS)`.
///
/// The configured budget is resident memory. The runtime maps much more
/// address space than it resides (the executable image and one stack per
/// thread). Two gigabytes of headroom lets that mapping exist; the resident
/// sampler still stops the process group when RSS exceeds the budget.
#[cfg(target_os = "linux")]
fn address_space_ceiling(resident_limit: u64) -> u64 {
    const MAPPING_HEADROOM: u64 = 2 * 1024 * 1024 * 1024;
    resident_limit.saturating_add(MAPPING_HEADROOM)
}

fn worker_memory_limit() -> u64 {
    const DEFAULT_BYTES: u64 = 512 * 1024 * 1024;
    #[cfg(debug_assertions)]
    if std::env::var("LIBRA_TEST").ok().as_deref() == Some("1")
        && let Ok(raw) = std::env::var("LIBRA_MCP_TEST_WORKER_MEM_MB")
        && let Ok(mb) = raw.parse::<u64>()
        && mb > 0
    {
        return mb.saturating_mul(1024 * 1024);
    }
    DEFAULT_BYTES
}

fn memory_limit_result() -> Value {
    protocol::tool_error_result_reason(
        "LBR-AGENT-028",
        "read worker memory limit exceeded",
        false,
        Some("worker_memory_limit"),
    )
}

fn finish_memory_watch(stop: &AtomicBool, peak: &AtomicU64) {
    stop.store(true, Ordering::Relaxed);
    let rss = peak.load(Ordering::Relaxed);
    #[cfg(debug_assertions)]
    if std::env::var("LIBRA_TEST").ok().as_deref() == Some("1")
        && let Ok(path) = std::env::var("LIBRA_MCP_TEST_PEAK_RSS_FILE")
    {
        let _ = std::fs::write(path, rss.to_string());
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = rss;
    }
}

fn exited_for_memory_limit(status: &ExitStatus) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        matches!(
            status.signal(),
            Some(libc::SIGKILL | libc::SIGSEGV | libc::SIGABRT | libc::SIGBUS)
        )
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        false
    }
}

fn sample_resident_memory(
    pid: u32,
    limit: u64,
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    process_identity_gate: Arc<Mutex<()>>,
) {
    loop {
        let identity_guard = process_identity_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if let Some(rss) = resident_bytes(pid) {
            peak.fetch_max(rss, Ordering::Relaxed);
            // The resident budget is enforced here. Linux `RLIMIT_AS` is higher
            // than this budget so the mapped executable can start; macOS has
            // no equivalent hard address-space cap.
            #[cfg(unix)]
            if rss > limit {
                // SAFETY: the reaper holds the same identity gate, so this
                // child PID cannot be reaped and reused before the signal.
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
                break;
            }
        }
        drop(identity_guard);
        let _ = limit;
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn resident_bytes(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        for line in text.lines() {
            let Some(rest) = line.strip_prefix("VmRSS:") else {
                continue;
            };
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb.saturating_mul(1024));
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        macos_resident_bytes(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "macos")]
fn macos_resident_bytes(pid: u32) -> Option<u64> {
    #[repr(C)]
    struct RusageInfoV0 {
        ri_uuid: [u8; 16],
        ri_user_time: u64,
        ri_system_time: u64,
        ri_pkg_idle_wkups: u64,
        ri_interrupt_wkups: u64,
        ri_pageins: u64,
        ri_wired_size: u64,
        ri_resident_size: u64,
        ri_phys_footprint: u64,
        ri_proc_start_abstime: u64,
        ri_proc_exit_abstime: u64,
        ri_child_user_time: u64,
        ri_child_system_time: u64,
        ri_child_pkg_idle_wkups: u64,
        ri_child_interrupt_wkups: u64,
        ri_child_pageins: u64,
        ri_child_elapsed_abstime: u64,
        ri_diskio_bytesread: u64,
        ri_diskio_byteswritten: u64,
    }
    // Rust 2024 requires `unsafe` on `extern` blocks. The call below stays
    // in its own `unsafe` block.
    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut RusageInfoV0) -> i32;
    }
    let mut info = std::mem::MaybeUninit::<RusageInfoV0>::zeroed();
    // SAFETY: `info` is a zeroed V0 buffer and `pid` is the worker we spawned.
    let rc = unsafe { proc_pid_rusage(pid as i32, 0, info.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    Some(unsafe { info.assume_init() }.ri_resident_size)
}

#[cfg(windows)]
fn assign_windows_memory_job(pid: u32, bytes: u64) -> bool {
    use std::mem::size_of;

    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            },
            Threading::{
                OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA,
                PROCESS_TERMINATE,
            },
        },
    };
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return false;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_PROCESS_MEMORY;
        info.ProcessMemoryLimit = bytes as usize;
        let set = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        let process: HANDLE = OpenProcess(
            PROCESS_SET_QUOTA | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        );
        let assigned =
            !process.is_null() && AssignProcessToJobObject(job, process) != 0 && set != 0;
        if !process.is_null() {
            CloseHandle(process);
        }
        CloseHandle(job);
        assigned
    }
}

fn parse_worker_result(stdout: &[u8]) -> Result<Value, String> {
    let limited = if stdout.len() > 2 * 1024 * 1024 {
        &stdout[..2 * 1024 * 1024]
    } else {
        stdout
    };
    let text = String::from_utf8(limited.to_vec())
        .map_err(|_| "read worker output was not UTF-8".to_string())?;
    let parsed: Value = serde_json::from_str(text.trim())
        .map_err(|_| "read worker output was not JSON".to_string())?;
    if parsed.get("ok").and_then(Value::as_bool) == Some(true) {
        let structured = parsed.get("structured").cloned().unwrap_or(Value::Null);
        return Ok(protocol::tool_success_result(&structured));
    }
    let code = parsed
        .get("stable_code")
        .and_then(Value::as_str)
        .unwrap_or("LBR-AGENT-037");
    let message = parsed
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("read worker failed");
    let retryable = parsed
        .get("retryable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(protocol::tool_error_result(code, message, retryable))
}

pub fn line_starts_read_tool(line: &protocol::InboundLine) -> bool {
    let protocol::InboundLine::Message(bytes) = line else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let Ok(parsed) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    if parsed.get("method").and_then(Value::as_str) != Some("tools/call") {
        return false;
    }
    let Some(name) = parsed
        .get("params")
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
    else {
        return false;
    };
    tools::registered_tools()
        .iter()
        .any(|tool| tool.name == name && tool.read_only)
}

/// Hidden worker: one server-built request in, one JSON result out.
pub async fn run_read_worker() -> CliResult<()> {
    crate::internal::worktree_io::executor::set_in_process_only_for_process();
    let (name, arguments) = match read_worker_request().await {
        Ok(request) => request,
        Err(message) => {
            write_worker_error("LBR-AGENT-037", &message, false)?;
            return Ok(());
        }
    };
    // Alloc and status-pause fixtures only need the in-process executor. Opening
    // the repository database here races the parent server's pool and returns
    // EAGAIN. Schema checks stay on every other read tool.
    let skip_database = cfg!(debug_assertions)
        && std::env::var("LIBRA_TEST").ok().as_deref() == Some("1")
        && matches!(
            name.as_str(),
            "libra_test_alloc" | "libra_test_sleep" | "libra_test_status"
        );
    if !skip_database {
        let storage = crate::utils::util::try_get_storage_path(None)
            .map_err(|_| CliError::fatal("read worker could not find a repository".to_string()))?;
        if let Err(message) = prepare_read_only_repository(&storage).await {
            write_worker_error("LBR-AGENT-037", &message, false)?;
            return Ok(());
        }
    }
    match tools::invoke_in_worker(&name, &arguments) {
        Ok(structured) => write_worker_success(&structured),
        Err(message) => write_worker_error("LBR-AGENT-027", &message, false),
    }
}

async fn read_worker_request() -> Result<(String, Value), String> {
    let mut line = String::new();
    let read = std::io::stdin()
        .read_line(&mut line)
        .map_err(|error| format!("read worker could not read its request: {error}"))?;
    if read == 0 {
        return Err("read worker received an empty request".to_string());
    }
    let request: Value =
        serde_json::from_str(&line).map_err(|_| "read worker request was not JSON".to_string())?;
    let name = request
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let arguments = request
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    Ok((name, arguments))
}

/// Open the repository database with bounded retries for transient failures.
///
/// SQLite lock errors and SQLx native-thread creation failures are distinct:
/// `Io(EAGAIN)` can mean there is no address space left for a SQLite thread.
/// The private CLI runtime is bounded to leave room for that thread. This
/// existing retry loop neither identifies the cause nor relaxes worker limits.
async fn open_read_worker_database(
    db_path_text: &str,
    db_path: &Path,
) -> Result<sea_orm::DatabaseConnection, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match crate::internal::db::open_connection_without_schema_management(
            db_path_text,
            Duration::from_millis(200),
        )
        .await
        {
            Ok(conn) => return Ok(conn),
            Err(error) => {
                let message = error.to_string();
                let locked = message.contains("WouldBlock")
                    || message.contains("database is locked")
                    || message.contains("Resource temporarily unavailable");
                if locked && Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    continue;
                }
                return Err(format!(
                    "read worker could not open the repository database at {}: {message}",
                    db_path.display()
                ));
            }
        }
    }
}

async fn prepare_read_only_repository(storage: &Path) -> Result<(), String> {
    let db_path = storage.join(crate::utils::util::DATABASE);
    let db_path_text = db_path.to_str().ok_or_else(|| {
        format!(
            "read worker could not open the repository database at {}: path is not UTF-8",
            db_path.display()
        )
    })?;
    let conn = open_read_worker_database(db_path_text, &db_path).await?;
    let current = crate::internal::db::migration::current_builtin_schema_version_readonly(&conn)
        .await
        .map_err(|_| "read worker could not read the repository schema version".to_string())?;
    let latest = crate::internal::db::migration::latest_builtin_schema_version()
        .map_err(|_| "read worker could not determine its schema version".to_string())?;
    if current.is_none() || current != latest {
        return Err("repository schema is not the version this binary reads".to_string());
    }
    let object_format = read_object_format(&conn).await?;
    let hash_kind = crate::internal::object_format::parse_config_value(&object_format)
        .map_err(|_| format!("unsupported object format: '{object_format}'"))?;
    git_internal::hash::set_hash_kind(hash_kind);
    Ok(())
}

async fn read_object_format(conn: &sea_orm::DatabaseConnection) -> Result<String, String> {
    use sea_orm::{ConnectionTrait, Statement};
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT value FROM config_kv WHERE key = ? ORDER BY id DESC LIMIT 1",
            ["core.objectformat".into()],
        ))
        .await
        .map_err(|_| "read worker could not read core.objectformat".to_string())?;
    match row {
        Some(row) => row
            .try_get_by_index::<String>(0)
            .map_err(|_| "read worker could not decode core.objectformat".to_string()),
        None => Ok("sha1".to_string()),
    }
}

fn write_worker_success(structured: &Value) -> CliResult<()> {
    let line =
        serde_json::to_string(&json!({"ok": true, "structured": structured})).map_err(|error| {
            CliError::fatal(format!("read worker could not encode its result: {error}"))
        })?;
    println!("{line}");
    Ok(())
}

fn write_worker_error(code: &str, message: &str, retryable: bool) -> CliResult<()> {
    let line = serde_json::to_string(&json!({
        "ok": false,
        "stable_code": code,
        "message": message,
        "retryable": retryable
    }))
    .map_err(|error| CliError::fatal(format!("read worker could not encode its error: {error}")))?;
    println!("{line}");
    Ok(())
}
