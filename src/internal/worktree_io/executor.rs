//! Bounded executor for read-only worktree I/O requests.
//!
//! This module owns scheduling and helper-process lifecycle only. Filesystem
//! operations remain in the read-only protocol handler supplied by the command
//! adapter; no arbitrary command or write capability is exposed here.

use std::{
    collections::VecDeque,
    io::{self, Read},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use super::protocol::{
    FRAME_CAP, IoEvent, IoRequest, ObjectBlobStatus,
    parse_event_frames as parse_protocol_event_frames, write_request,
};

pub(crate) const MAX_INFLIGHT: usize = 8;
pub(crate) const MAX_PENDING: usize = 64;
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(10);

type InProcessHandler = fn(IoRequest, &mut Vec<u8>) -> io::Result<bool>;

#[derive(Clone, Copy)]
struct WorkerConfig {
    worker_arg: &'static str,
    cap_env: &'static str,
    ppid_env: &'static str,
    in_process_handler: InProcessHandler,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Scheduler bounds. Per-request byte, frame, and entry budgets are carried
/// by [`IoRequest`] and enforced by the protocol codec/handlers, so they are
/// deliberately not duplicated in this executor limit type.
pub(crate) struct IoLimits {
    pub(crate) max_inflight: usize,
    pub(crate) max_pending: usize,
}

impl Default for IoLimits {
    fn default() -> Self {
        Self {
            max_inflight: MAX_INFLIGHT,
            max_pending: MAX_PENDING,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub(crate) fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
pub(crate) enum ExecutorError {
    Cancelled,
    DeadlineExpired,
    QueueFull,
    WorkerUnavailable(io::Error),
    Protocol(io::Error),
    Handler(io::Error),
}

impl std::fmt::Display for ExecutorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("worktree I/O request was cancelled"),
            Self::DeadlineExpired => formatter.write_str("worktree I/O request deadline expired"),
            Self::QueueFull => formatter.write_str("worktree I/O queue is full"),
            Self::WorkerUnavailable(error) => {
                write!(formatter, "worktree I/O worker unavailable: {error}")
            }
            Self::Protocol(error) => write!(formatter, "worktree I/O protocol error: {error}"),
            Self::Handler(error) => write!(formatter, "worktree I/O handler error: {error}"),
        }
    }
}

impl std::error::Error for ExecutorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::WorkerUnavailable(error) | Self::Protocol(error) | Self::Handler(error) => {
                Some(error)
            }
            Self::Cancelled | Self::DeadlineExpired | Self::QueueFull => None,
        }
    }
}

pub(crate) struct WorktreeIo {
    pool: Arc<Pool>,
}

static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

struct WorkerProc {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

struct Pool {
    idle: Mutex<Vec<WorkerProc>>,
    pending: Mutex<PendingQueue>,
    ready: Condvar,
    token: String,
    config: WorkerConfig,
    limits: IoLimits,
    dispatchers_started: AtomicUsize,
}

struct PendingQueue {
    jobs: VecDeque<Arc<Job>>,
    capacity: usize,
}

impl PendingQueue {
    fn new(capacity: usize) -> Self {
        Self {
            jobs: VecDeque::new(),
            capacity,
        }
    }

    fn insert(&mut self, job: Arc<Job>) -> Result<(), ExecutorError> {
        if self.jobs.len() >= self.capacity {
            return Err(ExecutorError::QueueFull);
        }
        let idx = self
            .jobs
            .iter()
            .position(|existing| existing.path_key > job.path_key)
            .unwrap_or(self.jobs.len());
        self.jobs.insert(idx, job);
        Ok(())
    }

    fn pop(&mut self) -> Option<Arc<Job>> {
        while let Some(job) = self.jobs.pop_front() {
            if !job.cancel_token.is_cancelled() {
                return Some(job);
            }
        }
        None
    }

    fn remove(&mut self, target: &Arc<Job>) -> bool {
        let Some(index) = self.jobs.iter().position(|job| Arc::ptr_eq(job, target)) else {
            return false;
        };
        self.jobs.remove(index);
        true
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.jobs.len()
    }
}

struct Job {
    request: Mutex<Option<IoRequest>>,
    path_key: Vec<u8>,
    /// Wall-clock bound captured at submit (`now + timeout`).
    deadline: Instant,
    /// Per-stdout-wait window. Status walks use this as a no-progress timeout
    /// (fresh each frame). Object reads set `absolute` and share `deadline`
    /// across queue + every wait so a busy pool cannot stretch the budget.
    window: Duration,
    absolute: bool,
    result_tx: Mutex<Option<std::sync::mpsc::SyncSender<JobOutcome>>>,
    cancel_token: CancellationToken,
}

enum JobOutcome {
    Events(Vec<IoEvent>),
    Error(ExecutorError),
}

impl WorktreeIo {
    pub(crate) fn default() -> Self {
        Self::new(WorkerConfig {
            worker_arg: super::handler::WORKER_ARG,
            cap_env: super::handler::CAP_ENV,
            ppid_env: super::handler::PPID_ENV,
            in_process_handler: super::handler::handle_request_to_buffer,
        })
    }

    fn new(config: WorkerConfig) -> Self {
        Self::new_with_limits(config, IoLimits::default(), true)
    }

    fn new_with_limits(config: WorkerConfig, limits: IoLimits, start_dispatchers: bool) -> Self {
        let token = format!(
            "{:016x}{:016x}",
            NEXT_TOKEN.fetch_add(1, Ordering::Relaxed),
            std::process::id() as u64
        );
        let pool = Arc::new(Pool {
            idle: Mutex::new(Vec::new()),
            pending: Mutex::new(PendingQueue::new(limits.max_pending)),
            ready: Condvar::new(),
            token,
            config,
            limits,
            dispatchers_started: AtomicUsize::new(0),
        });
        if start_dispatchers {
            let mut started = 0usize;
            for index in 0..limits.max_inflight {
                let pool = Arc::clone(&pool);
                if std::thread::Builder::new()
                    .name(format!("libra-status-io-{index}"))
                    .spawn(move || dispatcher_loop(pool))
                    .is_ok()
                {
                    started += 1;
                }
            }
            pool.dispatchers_started.store(started, Ordering::SeqCst);
        }
        Self { pool }
    }

    pub(crate) fn submit(
        &self,
        request: IoRequest,
        path_key: Vec<u8>,
        timeout: Duration,
    ) -> Result<Vec<IoEvent>, ExecutorError> {
        self.submit_with_token(request, path_key, timeout, false, CancellationToken::new())
    }

    pub(crate) fn submit_absolute(
        &self,
        request: IoRequest,
        path_key: Vec<u8>,
        timeout: Duration,
    ) -> Result<Vec<IoEvent>, ExecutorError> {
        self.submit_with_token(request, path_key, timeout, true, CancellationToken::new())
    }

    pub(crate) fn submit_with_token(
        &self,
        request: IoRequest,
        path_key: Vec<u8>,
        timeout: Duration,
        absolute: bool,
        cancel_token: CancellationToken,
    ) -> Result<Vec<IoEvent>, ExecutorError> {
        if cancel_token.is_cancelled() {
            return Err(ExecutorError::Cancelled);
        }
        if timeout.is_zero() {
            return Err(ExecutorError::DeadlineExpired);
        }
        request.validate().map_err(ExecutorError::Protocol)?;
        let pool = &self.pool;
        if pool.dispatchers_started.load(Ordering::SeqCst) == 0 {
            return Err(ExecutorError::WorkerUnavailable(io::Error::other(
                "no status I/O dispatcher could be started",
            )));
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(ExecutorError::DeadlineExpired)?;
        let (tx, rx) = mpsc::sync_channel(1);
        let job = Arc::new(Job {
            request: Mutex::new(Some(request)),
            path_key,
            deadline,
            window: timeout,
            absolute,
            result_tx: Mutex::new(Some(tx)),
            cancel_token,
        });
        {
            let mut pending = pool
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            pending.insert(Arc::clone(&job))?;
            pool.ready.notify_one();
        }
        let wait = if helper_exe_is_cli(pool.config) && !absolute {
            Duration::from_secs(24 * 60 * 60)
        } else {
            deadline
                .saturating_duration_since(Instant::now())
                .saturating_add(Duration::from_secs(1))
        };
        let wait_deadline = Instant::now().checked_add(wait);
        loop {
            if job.cancel_token.is_cancelled() {
                cancel_job(pool, &job);
                return Err(ExecutorError::Cancelled);
            }
            let Some(wait_deadline) = wait_deadline else {
                cancel_job(pool, &job);
                return Err(ExecutorError::DeadlineExpired);
            };
            let remaining = wait_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                cancel_job(pool, &job);
                return Err(ExecutorError::DeadlineExpired);
            }
            match rx.recv_timeout(remaining.min(CANCEL_POLL_INTERVAL)) {
                Ok(JobOutcome::Events(events)) => return Ok(events),
                Ok(JobOutcome::Error(error)) => return Err(error),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    cancel_job(pool, &job);
                    return Err(ExecutorError::WorkerUnavailable(io::Error::other(
                        "worktree I/O dispatcher dropped the request result",
                    )));
                }
            }
        }
    }

    pub(crate) fn helper_available(&self) -> bool {
        helper_exe_is_cli(self.pool.config)
    }
}

fn cancel_job(pool: &Pool, job: &Arc<Job>) {
    job.cancel_token.cancel();
    let mut pending = pool
        .pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    pending.remove(job);
    let _ = job
        .request
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
}

fn spawn_worker(
    config: WorkerConfig,
    token: &str,
    deadline: Instant,
    cancel_token: &CancellationToken,
) -> Result<WorkerProc, ExecutorError> {
    if cancel_token.is_cancelled() {
        return Err(ExecutorError::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(ExecutorError::DeadlineExpired);
    }
    let exe = std::env::current_exe().map_err(ExecutorError::WorkerUnavailable)?;
    let mut command = Command::new(exe);
    command
        .arg(config.worker_arg)
        .env(config.cap_env, token)
        .env(config.ppid_env, std::process::id().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        #[cfg(target_os = "linux")]
        {
            // Own process group so timeout kill(-pid) cannot hit status;
            // PDEATHSIG so a killed/exiting parent still reaps a hung helper.
            // SAFETY: runs in the child after fork, before exec. Only
            // async-signal-safe calls (prctl / getppid / _exit).
            unsafe {
                command.pre_exec(|| {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::getppid() == 1 {
                        libc::_exit(1);
                    }
                    Ok(())
                });
            }
        }
    }
    let mut child = command.spawn().map_err(ExecutorError::WorkerUnavailable)?;
    let Some(stdin) = child.stdin.take() else {
        let stdout = child.stdout.take();
        retire_spawned_child(child, None, stdout);
        return Err(ExecutorError::WorkerUnavailable(io::Error::other(
            "status io worker missing stdin",
        )));
    };
    let Some(mut stdout) = child.stdout.take() else {
        retire_spawned_child(child, Some(stdin), None);
        return Err(ExecutorError::WorkerUnavailable(io::Error::other(
            "status io worker missing stdout",
        )));
    };
    if let Err(error) = set_stdout_nonblocking(&stdout) {
        retire_spawned_child(child, Some(stdin), Some(stdout));
        return Err(ExecutorError::WorkerUnavailable(error));
    }
    let ready = match read_worker_frame(&mut stdout, deadline, cancel_token) {
        Ok(event) => event,
        Err(error) => {
            retire_spawned_child(child, Some(stdin), Some(stdout));
            return Err(error);
        }
    };
    if !matches!(ready, IoEvent::Ready) {
        retire_spawned_child(child, Some(stdin), Some(stdout));
        return Err(ExecutorError::Protocol(io::Error::other(
            "status io worker handshake failed",
        )));
    }
    Ok(WorkerProc {
        child,
        stdin,
        stdout,
    })
}

fn kill_pid(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess},
        };
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !handle.is_null() {
                let _ = TerminateProcess(handle, 1);
                CloseHandle(handle);
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
    }
}

fn handoff_to_reaper<T, F>(item: T, wait: F) -> bool
where
    T: Send + 'static,
    F: FnOnce(T) + Send + 'static,
{
    std::thread::Builder::new()
        .name("libra-status-io-reaper".to_string())
        .spawn(move || wait(item))
        .is_ok()
}

fn reap_child_async(child: Child) {
    let _ = handoff_to_reaper(child, |mut child| {
        let _ = child.wait();
    });
}

fn retire_spawned_child(mut child: Child, stdin: Option<ChildStdin>, stdout: Option<ChildStdout>) {
    // Close both pipe ends before signalling the child, then leave all wait
    // work to the independent reaper. If thread creation is exhausted,
    // dropping Child is still preferable to blocking this dispatcher; the
    // process-group kill above and the Linux PDEATHSIG remain best effort.
    drop(stdin);
    drop(stdout);
    kill_pid(child.id());
    let _ = child.kill();
    reap_child_async(child);
}

fn kill_worker(worker: WorkerProc) {
    let WorkerProc {
        child,
        stdin,
        stdout,
    } = worker;
    retire_spawned_child(child, Some(stdin), Some(stdout));
}

fn dispatcher_loop(pool: Arc<Pool>) {
    loop {
        let job = {
            let mut pending = pool
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                if let Some(job) = pending.pop() {
                    break job;
                }
                pending = pool
                    .ready
                    .wait(pending)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };
        run_job(&pool, job);
    }
}

fn helper_exe_is_cli(_config: WorkerConfig) -> bool {
    static CLI: OnceLock<bool> = OnceLock::new();
    *CLI.get_or_init(|| {
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        // Cargo test harnesses live in `target/.../deps/`; the installed /
        // `cargo run` CLI is `…/libra` (or `libra.exe`).
        let in_deps = exe
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|name| name == "deps");
        if in_deps {
            return false;
        }
        exe.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "libra" || name.eq_ignore_ascii_case("libra.exe"))
    })
}

fn run_job(pool: &Pool, job: Arc<Job>) {
    if job.cancel_token.is_cancelled() {
        return;
    }
    let request = job
        .request
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    let Some(request) = request else {
        return;
    };
    if job.cancel_token.is_cancelled() {
        return;
    }
    if job.absolute && Instant::now() >= job.deadline {
        if let Some(tx) = job
            .result_tx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = tx.send(JobOutcome::Error(ExecutorError::DeadlineExpired));
        }
        return;
    }
    let worker_deadline = if job.absolute {
        Some(job.deadline)
    } else {
        Instant::now().checked_add(job.window)
    };
    let outcome = match worker_deadline {
        None => JobOutcome::Error(ExecutorError::DeadlineExpired),
        Some(worker_deadline) => match take_worker(pool, worker_deadline, &job.cancel_token) {
            // CLI helper spawn/handshake failed (EMFILE, process limit). Do not
            // fall back to an unkillable in-process syscall — that would pin a
            // dispatcher forever and exhaust the pool. Absolute object reads
            // (WIO-03) are the same: never `run_in_process` them on a pool
            // thread. Library/test binaries keep in-process only for relative
            // (no-progress) probe opcodes (R0).
            Err(error) if helper_exe_is_cli(pool.config) || job.absolute => {
                JobOutcome::Error(error)
            }
            Err(_error) => run_in_process(pool.config.in_process_handler, request),
            Ok(mut worker) => {
                let drive = drive_worker(
                    &mut worker,
                    &pool.token,
                    request,
                    job.deadline,
                    job.window,
                    job.absolute,
                    &job.cancel_token,
                );
                if job.cancel_token.is_cancelled() || drive.timed_out || !drive.reuse {
                    kill_worker(worker);
                } else {
                    recycle_worker(pool, worker);
                }
                if drive.events.is_empty() {
                    if let Some(error) = drive.error {
                        JobOutcome::Error(error)
                    } else if drive.timed_out {
                        JobOutcome::Error(ExecutorError::DeadlineExpired)
                    } else {
                        JobOutcome::Error(ExecutorError::Protocol(io::Error::other(
                            "status io worker returned no terminal event",
                        )))
                    }
                } else {
                    JobOutcome::Events(drive.events)
                }
            }
        },
    };
    if job.cancel_token.is_cancelled() {
        return;
    }
    if let Some(tx) = job
        .result_tx
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
    {
        let _ = tx.send(outcome);
    }
}

/// Library / in-process callers (`status::execute_to`, `cargo test` unit
/// binaries) cannot spawn the CLI helper. Run the opcode on this dispatcher
/// thread (already one of the 8 bounded slots). Hung syscalls remain
/// unkillable here; WIO-01 killability applies to the `libra` CLI worker.
fn run_in_process(handler: InProcessHandler, request: IoRequest) -> JobOutcome {
    let mut buf = Vec::new();
    match handler(request, &mut buf) {
        Ok(true) => match parse_event_frames(&buf) {
            Some(events) if !events.is_empty() => JobOutcome::Events(events),
            _ => JobOutcome::Error(ExecutorError::Protocol(io::Error::other(
                "in-process status I/O handler returned no events",
            ))),
        },
        Ok(false) => JobOutcome::Error(ExecutorError::Handler(io::Error::other(
            "in-process status I/O handler shut down",
        ))),
        Err(error) => JobOutcome::Error(ExecutorError::Handler(error)),
    }
}

fn parse_event_frames(data: &[u8]) -> Option<Vec<IoEvent>> {
    parse_protocol_event_frames(data)
}

fn take_worker(
    pool: &Pool,
    deadline: Instant,
    cancel_token: &CancellationToken,
) -> Result<WorkerProc, ExecutorError> {
    if !helper_exe_is_cli(pool.config) {
        return Err(ExecutorError::WorkerUnavailable(io::Error::other(
            "status helper is not available in this executable",
        )));
    }
    {
        let mut idle = pool
            .idle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(worker) = idle.pop() {
            return Ok(worker);
        }
    }
    spawn_worker(pool.config, &pool.token, deadline, cancel_token)
}

fn recycle_worker(pool: &Pool, worker: WorkerProc) {
    let mut idle = pool
        .idle
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if idle.len() < pool.limits.max_inflight {
        idle.push(worker);
    } else {
        let worker = worker;
        kill_worker(worker);
    }
}

/*
 * The executor owns the remaining stdout polling and framed protocol drive
 * below. Keeping these routines here ensures status only supplies a handler
 * for the in-process test/library fallback.
 */

#[cfg(unix)]
type StdoutWaitHandle = std::os::fd::RawFd;
#[cfg(windows)]
type StdoutWaitHandle = windows_sys::Win32::Foundation::HANDLE;
#[cfg(not(any(unix, windows)))]
type StdoutWaitHandle = ();

fn stdout_wait_handle(stdout: &ChildStdout) -> StdoutWaitHandle {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        stdout.as_raw_fd()
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        stdout.as_raw_handle() as StdoutWaitHandle
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = stdout;
        ()
    }
}

fn set_stdout_nonblocking(stdout: &ChildStdout) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let fd = stdout.as_raw_fd();
        // SAFETY: `fd` is borrowed from a live ChildStdout and the fcntl
        // operations only read and update its descriptor flags.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: same descriptor remains owned by `stdout` for this call.
        if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        // PeekNamedPipe below prevents blocking reads on anonymous pipes.
        let _ = stdout;
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = stdout;
        Ok(())
    }
}

fn wait_stdout_readable(
    handle: StdoutWaitHandle,
    timeout: Duration,
    cancel_token: &CancellationToken,
) -> io::Result<()> {
    if cancel_token.is_cancelled() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "worktree I/O request cancelled",
        ));
    }
    #[cfg(unix)]
    {
        let mut pollfd = libc::pollfd {
            fd: handle,
            events: libc::POLLIN,
            revents: 0,
        };
        let Some(deadline) = Instant::now().checked_add(timeout) else {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "status io worker timeout",
            ));
        };
        loop {
            if cancel_token.is_cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "worktree I/O request cancelled",
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "status io worker timeout",
                ));
            }
            let millis = remaining
                .min(CANCEL_POLL_INTERVAL)
                .as_millis()
                .max(1)
                .min(i32::MAX as u128) as libc::c_int;
            let n = unsafe { libc::poll(&mut pollfd, 1, millis) };
            if n < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if n > 0 && pollfd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                return Ok(());
            }
            pollfd.revents = 0;
        }
    }
    #[cfg(windows)]
    {
        // Anonymous pipe handles are not waitable synchronization objects;
        // `WaitForSingleObject` returns WAIT_FAILED. PeekNamedPipe reports
        // pending bytes (or ERROR_BROKEN_PIPE on EOF).
        use std::os::windows::io::AsRawHandle;

        use windows_sys::Win32::{
            Foundation::{ERROR_BROKEN_PIPE, HANDLE},
            System::Pipes::PeekNamedPipe,
        };
        const POLL_SLICE: Duration = Duration::from_millis(5);
        let Some(deadline) = std::time::Instant::now().checked_add(timeout) else {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "status io worker timeout",
            ));
        };
        let handle = handle as HANDLE;
        loop {
            if cancel_token.is_cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "worktree I/O request cancelled",
                ));
            }
            let mut avail: u32 = 0;
            let ok = unsafe {
                PeekNamedPipe(
                    handle,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut avail,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                    return Ok(());
                }
                return Err(error);
            }
            if avail > 0 {
                return Ok(());
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "status io worker timeout",
                ));
            }
            std::thread::sleep(POLL_SLICE.min(deadline.saturating_duration_since(now)));
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (timeout, cancel_token);
        Ok(())
    }
}

fn read_exact_bounded<R, F>(
    reader: &mut R,
    buffer: &mut [u8],
    deadline: Instant,
    cancel_token: &CancellationToken,
    mut wait_readable: F,
) -> io::Result<()>
where
    R: Read,
    F: FnMut(Duration, &CancellationToken) -> io::Result<()>,
{
    let mut offset = 0usize;
    while offset < buffer.len() {
        if cancel_token.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "worktree I/O request cancelled",
            ));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "status io worker frame timeout",
            ));
        }
        wait_readable(remaining, cancel_token)?;
        match reader.read(&mut buffer[offset..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "status io worker frame ended before its payload",
                ));
            }
            Ok(read) => offset += read,
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn classify_frame_error(error: io::Error, cancel_token: &CancellationToken) -> ExecutorError {
    if cancel_token.is_cancelled() || error.kind() == io::ErrorKind::Interrupted {
        ExecutorError::Cancelled
    } else if error.kind() == io::ErrorKind::TimedOut {
        ExecutorError::DeadlineExpired
    } else {
        ExecutorError::Protocol(error)
    }
}

fn read_frame_bounded<R, F>(
    reader: &mut R,
    deadline: Instant,
    cancel_token: &CancellationToken,
    wait_readable: F,
) -> Result<IoEvent, ExecutorError>
where
    R: Read,
    F: FnMut(Duration, &CancellationToken) -> io::Result<()>,
{
    let mut len_buf = [0u8; 4];
    let mut wait_readable = wait_readable;
    if let Err(error) = read_exact_bounded(
        reader,
        &mut len_buf,
        deadline,
        cancel_token,
        &mut wait_readable,
    ) {
        return Err(classify_frame_error(error, cancel_token));
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 || len > FRAME_CAP {
        return Err(ExecutorError::Protocol(io::Error::new(
            io::ErrorKind::InvalidData,
            "status io worker frame length invalid",
        )));
    }
    let mut payload = vec![0u8; len];
    if let Err(error) = read_exact_bounded(
        reader,
        &mut payload,
        deadline,
        cancel_token,
        &mut wait_readable,
    ) {
        return Err(classify_frame_error(error, cancel_token));
    }
    serde_json::from_slice(&payload)
        .map_err(|error| ExecutorError::Protocol(io::Error::new(io::ErrorKind::InvalidData, error)))
}

fn read_raw_frame_bounded<R, F>(
    reader: &mut R,
    deadline: Instant,
    cancel_token: &CancellationToken,
    wait_readable: F,
) -> Result<Vec<u8>, ExecutorError>
where
    R: Read,
    F: FnMut(Duration, &CancellationToken) -> io::Result<()>,
{
    let mut len_buf = [0u8; 4];
    let mut wait_readable = wait_readable;
    if let Err(error) = read_exact_bounded(
        reader,
        &mut len_buf,
        deadline,
        cancel_token,
        &mut wait_readable,
    ) {
        return Err(classify_frame_error(error, cancel_token));
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > FRAME_CAP {
        return Err(ExecutorError::Protocol(io::Error::new(
            io::ErrorKind::InvalidData,
            "status io worker binary frame length invalid",
        )));
    }
    let mut payload = vec![0u8; len];
    if let Err(error) = read_exact_bounded(
        reader,
        &mut payload,
        deadline,
        cancel_token,
        &mut wait_readable,
    ) {
        return Err(classify_frame_error(error, cancel_token));
    }
    Ok(payload)
}

fn read_worker_frame(
    stdout: &mut ChildStdout,
    deadline: Instant,
    cancel_token: &CancellationToken,
) -> Result<IoEvent, ExecutorError> {
    let handle = stdout_wait_handle(stdout);
    read_frame_bounded(stdout, deadline, cancel_token, |timeout, cancel| {
        wait_stdout_readable(handle, timeout, cancel)
    })
}

fn read_worker_raw_frame(
    stdout: &mut ChildStdout,
    deadline: Instant,
    cancel_token: &CancellationToken,
) -> Result<Vec<u8>, ExecutorError> {
    let handle = stdout_wait_handle(stdout);
    read_raw_frame_bounded(stdout, deadline, cancel_token, |timeout, cancel| {
        wait_stdout_readable(handle, timeout, cancel)
    })
}

struct DriveResult {
    events: Vec<IoEvent>,
    timed_out: bool,
    reuse: bool,
    error: Option<ExecutorError>,
}

fn drive_frame_error(events: Vec<IoEvent>, error: ExecutorError) -> DriveResult {
    match error {
        ExecutorError::Cancelled => DriveResult {
            events,
            timed_out: false,
            reuse: false,
            error: Some(ExecutorError::Cancelled),
        },
        ExecutorError::DeadlineExpired => DriveResult {
            events,
            timed_out: true,
            reuse: false,
            error: None,
        },
        error => DriveResult {
            events,
            timed_out: false,
            reuse: false,
            error: Some(error),
        },
    }
}

fn drive_worker(
    worker: &mut WorkerProc,
    token: &str,
    request: IoRequest,
    deadline: Instant,
    window: Duration,
    absolute: bool,
    cancel_token: &CancellationToken,
) -> DriveResult {
    if cancel_token.is_cancelled() {
        return DriveResult {
            events: Vec::new(),
            timed_out: false,
            reuse: false,
            error: Some(ExecutorError::Cancelled),
        };
    }
    if let Err(error) = write_request(&mut worker.stdin, token, request) {
        return DriveResult {
            events: Vec::new(),
            timed_out: false,
            reuse: false,
            error: Some(ExecutorError::Protocol(error)),
        };
    }
    let mut events = Vec::new();
    loop {
        let frame_deadline = if absolute {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return DriveResult {
                    events,
                    timed_out: true,
                    reuse: false,
                    error: None,
                };
            }
            deadline
        } else {
            // No-progress: each frame gets a fresh window so a wide
            // progressing `read_dir` is not cut by an absolute job clock.
            let Some(frame_deadline) = Instant::now().checked_add(window) else {
                return DriveResult {
                    events,
                    timed_out: true,
                    reuse: false,
                    error: None,
                };
            };
            frame_deadline
        };
        let event = match read_worker_frame(&mut worker.stdout, frame_deadline, cancel_token) {
            Ok(event) => event,
            Err(error) => return drive_frame_error(events, error),
        };
        if cancel_token.is_cancelled() {
            return DriveResult {
                events,
                timed_out: false,
                reuse: false,
                error: Some(ExecutorError::Cancelled),
            };
        }
        let reuse = !matches!(event, IoEvent::Error { .. });
        // Ok payloads travel as a trailing length-prefixed binary frame
        // (not base64 in JSON) so a 2 MiB blob stays within the ≤20%
        // wire-overhead budget (WIO-03).
        let event = if let IoEvent::DoneObjectBlob {
            status: ObjectBlobStatus::Ok,
            bytes: None,
        } = &event
        {
            let raw_deadline = if absolute {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return DriveResult {
                        events,
                        timed_out: true,
                        reuse: false,
                        error: None,
                    };
                }
                deadline
            } else {
                let Some(raw_deadline) = Instant::now().checked_add(window) else {
                    return DriveResult {
                        events,
                        timed_out: true,
                        reuse: false,
                        error: None,
                    };
                };
                raw_deadline
            };
            match read_worker_raw_frame(&mut worker.stdout, raw_deadline, cancel_token) {
                Ok(bytes) => IoEvent::DoneObjectBlob {
                    status: ObjectBlobStatus::Ok,
                    bytes: Some(bytes),
                },
                Err(error) => return drive_frame_error(events, error),
            }
        } else {
            event
        };
        let done = matches!(
            event,
            IoEvent::DoneStat { .. }
                | IoEvent::DoneCanonicalize { .. }
                | IoEvent::DoneReadDir { .. }
                | IoEvent::DoneHash { .. }
                | IoEvent::DoneObjectBlob { .. }
                | IoEvent::DoneMarker { .. }
                | IoEvent::Error { .. }
        );
        events.push(event);
        if done {
            return DriveResult {
                events,
                timed_out: false,
                reuse,
                error: None,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, io::Read};

    use super::*;

    struct ChunkedReader {
        chunks: VecDeque<Vec<u8>>,
    }

    impl Read for ChunkedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let Some(chunk) = self.chunks.pop_front() else {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "unit-test reader is stalled",
                ));
            };
            let count = buffer.len().min(chunk.len());
            buffer[..count].copy_from_slice(&chunk[..count]);
            if count < chunk.len() {
                self.chunks.push_front(chunk[count..].to_vec());
            }
            Ok(count)
        }
    }

    fn wait_until_stalled(
        calls: &mut usize,
        stop_after: usize,
    ) -> impl FnMut(Duration, &CancellationToken) -> io::Result<()> + '_ {
        move |_timeout, cancel_token| {
            *calls += 1;
            if cancel_token.is_cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "unit-test cancellation",
                ));
            }
            if *calls >= stop_after {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "unit-test frame timeout",
                ));
            }
            Ok(())
        }
    }

    fn frame_wire(event: &IoEvent) -> Vec<u8> {
        let mut wire = Vec::new();
        super::super::protocol::write_frame(&mut wire, event).expect("frame encoding");
        wire
    }

    fn raw_frame_wire(bytes: &[u8]) -> Vec<u8> {
        let mut wire = Vec::new();
        super::super::protocol::write_raw_frame(&mut wire, bytes).expect("raw frame encoding");
        wire
    }

    fn test_config(handler: InProcessHandler) -> WorkerConfig {
        WorkerConfig {
            worker_arg: "unused-in-unit-test",
            cap_env: "LIBRA_TEST_UNUSED_CAP",
            ppid_env: "LIBRA_TEST_UNUSED_PPID",
            in_process_handler: handler,
        }
    }

    fn test_job(path_key: &[u8], cancel_token: CancellationToken) -> Arc<Job> {
        let (result_tx, _result_rx) = mpsc::sync_channel(1);
        Arc::new(Job {
            request: Mutex::new(Some(IoRequest::Shutdown)),
            path_key: path_key.to_vec(),
            deadline: Instant::now() + Duration::from_secs(1),
            window: Duration::from_secs(1),
            absolute: false,
            result_tx: Mutex::new(Some(result_tx)),
            cancel_token,
        })
    }

    fn write_marker(request: IoRequest, output: &mut Vec<u8>) -> io::Result<bool> {
        let _ = request;
        super::super::protocol::write_frame(
            output,
            &IoEvent::DoneMarker {
                present: Some(true),
                err_kind: None,
                err_raw_os: None,
            },
        )?;
        Ok(true)
    }

    fn write_marker_after_delay(request: IoRequest, output: &mut Vec<u8>) -> io::Result<bool> {
        std::thread::sleep(Duration::from_millis(25));
        write_marker(request, output)
    }

    fn fail_handler(_request: IoRequest, _output: &mut Vec<u8>) -> io::Result<bool> {
        Err(io::Error::other("unit-test handler failure"))
    }

    fn malformed_handler(_request: IoRequest, output: &mut Vec<u8>) -> io::Result<bool> {
        output.extend_from_slice(b"not a framed event");
        Ok(true)
    }

    #[test]
    fn default_limits_pin_existing_bounds() {
        let limits = IoLimits::default();
        assert_eq!(limits.max_inflight, MAX_INFLIGHT);
        assert_eq!(limits.max_pending, MAX_PENDING);
        assert_eq!(MAX_INFLIGHT, 8);
        assert_eq!(MAX_PENDING, 64);
    }

    #[test]
    fn pending_queue_is_stably_sorted_and_bounded() {
        let mut queue = PendingQueue::new(MAX_PENDING);
        let first_equal = test_job(b"same", CancellationToken::new());
        let second_equal = test_job(b"same", CancellationToken::new());
        queue
            .insert(test_job(b"z", CancellationToken::new()))
            .expect("z fits");
        queue
            .insert(first_equal.clone())
            .expect("first equal key fits");
        queue
            .insert(test_job(b"a", CancellationToken::new()))
            .expect("a fits");
        queue
            .insert(second_equal.clone())
            .expect("second equal key fits");

        assert_eq!(queue.pop().expect("a is first").path_key, b"a");
        let next = queue.pop().expect("same key is next");
        assert!(Arc::ptr_eq(&next, &first_equal));
        let next = queue.pop().expect("same key retains FIFO");
        assert!(Arc::ptr_eq(&next, &second_equal));
        assert_eq!(queue.pop().expect("z is last").path_key, b"z");
        assert_eq!(queue.len(), 0);

        let mut full = PendingQueue::new(MAX_PENDING);
        for index in 0..MAX_PENDING {
            full.insert(test_job(
                format!("{index:03}").as_bytes(),
                CancellationToken::new(),
            ))
            .expect("queue must accept up to its cap");
        }
        assert!(matches!(
            full.insert(test_job(b"overflow", CancellationToken::new())),
            Err(ExecutorError::QueueFull)
        ));
        assert_eq!(full.len(), MAX_PENDING);
    }

    #[test]
    fn zero_deadline_is_rejected_before_enqueue() {
        let executor =
            WorktreeIo::new_with_limits(test_config(write_marker), IoLimits::default(), false);
        let result = executor.submit(IoRequest::Shutdown, b"expired".to_vec(), Duration::ZERO);
        assert!(matches!(result, Err(ExecutorError::DeadlineExpired)));
        let pending = executor
            .pool
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn pre_cancelled_request_is_rejected_before_enqueue() {
        let executor =
            WorktreeIo::new_with_limits(test_config(write_marker), IoLimits::default(), false);
        let token = CancellationToken::new();
        token.cancel();
        let result = executor.submit_with_token(
            IoRequest::Shutdown,
            b"cancelled".to_vec(),
            Duration::from_secs(1),
            false,
            token,
        );
        assert!(matches!(result, Err(ExecutorError::Cancelled)));
        let pending = executor
            .pool
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn cancelled_pending_job_is_removed_when_dispatched() {
        let mut queue = PendingQueue::new(MAX_PENDING);
        let token = CancellationToken::new();
        let job = test_job(b"cancelled", token.clone());
        queue.insert(job).expect("cancelled job fits");
        token.cancel();
        assert!(queue.pop().is_none());
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn handler_and_protocol_failures_remain_typed() {
        let handler = run_in_process(fail_handler, IoRequest::Shutdown);
        assert!(matches!(
            handler,
            JobOutcome::Error(ExecutorError::Handler(_))
        ));
        let protocol = run_in_process(malformed_handler, IoRequest::Shutdown);
        assert!(matches!(
            protocol,
            JobOutcome::Error(ExecutorError::Protocol(_))
        ));
    }

    #[test]
    fn reaper_handoff_does_not_block_dispatcher_when_wait_stalls() {
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(0);
        let (next_tx, next_rx) = mpsc::sync_channel(1);
        let started = Instant::now();
        let handed_off = handoff_to_reaper((), move |_| {
            let _ = started_tx.send(());
            let _ = release_rx.recv();
        });
        assert!(handed_off, "reaper thread should be available in tests");
        started_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("reaper must begin waiting independently");

        next_tx
            .send(())
            .expect("dispatcher slot must remain available");
        assert!(next_rx.recv_timeout(Duration::from_millis(200)).is_ok());
        assert!(started.elapsed() < Duration::from_millis(500));
        drop(release_tx);
    }

    #[test]
    fn partial_length_header_returns_typed_deadline() {
        let wire = frame_wire(&IoEvent::Ready);
        let mut reader = ChunkedReader {
            chunks: VecDeque::from([wire[..1].to_vec()]),
        };
        let token = CancellationToken::new();
        let started = Instant::now();
        let mut calls = 0;
        let result = read_frame_bounded(
            &mut reader,
            Instant::now() + Duration::from_millis(25),
            &token,
            wait_until_stalled(&mut calls, 2),
        );
        assert!(matches!(result, Err(ExecutorError::DeadlineExpired)));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn partial_json_payload_returns_typed_deadline() {
        let wire = frame_wire(&IoEvent::Ready);
        let mut reader = ChunkedReader {
            chunks: VecDeque::from([wire[..4].to_vec(), wire[4..5].to_vec()]),
        };
        let token = CancellationToken::new();
        let mut calls = 0;
        let result = read_frame_bounded(
            &mut reader,
            Instant::now() + Duration::from_millis(25),
            &token,
            wait_until_stalled(&mut calls, 2),
        );
        assert!(matches!(result, Err(ExecutorError::DeadlineExpired)));
    }

    #[test]
    fn partial_object_blob_payload_returns_typed_deadline() {
        let event_wire = frame_wire(&IoEvent::DoneObjectBlob {
            status: ObjectBlobStatus::Ok,
            bytes: None,
        });
        let raw_wire = raw_frame_wire(b"partial");
        let mut reader = ChunkedReader {
            chunks: VecDeque::from([event_wire, raw_wire[..4].to_vec(), raw_wire[4..5].to_vec()]),
        };
        let token = CancellationToken::new();
        let mut calls = 0;
        let mut waiter = wait_until_stalled(&mut calls, 5);
        let event = read_frame_bounded(
            &mut reader,
            Instant::now() + Duration::from_millis(25),
            &token,
            &mut waiter,
        );
        assert!(matches!(
            event,
            Ok(IoEvent::DoneObjectBlob {
                status: ObjectBlobStatus::Ok,
                bytes: None
            })
        ));
        let result = read_raw_frame_bounded(
            &mut reader,
            Instant::now() + Duration::from_millis(25),
            &token,
            &mut waiter,
        );
        assert!(matches!(result, Err(ExecutorError::DeadlineExpired)));
    }

    #[test]
    fn stalled_ready_handshake_returns_typed_deadline() {
        let wire = frame_wire(&IoEvent::Ready);
        let mut reader = ChunkedReader {
            chunks: VecDeque::from([wire[..4].to_vec(), wire[4..5].to_vec()]),
        };
        let token = CancellationToken::new();
        let mut calls = 0;
        let result = read_frame_bounded(
            &mut reader,
            Instant::now() + Duration::from_millis(25),
            &token,
            wait_until_stalled(&mut calls, 2),
        );
        assert!(matches!(result, Err(ExecutorError::DeadlineExpired)));
    }

    #[test]
    fn partial_frame_observes_cancellation() {
        let wire = frame_wire(&IoEvent::Ready);
        let mut reader = ChunkedReader {
            chunks: VecDeque::from([wire[..1].to_vec()]),
        };
        let token = CancellationToken::new();
        let cancel_token = token.clone();
        let mut calls = 0;
        let result = read_frame_bounded(
            &mut reader,
            Instant::now() + Duration::from_secs(5),
            &token,
            move |_timeout, _| {
                calls += 1;
                if calls >= 2 {
                    cancel_token.cancel();
                }
                Ok(())
            },
        );
        assert!(matches!(result, Err(ExecutorError::Cancelled)));
    }

    #[test]
    fn cancelled_running_job_releases_dispatcher_slot() {
        let executor = WorktreeIo::new_with_limits(
            test_config(write_marker_after_delay),
            IoLimits {
                max_inflight: 1,
                max_pending: MAX_PENDING,
            },
            true,
        );
        let token = CancellationToken::new();
        let cancel_token = token.clone();
        let cancel_thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            cancel_token.cancel();
        });
        let started = Instant::now();
        let first = executor.submit_with_token(
            IoRequest::Shutdown,
            b"first".to_vec(),
            Duration::from_secs(5),
            false,
            token,
        );
        cancel_thread.join().expect("cancellation thread");
        assert!(matches!(first, Err(ExecutorError::Cancelled)));
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "external cancellation must not wait for the request deadline"
        );

        let second = executor.submit(
            IoRequest::Shutdown,
            b"second".to_vec(),
            Duration::from_secs(1),
        );
        assert!(matches!(second, Ok(events) if !events.is_empty()));
    }
}
