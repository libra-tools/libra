//! Bounded-deadline blocking I/O worker pool sunk from `command::status_probe`.

use crate::internal::worktree_scope::{WorktreeScope, with_request_scope_sync};

/// Per-operation wall-clock budget for a potentially hanging worktree
/// syscall. Debug builds honor `LIBRA_TEST_STATUS_IO_TIMEOUT_MS` so tests can
/// trip it deterministically.
#[doc(hidden)]
pub fn io_op_timeout() -> std::time::Duration {
    if cfg!(debug_assertions)
        && std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_some()
        && let Ok(value) = std::env::var("LIBRA_TEST_STATUS_IO_TIMEOUT_MS")
        && let Ok(ms) = value.parse::<u64>()
        && ms > 0
    {
        return std::time::Duration::from_millis(ms);
    }
    std::time::Duration::from_secs(10)
}

/// Hard cap on simultaneously in-flight I/O workers.
const MAX_INFLIGHT_IO_WORKERS: usize = 8;

type IoJob = Box<dyn FnOnce() + Send + 'static>;

struct IoWorkerPool {
    queue: std::sync::Mutex<std::collections::VecDeque<IoJob>>,
    ready: std::sync::Condvar,
    handles: std::sync::Mutex<Vec<std::thread::JoinHandle<()>>>,
}

static IO_POOL: std::sync::OnceLock<std::sync::Arc<IoWorkerPool>> = std::sync::OnceLock::new();
static IO_BUSY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static IO_WORKERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Run one blocking filesystem operation with a wall-clock deadline.
#[doc(hidden)]
#[allow(clippy::result_unit_err)]
pub fn with_io_deadline<T, F>(op: F) -> Result<T, ()>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    with_io_deadline_bounded(io_op_timeout(), op)
}

/// [`with_io_deadline`] with an explicit budget instead of the per-operation default.
#[doc(hidden)]
#[allow(clippy::result_unit_err)]
pub fn with_io_deadline_bounded<T, F>(budget: std::time::Duration, op: F) -> Result<T, ()>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    use std::sync::atomic::Ordering;

    if budget.is_zero() {
        return Err(());
    }

    let pool = IO_POOL.get_or_init(|| {
        std::sync::Arc::new(IoWorkerPool {
            queue: std::sync::Mutex::new(std::collections::VecDeque::new()),
            ready: std::sync::Condvar::new(),
            handles: std::sync::Mutex::new(Vec::new()),
        })
    });

    if IO_BUSY
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
            (current < MAX_INFLIGHT_IO_WORKERS).then_some(current + 1)
        })
        .is_err()
    {
        return Err(());
    }

    let needed = IO_BUSY.load(Ordering::SeqCst);
    if IO_WORKERS
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |workers| {
            (workers < MAX_INFLIGHT_IO_WORKERS && workers < needed).then_some(workers + 1)
        })
        .is_ok()
    {
        let worker_pool = std::sync::Arc::clone(pool);
        match std::thread::Builder::new()
            .name("libra-status-io".to_string())
            .spawn(move || run_io_worker(&worker_pool))
        {
            Ok(handle) => pool
                .handles
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(handle),
            Err(_) => {
                IO_WORKERS.fetch_sub(1, Ordering::SeqCst);
                IO_BUSY.fetch_sub(1, Ordering::SeqCst);
                return Err(());
            }
        }
    }

    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let hash_kind = git_internal::hash::get_hash_kind();
    let request_scope = WorktreeScope::request_scope();
    let job: IoJob = Box::new(move || {
        git_internal::hash::set_hash_kind(hash_kind);
        let _ = tx.send(with_request_scope_sync(request_scope, op));
        IO_BUSY.fetch_sub(1, Ordering::SeqCst);
    });
    {
        let mut queue = pool
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        queue.push_back(job);
    }
    pool.ready.notify_one();

    rx.recv_timeout(budget).map_err(|_| ())
}

fn run_io_worker(pool: &IoWorkerPool) {
    loop {
        let job = {
            let mut queue = pool
                .queue
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                if let Some(job) = queue.pop_front() {
                    break job;
                }
                queue = pool
                    .ready
                    .wait(queue)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
        };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
    }
}
