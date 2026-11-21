//! Single background worker that owns every repository read for the window.
//!
//! The worker binds its reader futures to a task-local request scope, runs async readers on
//! one long-lived current-thread runtime, and publishes numbered snapshots.
//! The UI only sends requests and takes snapshots; it never touches a database
//! or the filesystem on the GPUI main thread (ADR-UID-01).

use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use super::{
    repo,
    snapshot::{DataError, RepoContext, Snapshot},
    state::SourceState,
};
use crate::internal::worktree_scope::{RequestScope, with_request_scope};

/// Upper bound on queued requests; older requests are dropped (superseded).
pub const QUEUE_CAP: usize = 8;
/// Budget for joining the worker when the window closes.
pub const CLOSE_BUDGET: Duration = Duration::from_secs(3);

/// One read result: state plus optional context and message.
pub type ReadOutcome = (SourceState, Option<RepoContext>, Option<String>);
/// A boxed read future; readers are async so the worker can drive them on its
/// own runtime instead of starting a nested runtime.
pub type ReadFuture = Pin<Box<dyn Future<Output = Result<ReadOutcome, DataError>> + Send>>;
/// Reader signature used by the worker (and injected by tests).
pub type Reader = dyn Fn(Option<RequestScope>) -> ReadFuture + Send + Sync;

/// One registered worker handle, keyed by handle id.
type WorkerEntry = (u64, JoinHandle<()>);
/// Process-lifetime owner of workers that did not finish inside the budget.
static WORKER_HANDLES: OnceLock<Mutex<Vec<WorkerEntry>>> = OnceLock::new();
static NEXT_WORKER_ID: AtomicU64 = AtomicU64::new(1);

fn registry() -> &'static Mutex<Vec<WorkerEntry>> {
    WORKER_HANDLES.get_or_init(|| Mutex::new(Vec::new()))
}

struct Shared {
    queue: VecDeque<u64>,
    latest: Option<Snapshot>,
    delivered: Option<u64>,
    seq: u64,
    cancelled: bool,
    done: bool,
    stalled: bool,
}

/// Handle to the window's data worker.
#[derive(Clone)]
pub struct DataHandle {
    inner: Arc<(Mutex<Shared>, Condvar)>,
    reads: Arc<AtomicU64>,
    worker_id: u64,
    worker_thread: Arc<Mutex<Option<thread::ThreadId>>>,
}

impl DataHandle {
    /// Start a worker that reads repository context from the current directory.
    pub fn spawn() -> Self {
        Self::spawn_with(|scope| {
            Box::pin(async move {
                match scope {
                    None => Ok((SourceState::Absent, None, Some("no repository".to_string()))),
                    Some(scope) => match repo::read_context(&scope).await {
                        Ok(context) => Ok((SourceState::Available, Some(context), None)),
                        Err(error) => Ok((error.state, None, Some(error.message))),
                    },
                }
            })
        })
    }

    /// Start a worker with an injected reader (tests and future surfaces).
    pub fn spawn_with<F>(reader: F) -> Self
    where
        F: Fn(Option<RequestScope>) -> ReadFuture + Send + Sync + 'static,
    {
        let inner = Arc::new((
            Mutex::new(Shared {
                queue: VecDeque::new(),
                latest: None,
                delivered: None,
                seq: 0,
                cancelled: false,
                done: false,
                stalled: false,
            }),
            Condvar::new(),
        ));
        let reads = Arc::new(AtomicU64::new(0));
        let worker_thread: Arc<Mutex<Option<thread::ThreadId>>> = Arc::new(Mutex::new(None));
        let worker_thread_slot = Arc::clone(&worker_thread);
        let worker_id = NEXT_WORKER_ID.fetch_add(1, Ordering::SeqCst);
        let worker_inner = Arc::clone(&inner);
        let worker_reads = Arc::clone(&reads);
        let reader: Arc<Reader> = Arc::new(reader);
        match thread::Builder::new()
            .name("libra-ui-data".to_string())
            .spawn(move || {
                if let Ok(mut slot) = worker_thread_slot.lock() {
                    *slot = Some(thread::current().id());
                }
                worker_loop(worker_inner, worker_reads, reader);
            }) {
            Ok(handle) => {
                if let Ok(mut slot) = registry().lock() {
                    slot.push((worker_id, handle));
                }
            }
            Err(error) => {
                let (lock, cv) = &*inner;
                let mut shared = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                shared.latest = Some(Snapshot {
                    seq: 0,
                    state: SourceState::Worker,
                    context: None,
                    message: Some(format!("the data worker could not start: {error}")),
                });
                shared.done = true;
                cv.notify_all();
            }
        }
        Self {
            inner,
            reads,
            worker_id,
            worker_thread,
        }
    }

    /// Test seam: a handle whose worker failed at startup (sequence zero).
    pub fn spawn_with_startup_failure(message: String) -> Self {
        let inner = Arc::new((
            Mutex::new(Shared {
                queue: VecDeque::new(),
                latest: None,
                delivered: None,
                seq: 0,
                cancelled: false,
                done: false,
                stalled: false,
            }),
            Condvar::new(),
        ));
        finish_with_error(&inner, message);
        Self {
            inner,
            reads: Arc::new(AtomicU64::new(0)),
            worker_id: 0,
            worker_thread: Arc::new(Mutex::new(None)),
        }
    }

    /// Queue a read request; returns the sequence number it will answer.
    pub fn request(&self) -> u64 {
        let (lock, cv) = &*self.inner;
        let mut shared = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        shared.seq += 1;
        let seq = shared.seq;
        while shared.queue.len() >= QUEUE_CAP {
            shared.queue.pop_front();
        }
        shared.queue.push_back(seq);
        cv.notify_one();
        seq
    }

    /// Take the newest snapshot that has not been delivered yet.
    pub fn take_snapshot(&self) -> Option<Snapshot> {
        let (lock, _) = &*self.inner;
        let mut shared = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let latest = shared.latest.clone()?;
        if shared.delivered == Some(latest.seq) {
            return None;
        }
        shared.delivered = Some(latest.seq);
        Some(latest)
    }

    /// Number of requests waiting in the bounded queue (test seam).
    pub fn pending_requests(&self) -> usize {
        let (lock, _) = &*self.inner;
        lock.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .queue
            .len()
    }

    /// Number of reads performed by the worker (never by the caller).
    pub fn worker_reads(&self) -> u64 {
        self.reads.load(Ordering::SeqCst)
    }

    /// Thread id of the worker (test seam: proves reads run off the caller).
    pub fn worker_thread_id(&self) -> Option<thread::ThreadId> {
        self.worker_thread.lock().ok().and_then(|slot| *slot)
    }

    /// Whether the worker was recorded as stalled during shutdown.
    pub fn stalled(&self) -> bool {
        let (lock, _) = &*self.inner;
        lock.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .stalled
    }

    /// Stop the worker within [`CLOSE_BUDGET`].
    ///
    /// Returns `true` when the thread joined inside the budget. Otherwise the
    /// handle stays in the process-lifetime registry (never detached) and the
    /// stalled flag is recorded.
    pub fn shutdown(&self) -> bool {
        let (lock, cv) = &*self.inner;
        {
            let mut shared = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            shared.cancelled = true;
            cv.notify_all();
        }
        let mut shared = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let deadline = std::time::Instant::now() + CLOSE_BUDGET;
        loop {
            if shared.done {
                break;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                shared.stalled = true;
                return false;
            }
            let (next, _) = cv
                .wait_timeout(shared, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            shared = next;
        }
        drop(shared);
        // Remove the handle before joining: a slow runtime teardown must not
        // hold the registry lock for other workers.
        let handle = {
            let mut slot = registry()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            slot.iter()
                .position(|(id, _)| *id == self.worker_id)
                .map(|position| slot.remove(position).1)
        };
        if let Some(handle) = handle {
            let _ = handle.join();
        }
        true
    }
}

fn worker_loop(inner: Arc<(Mutex<Shared>, Condvar)>, reads: Arc<AtomicU64>, reader: Arc<Reader>) {
    let workdir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let scope = repo::resolve_scope(workdir);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let runtime = match runtime {
        Ok(runtime) => runtime,
        Err(error) => {
            finish_with_error(&inner, format!("the data runtime could not start: {error}"));
            return;
        }
    };
    {
        let _guard = runtime.enter();
        loop {
            let mut stop = false;
            let seq = {
                let (lock, cv) = &*inner;
                let mut shared = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                loop {
                    if shared.cancelled {
                        stop = true;
                        break 0;
                    }
                    if let Some(seq) = shared.queue.pop_front() {
                        break seq;
                    }
                    shared = cv
                        .wait(shared)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
            };
            if stop {
                break;
            }
            reads.fetch_add(1, Ordering::SeqCst);
            let scope_value = match &scope {
                Ok(scope) => scope.clone(),
                Err(_) => None,
            };
            let outcome = match &scope {
                Err(error) => Err(error.clone()),
                Ok(scope) => runtime.block_on(with_request_scope(scope_value, async {
                    reader(scope.clone()).await
                })),
            };
            let snapshot = match outcome {
                Ok((state, context, message)) => Snapshot {
                    seq,
                    state,
                    context,
                    message,
                },
                Err(error) => Snapshot {
                    seq,
                    state: error.state,
                    context: None,
                    message: Some(error.message),
                },
            };
            let (lock, _) = &*inner;
            let mut shared = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            // Publish only when this response still answers the newest request;
            // obsolete in-flight responses are dropped (supersession).
            if seq == shared.seq {
                shared.latest = Some(snapshot);
            }
        }
    }
    // Include runtime teardown before signalling completion so the close
    // budget covers it.
    drop(runtime);
    mark_done(&inner);
}

fn mark_done(inner: &Arc<(Mutex<Shared>, Condvar)>) {
    let (lock, cv) = &**inner;
    let mut shared = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    shared.done = true;
    cv.notify_all();
}

fn finish_with_error(inner: &Arc<(Mutex<Shared>, Condvar)>, message: String) {
    let (lock, cv) = &**inner;
    let mut shared = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    shared.latest = Some(Snapshot {
        seq: 0,
        state: SourceState::Worker,
        context: None,
        message: Some(message),
    });
    shared.done = true;
    cv.notify_all();
}
