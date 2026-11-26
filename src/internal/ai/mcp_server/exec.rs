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

/// Verify repository schema version and configure the active Git hash algorithm
/// from a backend-agnostic [`crate::storage::ConfigStore`] without mutating state.
pub async fn prepare_read_only_repository_with_store(
    store: &dyn crate::storage::ConfigStore,
) -> Result<git_internal::hash::HashKind, String> {
    let receipts = store
        .list_schema_receipts()
        .await
        .map_err(|_| "read worker could not read the repository schema version".to_string())?;
    let current = receipts.iter().map(|receipt| receipt.version).max();
    let latest = crate::internal::db::migration::latest_builtin_schema_version()
        .map_err(|_| "read worker could not determine its schema version".to_string())?;
    if current.is_none() || current != latest {
        return Err("repository schema is not the version this binary reads".to_string());
    }
    let object_format = store
        .get("core.objectformat")
        .await
        .map_err(|_| "read worker could not read core.objectformat".to_string())?
        .map(|entry| entry.value)
        .unwrap_or_else(|| "sha1".to_string());
    let hash_kind = crate::internal::object_format::parse_config_value(&object_format)
        .map_err(|_| format!("unsupported object format: '{object_format}'"))?;
    git_internal::hash::set_hash_kind(hash_kind);
    Ok(hash_kind)
}

pub(crate) async fn prepare_read_only_repository(storage: &Path) -> Result<(), String> {
    match crate::storage::runtime::detect_repo_backend(storage).map_err(|error| {
        format!("read worker could not detect repository storage backend: {error}")
    })? {
        crate::storage::StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                let store = crate::storage::runtime::open_repo_config_store_read_only(storage)
                    .await
                    .map_err(|error| {
                        format!("read worker could not open the redb repository store: {error}")
                    })?;
                prepare_read_only_repository_with_store(store.as_ref()).await?;
                Ok(())
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                Err("this Libra binary was compiled without `storage-redb`".to_string())
            }
        }
        crate::storage::StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let db_path = storage.join(crate::utils::util::DATABASE);
                let db_path_text = db_path.to_str().ok_or_else(|| {
                    format!(
                        "read worker could not open the repository database at {}: path is not UTF-8",
                        db_path.display()
                    )
                })?;
                let deadline = Instant::now() + Duration::from_secs(5);
                let conn = loop {
                    match crate::internal::db::open_connection_without_schema_management(
                        db_path_text,
                        Duration::from_millis(200),
                    )
                    .await
                    {
                        Ok(conn) => break conn,
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
                };
                let current =
                    crate::internal::db::migration::current_builtin_schema_version_readonly(&conn)
                        .await
                        .map_err(|_| {
                            "read worker could not read the repository schema version".to_string()
                        })?;
                let latest = crate::internal::db::migration::latest_builtin_schema_version()
                    .map_err(|_| {
                        "read worker could not determine its schema version".to_string()
                    })?;
                if current.is_none() || current != latest {
                    return Err(
                        "repository schema is not the version this binary reads".to_string()
                    );
                }
                let object_format = read_object_format(&conn).await?;
                let hash_kind = crate::internal::object_format::parse_config_value(&object_format)
                    .map_err(|_| format!("unsupported object format: '{object_format}'"))?;
                git_internal::hash::set_hash_kind(hash_kind);
                Ok(())
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                Err("this Libra binary was compiled without `storage-sqlite`".to_string())
            }
        }
    }
}

#[cfg(feature = "storage-sqlite")]
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

#[cfg(test)]
mod tests {
    #[cfg(feature = "storage-redb")]
    use super::*;

    #[cfg(feature = "storage-redb")]
    struct CwdGuard(std::path::PathBuf);

    #[cfg(feature = "storage-redb")]
    impl CwdGuard {
        fn enter(path: &Path) -> Self {
            let prev = std::env::current_dir().expect("capture current_dir");
            std::env::set_current_dir(path).expect("enter test repo cwd");
            Self(prev)
        }
    }

    #[cfg(feature = "storage-redb")]
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    #[cfg(feature = "storage-redb")]
    struct HashKindGuard(git_internal::hash::HashKind);

    #[cfg(feature = "storage-redb")]
    impl HashKindGuard {
        fn capture() -> Self {
            Self(git_internal::hash::get_hash_kind())
        }
    }

    #[cfg(feature = "storage-redb")]
    impl Drop for HashKindGuard {
        fn drop(&mut self) {
            git_internal::hash::set_hash_kind(self.0);
        }
    }

    #[cfg(feature = "storage-redb")]
    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn redb_hooks_mcp_observed_and_native_origin_stores() {
        use std::{fs, time::Instant};

        use base64::{Engine as _, engine::general_purpose::STANDARD};
        use git_internal::hash::{HashKind, get_hash_kind};
        use ring::signature::{Ed25519KeyPair, KeyPair};
        use sha2::{Digest, Sha256};

        use crate::{
            internal::{
                ai::{
                    history,
                    hooks::load_hook_config,
                    mcp_server::stdio::{
                        repository_id_for_storage, repository_id_with_store, serve,
                    },
                    observed_agents::{
                        compliance::{
                            AuditRecord, AuditScope, MAX_TRANSCRIPT_READ_BYTES_KEY,
                            RETENTION_FINDINGS_DAYS_KEY, RETENTION_STDERR_DAYS_KEY,
                            RETENTION_TRANSCRIPT_DAYS_KEY, max_transcript_read_bytes,
                            max_transcript_read_bytes_setting_with_store,
                            retention_findings_days_with_store, retention_stderr_days_with_store,
                            retention_transcript_days, retention_transcript_days_with_store,
                            write_audit_record_with_store,
                        },
                        hermes_export::{
                            HermesBridgeStatus, HermesExporterTrust, HermesLiveError,
                            HermesLiveRequest, HermesLiveSource,
                            acquire_hermes_export_lease_with_store, export_for_live_capture,
                            read_hermes_live_source_with_stores,
                            release_hermes_export_lease_with_store,
                            resolve_capture_scope_with_stores,
                        },
                        native_origin::{
                            approved_build, codec,
                            registry::{
                                self, IssuerRecord, IssuerState, NativeOriginError, REGISTRY_KEY,
                                Registry,
                            },
                            verify::{self, OriginalField, SourceCoordinates, VerificationError},
                        },
                        trust::{
                            self, HERMES_EXPORTER_SLUG, OPENCODE_EXPORTER_SLUG, TRUSTED_DIRS_KEY,
                            TrustRecord,
                        },
                    },
                },
                config::ConfigKv,
            },
            storage::{
                RedbRepairContext, RefCasOutcome, RefKey, RefTarget, StorageBackendKind,
                runtime::{
                    detect_repo_backend, init_redb_repository_storage,
                    open_capture_store_read_write, open_git_state_store_read_only,
                    open_git_store_read_write, open_repo_config_store_read_only,
                    open_repo_config_store_read_write,
                },
            },
        };

        let _hash_guard = HashKindGuard::capture();
        let temp = tempfile::tempdir().expect("create temp dir");
        let repo_root = temp.path().join("repo");
        let storage = repo_root.join(".libra");
        fs::create_dir_all(storage.join("objects")).expect("create .libra/objects");

        let _gen = init_redb_repository_storage(&storage)
            .await
            .expect("init pure redb repo");
        assert_eq!(
            detect_repo_backend(&storage).expect("detect backend"),
            StorageBackendKind::Redb
        );

        let repo_id = "rb24-redb-repo-id-0001";
        {
            let cfg_rw = open_repo_config_store_read_write(&storage)
                .await
                .expect("open redb config rw");
            cfg_rw
                .set("libra.repoid", repo_id, false)
                .await
                .expect("seed libra.repoid");
            cfg_rw
                .set("core.objectformat", "sha256", false)
                .await
                .expect("seed core.objectformat=sha256");
        }

        let _cwd_guard = CwdGuard::enter(&repo_root);

        // -----------------------------------------------------------------
        // AC1: hooks/intent configuration & AI history store on pure Redb
        // -----------------------------------------------------------------
        let _hook_cfg = load_hook_config(&repo_root).expect("load hook config on redb repo");
        {
            let git_store = open_git_store_read_write(&storage)
                .await
                .expect("open redb git store rw");
            let ai_ref = RefKey::LocalBranch {
                name: history::ai_ref_name().to_string(),
            };
            let synthetic_commit = "a".repeat(64);
            let cas = git_store
                .update_ref_cas(
                    &ai_ref,
                    None,
                    Some(&RefTarget::Direct(synthetic_commit.clone())),
                    &[],
                )
                .await
                .expect("cas create ai history ref");
            assert!(matches!(cas, RefCasOutcome::Applied { .. }));
            assert_eq!(
                git_store.get_ref(&ai_ref).await.expect("read ai ref"),
                Some(RefTarget::Direct(synthetic_commit))
            );
        }

        // -----------------------------------------------------------------
        // AC2: mcp_server exec & stdio storage probe and JSON-RPC roundtrip
        // -----------------------------------------------------------------
        prepare_read_only_repository(&storage)
            .await
            .expect("prepare_read_only_repository on redb (sha256)");
        assert_eq!(get_hash_kind(), HashKind::Sha256);

        {
            let cfg_rw = open_repo_config_store_read_write(&storage)
                .await
                .expect("open redb config rw");
            cfg_rw
                .set("core.objectformat", "sha1", false)
                .await
                .expect("switch core.objectformat=sha1");
            let cfg_ro = open_repo_config_store_read_only(&storage)
                .await
                .expect("open redb config ro");
            let kind = prepare_read_only_repository_with_store(cfg_ro.as_ref())
                .await
                .expect("prepare_read_only_repository_with_store on redb (sha1)");
            assert_eq!(kind, HashKind::Sha1);
            assert_eq!(get_hash_kind(), HashKind::Sha1);
            assert_eq!(repository_id_with_store(cfg_ro.as_ref()).await, repo_id);
        }
        assert_eq!(repository_id_for_storage(&storage).await, repo_id);

        // Verify stdio::serve JSON-RPC initialize + tools/list on pure Redb.
        let rpc_input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"rb24","version":"1.0"}}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            "\n"
        );
        let reader = tokio::io::BufReader::new(std::io::Cursor::new(rpc_input.as_bytes().to_vec()));
        let mut rpc_out = Vec::new();
        serve(reader, &mut rpc_out)
            .await
            .expect("mcp stdio serve on redb");
        let rpc_text = String::from_utf8(rpc_out).expect("utf8 rpc output");
        let responses: Vec<Value> = rpc_text
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid jsonrpc line"))
            .collect();
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["id"], json!(1));
        assert_eq!(responses[0]["result"]["serverInfo"]["name"], json!("libra"));
        assert_eq!(responses[1]["id"], json!(2));
        assert!(responses[1]["result"]["tools"].is_array());

        // -----------------------------------------------------------------
        // AC3a: observed_agents::compliance (ConfigStore + CaptureStore)
        // -----------------------------------------------------------------
        let cfg_rw = open_repo_config_store_read_write(&storage)
            .await
            .expect("open redb config rw");
        let state_ro = open_git_state_store_read_only(&storage)
            .await
            .expect("open redb git state ro");
        let capture_rw = open_capture_store_read_write(&storage, RedbRepairContext::WriteCommand)
            .await
            .expect("open redb capture rw");

        cfg_rw
            .set(RETENTION_TRANSCRIPT_DAYS_KEY, "45", false)
            .await
            .expect("set transcript retention");
        cfg_rw
            .set(RETENTION_STDERR_DAYS_KEY, "14", false)
            .await
            .expect("set stderr retention");
        cfg_rw
            .set(RETENTION_FINDINGS_DAYS_KEY, "60", false)
            .await
            .expect("set findings retention");
        cfg_rw
            .set(MAX_TRANSCRIPT_READ_BYTES_KEY, "1048576", false)
            .await
            .expect("set max_transcript_read_bytes");

        assert_eq!(
            retention_transcript_days_with_store(cfg_rw.as_ref())
                .await
                .expect("transcript days with store"),
            45
        );
        assert_eq!(
            retention_transcript_days()
                .await
                .expect("transcript days current repo"),
            45
        );
        assert_eq!(
            retention_stderr_days_with_store(cfg_rw.as_ref())
                .await
                .expect("stderr days with store"),
            14
        );
        assert_eq!(
            retention_findings_days_with_store(cfg_rw.as_ref())
                .await
                .expect("findings days with store"),
            60
        );
        assert_eq!(
            max_transcript_read_bytes_setting_with_store(cfg_rw.as_ref())
                .await
                .expect("max transcript read bytes with store"),
            (1_048_576, true)
        );
        assert_eq!(
            max_transcript_read_bytes()
                .await
                .expect("max transcript read bytes current repo"),
            1_048_576
        );

        let audit = AuditRecord::new(
            "audit-rb24-1".to_string(),
            "2026-11-26T00:00:00Z".to_string(),
            (Some("u1".to_string()), Some("tester".to_string())),
            "raw_export",
            "chk-rb24-1",
            AuditScope::Transcript,
            Some("/tmp/export.jsonl".to_string()),
            Some("rb24 verification".to_string()),
            true,
        );
        write_audit_record_with_store(capture_rw.as_ref(), &audit)
            .await
            .expect("write audit record to redb capture store");
        let fetched_audit = capture_rw
            .get_audit_log("audit-rb24-1")
            .await
            .expect("get audit log")
            .expect("audit row must exist");
        assert_eq!(fetched_audit.checkpoint_id, "chk-rb24-1");
        assert_eq!(fetched_audit.scope, "transcript");
        assert!(fetched_audit.granted);

        // -----------------------------------------------------------------
        // AC3b: observed_agents::trust & observed_agents::hermes_export
        // -----------------------------------------------------------------
        let trusted_bin_dir = temp.path().join("trusted-bin");
        fs::create_dir_all(&trusted_bin_dir).expect("create trusted bin dir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&trusted_bin_dir, fs::Permissions::from_mode(0o700))
                .expect("chmod 0700 trusted dir");
        }
        let hermes_bin = trusted_bin_dir.join("hermes");
        fs::write(&hermes_bin, b"#!/bin/sh\nexit 0\n").expect("write dummy hermes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&hermes_bin, fs::Permissions::from_mode(0o700))
                .expect("chmod 0700 hermes");
        }

        let canonical_dir = trust::add_trusted_dir(&trusted_bin_dir)
            .await
            .expect("add trusted dir on redb");
        let dirs = trust::read_trusted_dirs_with_store(cfg_rw.as_ref())
            .await
            .expect("read trusted dirs with store");
        assert!(dirs.contains(&canonical_dir));

        let hermes_record = trust::record_trust(HERMES_EXPORTER_SLUG, &hermes_bin)
            .await
            .expect("record hermes trust on redb");
        assert_eq!(
            trust::read_trust_with_store(cfg_rw.as_ref(), HERMES_EXPORTER_SLUG)
                .await
                .expect("read hermes trust with store"),
            Some(hermes_record.clone())
        );
        trust::revalidate_trust(HERMES_EXPORTER_SLUG, &hermes_record)
            .await
            .expect("revalidate hermes trust on redb");

        let status_current = HermesBridgeStatus::current()
            .await
            .expect("HermesBridgeStatus::current");
        let status_store = HermesBridgeStatus::from_store(cfg_rw.as_ref())
            .await
            .expect("HermesBridgeStatus::from_store");
        assert_eq!(status_current, status_store);
        assert!(status_store.trusted);
        assert!(status_store.source_qualified);
        assert!(!status_store.native_claimed);

        let loaded_trust = HermesExporterTrust::load_with_store(cfg_rw.as_ref())
            .await
            .expect("load hermes trust with store")
            .expect("hermes trust must be present");
        assert_eq!(loaded_trust.path(), hermes_record.path.as_path());

        let scope =
            resolve_capture_scope_with_stores(cfg_rw.as_ref(), state_ro.as_ref(), &repo_root)
                .await
                .expect("resolve capture scope on redb");
        assert_eq!(scope.repo_id, repo_id);
        assert!(scope.worktree_id.is_empty());

        let deadline = Instant::now() + Duration::from_secs(10);
        let lease1 = acquire_hermes_export_lease_with_store(
            capture_rw.as_ref(),
            &scope,
            "hermes-rb24-sess",
            deadline,
        )
        .await
        .expect("acquire hermes lease")
        .expect("first caller must acquire lease");
        assert_eq!(lease1.session_id(), "hermes-rb24-sess");

        // Second concurrent acquire while lease is live must return None (RecordedOnly).
        let second = acquire_hermes_export_lease_with_store(
            capture_rw.as_ref(),
            &scope,
            "hermes-rb24-sess",
            deadline,
        )
        .await
        .expect("contended acquire");
        assert!(second.is_none());

        // Missing lease fails closed with NoLease.
        let no_lease_req = HermesLiveRequest {
            session_id: "hermes-rb24-sess".to_string(),
            profile: "default".to_string(),
            cwd: repo_root.clone(),
            source: HermesLiveSource::Export,
        };
        let no_lease_err = read_hermes_live_source_with_stores(
            cfg_rw.as_ref(),
            capture_rw.as_ref(),
            &scope,
            None,
            Some(&loaded_trust),
            &no_lease_req,
            deadline,
        )
        .await
        .expect_err("missing lease must fail");
        assert_eq!(no_lease_err, HermesLiveError::NoLease);

        release_hermes_export_lease_with_store(capture_rw.as_ref(), &scope, lease1)
            .await
            .expect("release hermes lease");
        let lease2 = acquire_hermes_export_lease_with_store(
            capture_rw.as_ref(),
            &scope,
            "hermes-rb24-sess",
            deadline,
        )
        .await
        .expect("reacquire after release")
        .expect("released lease must be re-acquirable");
        release_hermes_export_lease_with_store(capture_rw.as_ref(), &scope, lease2)
            .await
            .expect("release reacquired hermes lease");

        // Mutate binary -> revalidate_trust must detect drift and revoke in Redb.
        fs::write(&hermes_bin, b"#!/bin/sh\nexit 1\n").expect("mutate hermes binary");
        assert!(
            trust::revalidate_trust(HERMES_EXPORTER_SLUG, &hermes_record)
                .await
                .is_err()
        );
        assert!(
            trust::read_trust_with_store(cfg_rw.as_ref(), HERMES_EXPORTER_SLUG)
                .await
                .expect("read revoked hermes trust")
                .is_none()
        );
        // Without trust, export_for_live_capture dispatches to Redb and returns Unavailable.
        let unavail = export_for_live_capture("hermes-rb24-sess", &repo_root, deadline)
            .await
            .expect_err("untrusted hermes must be unavailable");
        assert_eq!(unavail, HermesLiveError::Unavailable);

        // -----------------------------------------------------------------
        // AC3c: native_origin::{attest,registry,verify} on pure Redb
        // -----------------------------------------------------------------
        assert!(
            registry::read_with_store(cfg_rw.as_ref())
                .await
                .expect("empty native_origin registry")
                .is_none()
        );

        // register() on Redb dispatches through detect_repo_backend -> register_with_store ->
        // attest::prepare_with_store and returns PriorTrust when OpenCode trust is absent.
        let missing_manifest = temp.path().join("installation.json");
        let prior_err = registry::register(&missing_manifest, 0)
            .await
            .expect_err("register without prior opencode trust must fail");
        assert_eq!(
            prior_err.downcast_ref::<NativeOriginError>(),
            Some(&NativeOriginError::PriorTrust)
        );

        // Seed a valid synthetic approved-build Registry + OpenCode trust into Redb ConfigStore.
        const BINARY_SHA: &str = "547c36eb928da163ef61d019bad669860d519f2da0b67737c9a3920e831c9196";
        const BUILD_ID: &str = "bac21cc4027896b8d1836133c347d20f";
        let seed: Vec<u8> = (0u8..32u8).collect();
        let kp = Ed25519KeyPair::from_seed_unchecked(&seed).expect("test ed25519 key");
        let public_key_hex = hex::encode(kp.public_key().as_ref());
        let issuer_hex = hex::encode(Sha256::digest(kp.public_key().as_ref()));
        let build =
            approved_build::lookup(codec::PRODUCER_VERSION, BUILD_ID, BINARY_SHA).expect("build");
        let cmd_path = "/Applications/Libra.app/Contents/MacOS/libra";
        let rendered = build.render_observer(cmd_path).expect("render observer");
        let policy = build
            .installation_policy(cmd_path, rendered.as_bytes())
            .expect("policy");
        let policy_digest = hex::encode(Sha256::digest(
            codec::policy_bytes(&policy).expect("policy bytes"),
        ));
        let synthetic_producer = "/synthetic/rb24-producer/opencode";
        let issuer_record = IssuerRecord {
            issuer: issuer_hex.clone(),
            public_key: public_key_hex,
            key_generation: 1,
            build_identity: BUILD_ID.to_string(),
            producer_version: codec::PRODUCER_VERSION.to_string(),
            producer_binary_digest: BINARY_SHA.to_string(),
            producer_path: synthetic_producer.to_string(),
            policy_descriptor: policy.clone(),
            policy_digest: policy_digest.clone(),
            state: IssuerState::Active,
        };
        let reg = Registry {
            version: 1,
            epoch: 1,
            issuers: vec![issuer_record.clone()],
        };
        let opencode_trust = TrustRecord {
            path: synthetic_producer.into(),
            sha256: BINARY_SHA.to_string(),
            device: 7,
            inode: 8,
            mtime: 9,
        };
        ConfigKv::set_with_store(
            cfg_rw.as_ref(),
            REGISTRY_KEY,
            &serde_json::to_string(&reg).expect("encode registry"),
            false,
        )
        .await
        .expect("seed native_origin registry");
        ConfigKv::set_with_store(
            cfg_rw.as_ref(),
            &format!("agent.trust.{OPENCODE_EXPORTER_SLUG}"),
            &serde_json::to_string(&opencode_trust).expect("encode opencode trust"),
            false,
        )
        .await
        .expect("seed opencode trust");
        ConfigKv::set_with_store(
            cfg_rw.as_ref(),
            TRUSTED_DIRS_KEY,
            r#"["/synthetic/rb24-producer"]"#,
            false,
        )
        .await
        .expect("seed trusted dirs for synthetic producer");

        let read_reg = registry::read_with_store(cfg_rw.as_ref())
            .await
            .expect("read registry with store")
            .expect("registry must exist");
        assert_eq!(read_reg, reg);

        let prov = trust::Provenance {
            canonical_path: synthetic_producer.into(),
            sha256: BINARY_SHA.to_string(),
            device: 7,
            inode: 8,
            mtime: 9,
        };
        let active = registry::active_issuer_with_store(cfg_rw.as_ref(), &issuer_hex, &prov)
            .await
            .expect("active_issuer_with_store")
            .expect("active issuer must match");
        assert_eq!(active, issuer_record);

        // Epoch mismatch on register() via Redb dispatch returns NativeOriginError::Epoch.
        let epoch_err = registry::register(&missing_manifest, 99)
            .await
            .expect_err("register with stale epoch must fail");
        assert_eq!(
            epoch_err.downcast_ref::<NativeOriginError>(),
            Some(&NativeOriginError::Epoch)
        );

        // verify_with_store reads registry & trust from Redb ConfigStore before measuring runtime.
        let sample_val = "rb24 synthetic signature value";
        let mut receipt = codec::Receipt {
            version: 1,
            issuer: issuer_hex.clone(),
            key_generation: 1,
            build_identity: BUILD_ID.to_string(),
            producer_version: codec::PRODUCER_VERSION.to_string(),
            policy_digest,
            protocol: codec::Protocol::OpenaiResponses,
            route: "/v1/responses".to_string(),
            endpoint_digest: policy.endpoint_digests[0].clone(),
            source_session: "sess_rb24".to_string(),
            source_message: "msg_rb24".to_string(),
            source_part: "npart_0102030405060708090a0b0c0d0e0f10".to_string(),
            stream: "10".repeat(16),
            fragment: "frag_rb24".to_string(),
            fields: vec![codec::FieldDigest {
                name: codec::FieldName::Signature,
                length: u32::try_from(sample_val.len()).expect("u32 field length"),
                sha256: hex::encode(Sha256::digest(sample_val.as_bytes())),
            }],
            signature: STANDARD.encode([0u8; 64]),
        };
        receipt.signature = STANDARD.encode(
            kp.sign(&codec::signing_bytes(&receipt).expect("signing bytes"))
                .as_ref(),
        );
        let raw_receipt = serde_json::to_vec(&receipt).expect("encode receipt");
        let verify_err = verify::verify_with_store(
            cfg_rw.as_ref(),
            &repo_root,
            &raw_receipt,
            &SourceCoordinates {
                session: &receipt.source_session,
                message: &receipt.source_message,
                part: &receipt.source_part,
            },
            &[OriginalField {
                name: codec::FieldName::Signature,
                value: sample_val,
            }],
        )
        .await
        .err()
        .expect("synthetic producer path does not exist on disk");
        assert_eq!(
            verify_err.downcast_ref::<VerificationError>(),
            Some(&VerificationError::Runtime)
        );

        // revoke() dispatches to Redb -> removes opencode trust, marks issuer Revoked, bumps epoch.
        let rev = registry::revoke()
            .await
            .expect("native_origin::registry::revoke on redb");
        assert!(rev.removed);
        assert_eq!(rev.native_revoked, 1);
        assert_eq!(rev.native_epoch, Some(2));
        let after_rev = registry::read_with_store(cfg_rw.as_ref())
            .await
            .expect("read revoked registry")
            .expect("tombstone must be preserved");
        assert_eq!(after_rev.epoch, 2);
        assert_eq!(after_rev.issuers[0].state, IssuerState::Revoked);
        assert!(
            trust::read_trust_with_store(cfg_rw.as_ref(), OPENCODE_EXPORTER_SLUG)
                .await
                .expect("read opencode trust after revoke")
                .is_none()
        );

        // -----------------------------------------------------------------
        // Final invariant: NO SQLite files created anywhere under .libra/
        // -----------------------------------------------------------------
        assert!(
            !storage.join("libra.db").exists(),
            "pure-redb test must never create .libra/libra.db"
        );
        assert!(
            !storage.join("private").join("capture.db").exists(),
            "pure-redb test must never create .libra/private/capture.db"
        );
    }
}
