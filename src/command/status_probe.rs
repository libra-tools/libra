//! Bounded rename-destination probe (plan-20260714 R0-3, §B.3.1–§B.3.2,
//! §B.3.5).
//!
//! Under the `status.renameUntracked` extension, unstaged rename
//! DESTINATIONS come from this probe — decoupled from the untracked display
//! scan (`-uno` hides display markers but never the probe), bounded by a
//! call-global dual budget, and qualified by the same tracked/ignore
//! layering the display scan uses. The probe never injects `?`/`??` markers
//! into the display layer.

use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
};

use git_internal::internal::index::Index;

use crate::{
    command::status_untracked_paths::TrackedPaths,
    utils::{pathspec::PathspecSet, util},
};

/// Cross-root enumeration budget (§B.3.2): every entry taken from a
/// `read_dir` counts, including ones excluded afterwards.
pub(crate) const PROBE_MAX_ENUMERATED_ENTRIES: usize = 50_000;
/// Qualified-destination budget (§B.3.2): counted separately so candidate
/// storage stays bounded even under the enumeration cap.
pub(crate) const PROBE_MAX_QUALIFIED_DESTINATIONS: usize = 10_000;

/// Injectable probe limits (GC-DR-07 pattern: debug-only env overrides so
/// tests can trip the budgets without 50k-file fixtures).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProbeLimits {
    pub(crate) max_enumerated_entries: usize,
    pub(crate) max_qualified_destinations: usize,
}

impl ProbeLimits {
    pub(crate) fn effective() -> Self {
        let read = |name: &str, default: usize| -> usize {
            if cfg!(debug_assertions)
                && std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_some()
                && let Ok(value) = std::env::var(name)
                && let Ok(parsed) = value.parse::<usize>()
                && parsed > 0
            {
                return parsed.min(default);
            }
            default
        };
        Self {
            max_enumerated_entries: read(
                "LIBRA_TEST_STATUS_PROBE_ENUM_BUDGET",
                PROBE_MAX_ENUMERATED_ENTRIES,
            ),
            max_qualified_destinations: read(
                "LIBRA_TEST_STATUS_PROBE_DEST_BUDGET",
                PROBE_MAX_QUALIFIED_DESTINATIONS,
            ),
        }
    }
}

/// Why a path could not be inspected (§B.6.0.1 reason taxonomy; the JSON
/// serde contract lands with R0-8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IoBlockedReason {
    PermissionDenied,
    IoError,
    /// A single worktree I/O operation exceeded [`IO_OP_TIMEOUT`] (a hung
    /// NFS/FUSE mount): the status RUN is reclaimed and the path is
    /// reported blocked instead of hanging forever (§B.3.3).
    IoTimeout,
}

impl IoBlockedReason {
    fn from_error(error: &io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::PermissionDenied => IoBlockedReason::PermissionDenied,
            // A syscall can return `TimedOut` on its own (a mount that gave
            // up), not only through the watchdog. Folding it into `IoError`
            // would break the public `reason` contract for exactly the case
            // it exists to describe.
            io::ErrorKind::TimedOut => IoBlockedReason::IoTimeout,
            _ => IoBlockedReason::IoError,
        }
    }
}

/// Per-operation wall-clock budget for a potentially hanging worktree
/// syscall (§B.3.3 "10 秒后回收并报告 io_timeout"). Debug builds honor
/// `LIBRA_TEST_STATUS_IO_TIMEOUT_MS` so tests can trip it deterministically.
pub(crate) fn io_op_timeout() -> std::time::Duration {
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

/// Hard cap on simultaneously in-flight I/O workers (§B.3.4 并发上限 8).
const MAX_INFLIGHT_IO_WORKERS: usize = 8;

type IoJob = Box<dyn FnOnce() + Send + 'static>;

/// A fixed-size, lazily grown pool of REUSED worker threads. Reuse is the
/// point: a status run that trips the deadline thousands of times must not
/// create thousands of threads, and a worker that finishes late rejoins the
/// pool instead of dying detached.
struct IoWorkerPool {
    queue: std::sync::Mutex<std::collections::VecDeque<IoJob>>,
    ready: std::sync::Condvar,
    /// Join handles of every spawned worker. Workers park on `ready`
    /// between jobs and are REUSED, but their handles stay owned by the
    /// pool: dropping a handle would detach the thread, and the scan
    /// contract is a bounded pool with NO detached threads (§B.3.4).
    handles: std::sync::Mutex<Vec<std::thread::JoinHandle<()>>>,
}

static IO_POOL: std::sync::OnceLock<std::sync::Arc<IoWorkerPool>> = std::sync::OnceLock::new();
/// Slots currently held by a submitted job. A worker still stuck inside a
/// hung syscall keeps its slot, so the pool degrades to fail-fast rather
/// than growing without bound.
static IO_BUSY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static IO_WORKERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Run one blocking filesystem operation with a wall-clock deadline.
///
/// A hung syscall cannot be interrupted in safe Rust, so the operation runs
/// on a pooled worker thread: on timeout the CALLER is reclaimed (it records
/// an `IoTimeout` blocked event and continues) while the worker stays parked
/// in the kernel call, holding one of the eight slots until the mount
/// answers. This bounds `status` — the documented contract — without
/// claiming to abort the kernel call itself. When all eight slots are held,
/// further timed reads fail fast as timeouts instead of queueing behind a
/// hang. (§B.3.3; the reclaimable out-of-process worker described in §B.3.2
/// is deferred to plan-20260715 W-IO.)
pub(crate) fn with_io_deadline<T, F>(op: F) -> Result<T, ()>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    with_io_deadline_bounded(io_op_timeout(), op)
}

/// [`with_io_deadline`] with an EXPLICIT budget instead of the per-operation
/// default.
///
/// A caller with a batch budget cannot express it through the default: the
/// per-operation deadline gates how long ONE read may take, so a batch with
/// 0.1s left still hands the worker a full 10s window and overshoots its own
/// cap without any degradation signal. Passing the remaining batch time makes
/// the tighter of the two bounds win.
pub(crate) fn with_io_deadline_bounded<T, F>(budget: std::time::Duration, op: F) -> Result<T, ()>
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
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
            (current < MAX_INFLIGHT_IO_WORKERS).then_some(current + 1)
        })
        .is_err()
    {
        // Every slot is held by an earlier hung read; refuse immediately
        // rather than wait out a deadline that cannot be met.
        return Err(());
    }

    // Grow the pool only while there are fewer live workers than reserved
    // slots, and never past the cap.
    let needed = IO_BUSY.load(Ordering::SeqCst);
    if IO_WORKERS
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |workers| {
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
    // The repository hash kind is THREAD-LOCAL and set on the main thread by
    // the CLI preflight. A pooled worker starts at the SHA-1 default, so a
    // worktree OID hashed there would never equal the SHA-256 index entry it
    // is supposed to match — the exact stage would silently degrade to
    // inexact and read objects it did not need. Carry it across.
    let hash_kind = git_internal::hash::get_hash_kind();
    let job: IoJob = Box::new(move || {
        git_internal::hash::set_hash_kind(hash_kind);
        // A closed receiver (timed-out caller) is expected; drop the value,
        // then release the slot so the pool accounting stays accurate.
        let _ = tx.send(op());
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

/// Like [`with_io_deadline`], but the deadline measures LACK OF PROGRESS
/// rather than total duration.
///
/// `progress` is a counter the operation bumps as it does useful work. The
/// caller waits in slices and only gives up once a whole slice passes with
/// the counter unchanged — so a genuinely large directory that keeps
/// yielding entries is never mistaken for a hung mount, while a mount that
/// truly stops answering is still reclaimed within one timeout window.
pub(crate) fn with_no_progress_deadline<T, F>(
    progress: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    op: F,
) -> Result<T, ()>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    use std::sync::atomic::Ordering;

    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let submitted = with_io_deadline_detached(move || {
        let _ = tx.send(op());
    });
    if submitted.is_err() {
        return Err(());
    }
    let window = io_op_timeout();
    // Poll in slices so progress can extend the budget; the slice is small
    // relative to the window but never so small that we spin.
    let slice = window / 10;
    let slice = if slice.is_zero() {
        std::time::Duration::from_millis(1)
    } else {
        slice
    };
    let mut idle = std::time::Duration::ZERO;
    let mut seen = progress.load(Ordering::SeqCst);
    loop {
        match rx.recv_timeout(slice) {
            Ok(value) => return Ok(value),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Err(()),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let now = progress.load(Ordering::SeqCst);
                if now != seen {
                    seen = now;
                    idle = std::time::Duration::ZERO;
                } else {
                    idle += slice;
                    if idle >= window {
                        return Err(());
                    }
                }
            }
        }
    }
}

/// Submit a job to the pooled workers without waiting for it. Shares the
/// same bounded slot accounting as [`with_io_deadline`].
fn with_io_deadline_detached<F>(op: F) -> Result<(), ()>
where
    F: FnOnce() + Send + 'static,
{
    use std::sync::atomic::Ordering;

    let pool = IO_POOL.get_or_init(|| {
        std::sync::Arc::new(IoWorkerPool {
            queue: std::sync::Mutex::new(std::collections::VecDeque::new()),
            ready: std::sync::Condvar::new(),
            handles: std::sync::Mutex::new(Vec::new()),
        })
    });
    if IO_BUSY
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
            (current < MAX_INFLIGHT_IO_WORKERS).then_some(current + 1)
        })
        .is_err()
    {
        return Err(());
    }
    let needed = IO_BUSY.load(Ordering::SeqCst);
    if IO_WORKERS
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |workers| {
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
    let hash_kind = git_internal::hash::get_hash_kind();
    let job: IoJob = Box::new(move || {
        git_internal::hash::set_hash_kind(hash_kind);
        op();
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
    Ok(())
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
        job();
    }
}

#[cfg(test)]
mod pool_smoke_tests {
    #[test]
    fn io_deadline_pool_runs_a_trivial_job() {
        let got = super::with_io_deadline(|| 42_u32);
        assert_eq!(got, Ok(42), "the pooled worker never ran the job");
    }

    #[tokio::test]
    async fn io_deadline_pool_runs_a_trivial_job_inside_tokio() {
        let got = super::with_io_deadline(|| 7_u32);
        assert_eq!(
            got,
            Ok(7),
            "the pooled worker never ran the job under tokio"
        );
    }
}

/// One blocked path (workdir-relative) with its reason.
#[derive(Debug, Clone)]
pub(crate) struct IoBlockedEvent {
    pub(crate) path: PathBuf,
    /// Reason taxonomy (§B.6.0.1), consumed by the io_blocked[] JSON
    /// contract (R0-8) and its worktree-family warning mapping.
    pub(crate) reason: IoBlockedReason,
    /// The scan ABSORBED this block into the listing by over-reporting
    /// conservatively: the affected path still appears in the output, so the
    /// omission is compensated in the listing sense only.
    ///
    /// The only case today is an unreadable top-level untracked directory:
    /// we cannot prove it holds a visible file, so we emit its `?? dir/`
    /// marker anyway (a marker is not an inspection result, and listing a
    /// directory a readable scan would have hidden is the safe direction).
    /// §B.6.0.1 still fails TEXT modes closed for every blocked event,
    /// absorbed or not — while `--json` keeps the partial contract: the
    /// event is listed in `io_blocked[]`, `base_scan_complete` stays false,
    /// and the dirty cache is not rewritten from a guess.
    pub(crate) absorbed: bool,
}

/// Which budget tripped first when `truncated` is set (JSON warning must
/// attribute the cause, §B.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeBudgetKind {
    Enumeration,
    Destination,
}

/// One directory's bounded listing, produced INSIDE the deadline worker so
/// both the blocking reads and the enumeration budget are enforced in the
/// same place (§B.3.2).
struct DirListing {
    entries: Vec<std::ffi::OsString>,
    errors: Vec<io::Error>,
    /// Entries actually taken from the iterator — the amount to charge
    /// against the cross-root enumeration budget, including ones dropped
    /// afterwards.
    taken: usize,
    /// The budget tripped mid-directory; the listing is partial.
    hit_cap: bool,
}

/// Composite probe outcome across every root (§B.3.2 归并).
#[derive(Debug, Default)]
pub(crate) struct ProbeOutcome {
    /// Qualified destinations (workdir-relative), stably sorted.
    pub(crate) destinations: Vec<PathBuf>,
    /// §B.3.2 per-root三态: roots the probe walked to completion (no
    /// budget trip, no blocked path underneath). A marker may only
    /// collapse when ITS OWN root is complete — an unrelated root's
    /// truncation must not freeze it.
    pub(crate) complete_roots: Vec<PathBuf>,
    /// A budget tripped; partial destinations are still pairable.
    pub(crate) truncated: Option<ProbeBudgetKind>,
    /// Every I/O failure encountered (path-sorted, deduplicated).
    pub(crate) io_blocked: Vec<IoBlockedEvent>,
    /// Candidates skipped because their name is not valid UTF-8 (R0 scope:
    /// they keep their base `??` behavior but never join rename scoring —
    /// surfaced as one `rename_path_encoding_unsupported` warning).
    pub(crate) encoding_skipped: u64,
}

/// Derive the probe roots from the positive pathspecs (§B.3.1.1) —
/// workdir-relative, `""` meaning the repository root. Case-folded or
/// otherwise unnarrowable specs conservatively fall back to the root;
/// redundant nested roots are removed.
pub(crate) fn pathspec_probe_roots(pathspecs: Option<&PathspecSet>) -> Vec<PathBuf> {
    let Some(set) = pathspecs else {
        return vec![PathBuf::new()];
    };
    if set.is_empty() || !set.has_positive() {
        // No specs, or exclude-only (positive set ≡ whole repository).
        return vec![PathBuf::new()];
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    for root in set.positive_depth_roots() {
        if root.icase() {
            // Cannot narrow safely on a case-sensitive FS: probe everything.
            return vec![PathBuf::new()];
        }
        roots.push(root.path().to_path_buf());
    }
    roots.sort();
    roots.dedup();
    if roots.iter().any(|r| r.as_os_str().is_empty()) {
        return vec![PathBuf::new()];
    }
    // Drop roots covered by an ancestor root (`a/` covers `a/b/`).
    let mut kept: Vec<PathBuf> = Vec::new();
    for root in roots {
        if !kept.iter().any(|prev| root.starts_with(prev)) {
            kept.push(root);
        }
    }
    kept
}

/// Inputs for destination qualification (§B.3.1.2): the same tracked/ignore
/// layering as the display scan.
pub(crate) struct DestinationFilter<'a> {
    pub(crate) workdir: &'a Path,
    pub(crate) index: &'a Index,
    pub(crate) tracked: &'a TrackedPaths,
    pub(crate) pathspecs: Option<&'a PathspecSet>,
}

impl DestinationFilter<'_> {
    /// Whether a path is even eligible to become a destination, judged
    /// without reading it: inside the pathspec and not ignored. Used to
    /// decide whether an I/O error on that path is relevant to this run
    /// (§B.3.2) — errors on irrelevant paths are dropped, never surfaced.
    fn could_qualify(&self, relative: &Path, absolute: &Path) -> bool {
        if let Some(set) = self.pathspecs
            && !set.matches_path(relative)
            && !self.could_contain_match(relative)
        {
            return false;
        }
        // A reclaimed lookup means "cannot decide": treat the path as
        // potentially relevant so its block is reported rather than dropped.
        !util::check_gitignore_bounded(self.workdir, absolute).unwrap_or(false)
    }

    /// Whether a DIRECTORY might still hold a matching path even though the
    /// directory itself does not match. `:(glob)wanted/*.txt` never matches
    /// `wanted`, so judging relevance by the directory alone would discard
    /// an EACCES on the one directory the caller asked about — silently
    /// reporting "no renames here" for a subtree that could not be read.
    fn could_contain_match(&self, relative: &Path) -> bool {
        let Some(set) = self.pathspecs else {
            return true;
        };
        if set.matches_path(relative) {
            return true;
        }
        // If an ANCESTOR matches, descendants of this directory can match
        // (`:(glob)wanted/*.txt` does not match `wanted`, but files under it
        // do).
        let mut probe = relative.to_path_buf();
        while probe.pop() {
            if set.matches_path(&probe) {
                return true;
            }
        }
        // Unnarrowable specs (`:(icase)`) fall back to the repository root,
        // which sweeps in directories that have nothing to do with the
        // request. Returning `true` for those made an unrelated EACCES fail
        // a narrowed status closed. Relevance is judged against the DERIVED
        // probe roots instead: a path under a non-root scope is out of
        // scope, while a path ABOVE a root still is (its subtree holds one).
        pathspec_probe_roots(Some(set)).iter().any(|root| {
            root.as_os_str().is_empty() || relative.starts_with(root) || root.starts_with(relative)
        })
    }

    /// §B.3.2: a directory the index records as a gitlink (mode `0o160000`)
    /// is a submodule checkout and is never traversed. The marker-file test
    /// alone is not enough — a submodule that has been registered but not
    /// yet initialized has no `.git`/`.libra` inside it, so without this the
    /// probe would walk the whole nested tree and charge it to the budget.
    fn is_gitlink_dir(&self, relative: &Path) -> bool {
        relative.to_str().is_some_and(|name| {
            self.index
                .get(name, 0)
                .is_some_and(|entry| entry.mode == 0o160000)
        })
    }

    /// §B.3.1.2: keep a probed path only when it matches the pathspec set,
    /// is not tracked (including a case-fold alias), not unmerged, and not
    /// ignored.
    fn qualifies(&self, relative: &Path, absolute: &Path, encoding_skipped: &mut u64) -> bool {
        if let Some(set) = self.pathspecs
            && !set.matches_path(relative)
        {
            return false;
        }
        let Some(rel_str) = relative.to_str() else {
            // Non-UTF-8 paths stay out of rename candidacy in R0 (DEFER-02);
            // base D/A/`??` behavior is unaffected (§B.6.1) and the skip is
            // counted for one deduplicated
            // `rename_path_encoding_unsupported` warning.
            *encoding_skipped += 1;
            return false;
        };
        if self.index.tracked(rel_str, 0)
            || (1..=3).any(|stage| self.index.get(rel_str, stage).is_some())
        {
            return false;
        }
        if self.tracked.same_file_case_alias(self.workdir, relative) {
            return false;
        }
        if util::check_gitignore_bounded(self.workdir, absolute).unwrap_or(true) {
            return false;
        }
        true
    }
}

/// Tri-state repository-marker probe for a candidate directory.
enum MarkerProbe {
    Present,
    Absent,
    Blocked(IoBlockedReason),
}

/// Whether `dir` holds a repository marker (`.libra` / `.git`), under the
/// §B.3.3 deadline and WITHOUT collapsing errors into "absent" — an
/// unreadable marker must exclude the directory and be reported, never wave
/// a foreign repository through.
fn marker_probe(dir: &Path) -> MarkerProbe {
    let target = dir.to_path_buf();
    let probe = with_io_deadline(move || {
        for marker in [util::ROOT_DIR, util::GIT_DIR] {
            match target.join(marker).symlink_metadata() {
                Ok(_) => return Ok(true),
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(false)
    });
    match probe {
        Ok(Ok(true)) => MarkerProbe::Present,
        Ok(Ok(false)) => MarkerProbe::Absent,
        Ok(Err(error)) => MarkerProbe::Blocked(IoBlockedReason::from_error(&error)),
        Err(()) => MarkerProbe::Blocked(IoBlockedReason::IoTimeout),
    }
}

/// Whether `candidate` really lives inside `workdir` once symlinks are
/// resolved. `symlink_metadata` does not follow the FINAL component but does
/// follow every intermediate one, so a lexical comparison of the unresolved
/// path would wave `link/child` through when `link` points outside.
///
/// An unresolvable path falls back to the lexical check rather than silently
/// trusting it. (Closes the STATIC case; the remaining check→open race is
/// the fd/beneath work deferred to plan-20260715 W-IO.)
fn resolves_inside(candidate: &Path, workdir: &Path) -> bool {
    let canon_candidate = candidate.to_path_buf();
    let canon_root = workdir.to_path_buf();
    match with_io_deadline(move || (canon_candidate.canonicalize(), canon_root.canonicalize())) {
        Ok((Ok(resolved), Ok(root))) => resolved.starts_with(&root),
        Ok(_) => util::is_sub_path(candidate, workdir),
        // A reclaimed resolution cannot vouch for containment.
        Err(()) => false,
    }
}

/// Whether any component of a workdir-relative path is a repository
/// metadata directory (`.libra`, or `.git` for imported trees).
fn contains_metadata_component(relative: &Path) -> bool {
    relative.components().any(|component| {
        let name = component.as_os_str();
        name == std::ffi::OsStr::new(util::ROOT_DIR) || name == std::ffi::OsStr::new(util::GIT_DIR)
    })
}

/// Walk the probe roots with the shared dual budget (§B.3.2). Never follows
/// symlinks (they are leaf candidates), never enters `.libra`/`.git` or a
/// nested repository, records EACCES/I-O as events while continuing
/// siblings, and treats a `NotFound` entry as a TOCTOU no-op.
pub(crate) fn probe_rename_destinations(
    roots: &[PathBuf],
    filter: &DestinationFilter<'_>,
    limits: ProbeLimits,
) -> ProbeOutcome {
    let mut outcome = ProbeOutcome::default();
    let mut enumerated = 0usize;
    let workdir = filter.workdir;

    'roots: for root in roots {
        let blocked_before = outcome.io_blocked.len();
        let truncated_before = outcome.truncated.is_some();
        let root_abs = workdir.join(root);
        // Containment is checked BEFORE anything else touches the root —
        // including the metadata/gitlink exclusions below. An intermediate
        // symlink can point a root like `link/.git` at a directory outside
        // the worktree; if that outside directory happens to hold a
        // `.git`/`.libra` marker the metadata check would "exclude a nested
        // repository" and move on silently, never recording the escape.
        if !root.as_os_str().is_empty() && !resolves_inside(&root_abs, workdir) {
            outcome.io_blocked.push(IoBlockedEvent {
                path: root.clone(),
                reason: IoBlockedReason::IoError,
                absorbed: false,
            });
            continue;
        }
        // §B.3.2: repository metadata is excluded UNCONDITIONALLY, including
        // when a pathspec names it directly (`libra status -- .libra`). The
        // per-entry child filter below only sees children, so without this a
        // metadata root would be walked and would consume the global budget.
        if contains_metadata_component(root) {
            continue;
        }
        // The same exclusions the per-entry filter applies to CHILD
        // directories must apply to a directly selected root: without this,
        // `status -- nested-repo` (or a gitlink path) walks straight into a
        // foreign repository because the child filter never sees the root.
        if !root.as_os_str().is_empty() && filter.is_gitlink_dir(root) {
            continue;
        }
        let root_stat_target = root_abs.clone();
        let root_stat = match with_io_deadline(move || root_stat_target.symlink_metadata()) {
            Ok(result) => result,
            Err(()) => {
                outcome.io_blocked.push(IoBlockedEvent {
                    path: root.clone(),
                    reason: IoBlockedReason::IoTimeout,
                    absorbed: false,
                });
                continue;
            }
        };
        match root_stat {
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue, // gone
            Err(error) => {
                outcome.io_blocked.push(IoBlockedEvent {
                    path: root.clone(),
                    reason: IoBlockedReason::from_error(&error),
                    absorbed: false,
                });
                continue;
            }
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                // A file/symlink root is itself a candidate.
                enumerated += 1;
                if enumerated > limits.max_enumerated_entries {
                    outcome.truncated = Some(ProbeBudgetKind::Enumeration);
                    break 'roots;
                }
                if filter.qualifies(root, &root_abs, &mut outcome.encoding_skipped) {
                    if outcome.destinations.len() >= limits.max_qualified_destinations {
                        outcome.truncated = Some(ProbeBudgetKind::Destination);
                        break 'roots;
                    }
                    outcome.destinations.push(root.clone());
                }
                continue;
            }
            Ok(_) => {}
        }

        // Only now is the root known to be a real directory (the arm above
        // returns early for files and symlinks — a symlink root is a LEAF
        // candidate and must never be dereferenced). Probing markers before
        // this point turned `status -- dest.txt` into `ENOTDIR` → blocked,
        // and dereferenced a directory symlink.
        if !root.as_os_str().is_empty() {
            match marker_probe(&root_abs) {
                MarkerProbe::Present => continue,
                MarkerProbe::Blocked(reason) => {
                    outcome.io_blocked.push(IoBlockedEvent {
                        path: root.clone(),
                        reason,
                        absorbed: false,
                    });
                    continue;
                }
                MarkerProbe::Absent => {}
            }
        }

        let mut pending: Vec<PathBuf> = vec![root.clone()];
        while let Some(dir) = pending.pop() {
            let dir_abs = workdir.join(&dir);
            // §B.3.2 escape guard: an ancestor replaced by a symlink after
            // the parent walk must never let the probe read outside the
            // worktree. Re-verify the directory is a real directory
            // (not a symlink) and still lexically inside the worktree.
            let dir_stat_target = dir_abs.clone();
            let dir_stat = match with_io_deadline(move || dir_stat_target.symlink_metadata()) {
                Ok(result) => result,
                Err(()) => {
                    outcome.io_blocked.push(IoBlockedEvent {
                        path: dir.clone(),
                        reason: IoBlockedReason::IoTimeout,
                        absorbed: false,
                    });
                    continue;
                }
            };
            match dir_stat {
                Ok(meta) if meta.file_type().is_symlink() => continue,
                Ok(meta) if !meta.is_dir() => continue,
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    outcome.io_blocked.push(IoBlockedEvent {
                        path: dir.clone(),
                        reason: IoBlockedReason::from_error(&error),
                        absorbed: false,
                    });
                    continue;
                }
            }
            // Resolve BOTH sides before comparing: `symlink_metadata` above
            // does not follow the FINAL component, but every intermediate
            // one is followed, so a root like `link/inside` can name a
            // directory that physically lives outside the worktree. A
            // lexical comparison of the unresolved path would wave it
            // through. (Resolution closes the static case; the remaining
            // check→open race is the fd/beneath work deferred to W-IO.)
            if !resolves_inside(&dir_abs, workdir) {
                // A path that resolves outside the worktree is REPORTED,
                // never silently skipped (§B.3.2): silently dropping it
                // would let a redirected subtree read as "no candidates
                // here", which is indistinguishable from an empty
                // directory to every consumer.
                outcome.io_blocked.push(IoBlockedEvent {
                    path: dir.clone(),
                    reason: IoBlockedReason::IoError,
                    absorbed: false,
                });
                continue;
            }
            // Bounded enumeration UNDER the deadline (§B.3.2): the budget is
            // charged inside the worker as entries are taken, so the listing
            // stops the moment the cap trips. Reading the whole directory
            // first and counting afterwards would make the 50k cap advisory
            // — a directory with millions of entries would still be read in
            // full, in unbounded time and memory, before anyone noticed.
            let read_target = dir_abs.clone();
            let remaining = limits.max_enumerated_entries.saturating_sub(enumerated);
            // No-progress deadline: a wide directory that keeps yielding
            // entries is not a hung mount (see the main scan for the same
            // reasoning).
            let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let ticker = std::sync::Arc::clone(&progress);
            let listing = match with_no_progress_deadline(
                std::sync::Arc::clone(&progress),
                move || -> io::Result<DirListing> {
                    let reader = std::fs::read_dir(&read_target)?;
                    let mut entries = Vec::new();
                    let mut errors = Vec::new();
                    let mut taken = 0usize;
                    let mut hit_cap = false;
                    for entry in reader {
                        taken += 1;
                        ticker.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if taken > remaining {
                            // The entry that PROVES truncation cost a real
                            // readdir, so it is charged too — but never
                            // processed. Reading it PAST the cap check (the
                            // `for` loop used to pull it uncharged) is how
                            // a wide directory overran the budget by one
                            // entry per directory.
                            hit_cap = true;
                            break;
                        }
                        match entry {
                            Ok(entry) => entries.push(entry.file_name()),
                            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                            Err(error) => errors.push(error),
                        }
                    }
                    Ok(DirListing {
                        entries,
                        errors,
                        taken,
                        hit_cap,
                    })
                },
            ) {
                Err(()) => {
                    // Only a directory that could hold a QUALIFYING candidate
                    // is this run's problem (§B.3.2). A repo-root fallback
                    // (`:(icase)wanted`) sweeps in unrelated subtrees; an
                    // EACCES there must not mark the probe incomplete and
                    // strand another directory's `? dir/` marker, especially
                    // since pathspec filtering drops the event downstream and
                    // cannot undo the completeness decision.
                    if filter.could_qualify(&dir, &dir_abs) {
                        outcome.io_blocked.push(IoBlockedEvent {
                            path: dir.clone(),
                            reason: IoBlockedReason::IoTimeout,
                            absorbed: false,
                        });
                    }
                    continue;
                }
                Ok(Ok(listing)) => listing,
                Ok(Err(error)) if error.kind() == io::ErrorKind::NotFound => continue,
                Ok(Err(error)) => {
                    if filter.could_qualify(&dir, &dir_abs) {
                        outcome.io_blocked.push(IoBlockedEvent {
                            path: dir.clone(),
                            reason: IoBlockedReason::from_error(&error),
                            absorbed: false,
                        });
                    }
                    continue;
                }
            };
            enumerated += listing.taken;
            for error in &listing.errors {
                outcome.io_blocked.push(IoBlockedEvent {
                    path: dir.clone(),
                    reason: IoBlockedReason::from_error(error),
                    absorbed: false,
                });
            }
            if listing.hit_cap {
                outcome.truncated = Some(ProbeBudgetKind::Enumeration);
            }
            let mut names: Vec<PathBuf> = listing
                .entries
                .into_iter()
                .map(|name| dir.join(name))
                .collect();
            // Fully-enumerated directories process in byte order for
            // deterministic recursion/qualification.
            names.sort();
            for relative in names {
                let absolute = workdir.join(&relative);
                let name = relative.file_name().unwrap_or_default();
                if name == std::ffi::OsStr::new(util::ROOT_DIR)
                    || name == std::ffi::OsStr::new(util::GIT_DIR)
                {
                    continue;
                }
                // Per-entry stat under the deadline as well: a hung mount
                // blocks on the entry, not only on the directory open.
                let entry_target = absolute.clone();
                let entry_stat = match with_io_deadline(move || entry_target.symlink_metadata()) {
                    Ok(result) => result,
                    Err(()) => {
                        if filter.could_qualify(&relative, &absolute) {
                            outcome.io_blocked.push(IoBlockedEvent {
                                path: relative.clone(),
                                reason: IoBlockedReason::IoTimeout,
                                absorbed: false,
                            });
                        }
                        continue;
                    }
                };
                let meta = match entry_stat {
                    Ok(meta) => meta,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        // §B.3.2: an error on an entry that could never be
                        // a destination (outside the pathspec, or an
                        // ignored path) is NOT this run's problem — it must
                        // not fail a narrowed status closed nor leak the
                        // path into `io_blocked[]`.
                        if filter.could_qualify(&relative, &absolute) {
                            outcome.io_blocked.push(IoBlockedEvent {
                                path: relative.clone(),
                                reason: IoBlockedReason::from_error(&error),
                                absorbed: false,
                            });
                        }
                        continue;
                    }
                };
                if meta.file_type().is_symlink() || meta.is_file() {
                    if filter.qualifies(&relative, &absolute, &mut outcome.encoding_skipped) {
                        if outcome.destinations.len() >= limits.max_qualified_destinations {
                            outcome.truncated = Some(ProbeBudgetKind::Destination);
                            break;
                        }
                        outcome.destinations.push(relative);
                    }
                } else if meta.is_dir() {
                    // Prune ignored subtrees before enumerating them and
                    // never enter a nested repository or gitlink checkout.
                    // The ignore lookup and the two marker probes all touch
                    // the filesystem, so they run under the same deadline as
                    // every other traversal read. A reclaimed probe is
                    // treated as "do not descend": conservative, and it
                    // cannot wedge the walk.
                    // `core.excludesFile` is a DB read, not a filesystem
                    // one; resolving it here keeps database contention off
                    // the worktree I/O deadline.
                    util::prewarm_ignore_config(workdir);
                    let ignore_target = (workdir.to_path_buf(), absolute.clone());
                    let ignored = match with_io_deadline(move || {
                        let (workdir, absolute) = ignore_target;
                        util::check_gitignore(&workdir, &absolute)
                    }) {
                        Ok(value) => value,
                        Err(()) => {
                            // "Could not decide" is NOT "ignored": skipping
                            // the subtree silently would degrade to "no
                            // renames here", which the contract forbids.
                            // Report it and skip.
                            if filter.could_qualify(&relative, &absolute) {
                                outcome.io_blocked.push(IoBlockedEvent {
                                    path: relative.clone(),
                                    reason: IoBlockedReason::IoTimeout,
                                    absorbed: false,
                                });
                            }
                            true
                        }
                    };
                    let marker = marker_probe(&absolute);
                    if let MarkerProbe::Blocked(reason) = marker
                        && filter.could_qualify(&relative, &absolute)
                    {
                        outcome.io_blocked.push(IoBlockedEvent {
                            path: relative.clone(),
                            reason,
                            absorbed: false,
                        });
                    }
                    if ignored
                        || !matches!(marker, MarkerProbe::Absent)
                        || filter.is_gitlink_dir(&relative)
                    {
                        continue;
                    }
                    pending.push(relative);
                }
            }
            if outcome.truncated.is_some() {
                break 'roots;
            }
        }
        if !truncated_before
            && outcome.truncated.is_none()
            && outcome.io_blocked.len() == blocked_before
        {
            outcome.complete_roots.push(root.clone());
        }
    }

    outcome.destinations.sort();
    outcome.destinations.dedup();
    outcome.io_blocked.sort_by(|a, b| a.path.cmp(&b.path));
    outcome.io_blocked.dedup_by(|a, b| a.path == b.path);
    outcome
}

/// §B.3.5: collapse `? dir/` markers consumed by rename pairing (display
/// base). A marker is removed only when the probe was COMPLETE (no
/// truncation, no I/O blocks) and no qualified-but-unconsumed candidate
/// remains under the directory; incomplete probes conservatively keep every
/// marker.
pub(crate) fn collapse_untracked_markers(
    unstaged_new: &mut Vec<PathBuf>,
    destinations: &[PathBuf],
    consumed: &HashSet<PathBuf>,
    complete_roots: &[PathBuf],
) {
    if complete_roots.is_empty() {
        return;
    }
    // A path is collapsible only when the root that governs it was walked
    // to completion (§B.3.2 per-root Complete/Truncated/IoBlocked).
    let under_complete_root = |path: &Path| -> bool {
        complete_roots
            .iter()
            .any(|root| root.as_os_str().is_empty() || path.starts_with(root))
    };
    unstaged_new.retain(|entry| {
        if !under_complete_root(Path::new(entry.to_string_lossy().trim_end_matches('/'))) {
            return true;
        }
        let text = entry.to_string_lossy();
        if !text.ends_with('/') {
            // Plain untracked row consumed as a destination: drop it (the
            // rename record is its signal now).
            return !consumed.contains(entry);
        }
        let dir = PathBuf::from(text.trim_end_matches('/'));
        let mut saw_candidate = false;
        for destination in destinations {
            if destination.starts_with(&dir) {
                saw_candidate = true;
                if !consumed.contains(destination) {
                    return true; // unconsumed candidate remains → keep marker
                }
            }
        }
        // Every candidate under the dir was consumed → remove the marker; a
        // dir the probe never saw keeps its marker conservatively.
        !saw_candidate
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The debug-only env overrides must be honored only under the test
    /// harness: with the variables set but `LIBRA_TEST` absent, production
    /// defaults stay in effect; with the gate present, the overrides bite.
    #[test]
    #[serial_test::serial]
    fn seam_timeouts_and_probe_limits_require_the_harness_gate() {
        // SAFETY: serialized test body; every variable is removed again
        // before the test returns.
        unsafe {
            std::env::set_var("LIBRA_TEST_STATUS_IO_TIMEOUT_MS", "1");
            std::env::set_var("LIBRA_TEST_STATUS_PROBE_ENUM_BUDGET", "1");
            std::env::set_var("LIBRA_TEST_STATUS_PROBE_DEST_BUDGET", "1");
            std::env::remove_var(crate::utils::pager::LIBRA_TEST_ENV);

            assert_eq!(
                io_op_timeout(),
                std::time::Duration::from_secs(10),
                "without LIBRA_TEST the timeout override must be ignored"
            );
            let limits = ProbeLimits::effective();
            assert_eq!(
                limits.max_enumerated_entries, PROBE_MAX_ENUMERATED_ENTRIES,
                "without LIBRA_TEST the enum-budget override must be ignored"
            );
            assert_eq!(
                limits.max_qualified_destinations, PROBE_MAX_QUALIFIED_DESTINATIONS,
                "without LIBRA_TEST the dest-budget override must be ignored"
            );

            std::env::set_var(crate::utils::pager::LIBRA_TEST_ENV, "1");
            assert_eq!(
                io_op_timeout(),
                std::time::Duration::from_millis(1),
                "with the gate the timeout override applies"
            );
            let limits = ProbeLimits::effective();
            assert_eq!(limits.max_enumerated_entries, 1);
            assert_eq!(limits.max_qualified_destinations, 1);

            std::env::remove_var("LIBRA_TEST_STATUS_IO_TIMEOUT_MS");
            std::env::remove_var("LIBRA_TEST_STATUS_PROBE_ENUM_BUDGET");
            std::env::remove_var("LIBRA_TEST_STATUS_PROBE_DEST_BUDGET");
            std::env::remove_var(crate::utils::pager::LIBRA_TEST_ENV);
        }
    }
}
