//! A0-04: cross-process run-level admission / queue control for
//! `libra review` and `libra investigate`.
//!
//! Each `review` / `investigate` invocation is a **separate process**, so an
//! in-process `Semaphore` (the per-run `max_reviewers_per_run` limiter) cannot
//! bound how many *runs* execute concurrently across a repository. This module
//! provides a small filesystem-backed bounded semaphore shared by both run
//! kinds, rooted under the same `.libra/sessions/agent-runs/` tree the run
//! stores use:
//!
//! ```text
//! agent-runs/.admission/
//!   lock          flock-serialized admission critical section
//!   slots/<t>     one file per *executing* run (an occupied slot)
//!   queue/<t>     one file per *waiting* run (a queued reservation)
//! ```
//!
//! A run acquires a **slot ticket** ([`RunSlot`]) for its whole lifetime; the
//! ticket file is removed on `Drop` (normal completion, cancel, failure, or
//! process death via stale reclaim), so a crashed run never permanently holds
//! a slot. Admission decisions ([`decide`]) are:
//!
//! - `active < max_concurrent_runs` → **Admit** (take a slot immediately);
//! - `active == max` and `queued < cap` → **Queue** (park a reservation and
//!   block-poll until a slot frees);
//! - `queued == cap` → **Reject** (fail closed).
//!
//! `.admission` starts with a dot and is not a valid run id, so the run stores'
//! `list_runs()` scans skip it.
//!
//! **Platform**: enforcement is Unix-only, matching the run stores' `flock`
//! `RunLock`. Without a cross-process advisory lock a filesystem semaphore
//! cannot be made correct, so on non-Unix targets admission is a documented
//! no-op — every run is admitted (unlimited, as before A0-04) rather than
//! shipping a racy, un-reclaimable half-semaphore.

use std::{
    io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result};

use crate::internal::config::ConfigKv;

/// Config key bounding concurrent review/investigate runs (`agent.md` §12).
pub const MAX_CONCURRENT_RUNS_KEY: &str = "agent.max_concurrent_runs";
/// Default when unset — matches the documented `agent.max_concurrent_runs=2`.
pub const DEFAULT_MAX_CONCURRENT_RUNS: usize = 2;
/// Maximum queued (waiting) runs before admission fails closed (`agent.md`
/// §12 "队列上限 10"). Fixed, not configurable, so a mis-set config can never
/// unbound the queue.
pub const RUN_QUEUE_CAP: usize = 10;

const ADMISSION_DIR: &str = ".admission";
const SLOTS_DIR: &str = "slots";
const QUEUE_DIR: &str = "queue";
const LOCK_FILE: &str = "lock";

/// Poll interval while a queued run waits for a slot to free.
const QUEUE_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// The pure admission decision — the single source of truth for the three
/// outcomes, kept side-effect free so it is exhaustively unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionDecision {
    Admit,
    Queue,
    Reject,
}

/// Decide admission from the observed occupancy. `active` is the number of
/// executing runs (occupied slots); `queued` the number of waiting runs.
///
/// FIFO fairness: a fresh arrival is admitted only when a slot is free AND no
/// one is already waiting (`queued == 0`); otherwise it joins the back of the
/// queue. Without the `queued == 0` guard a steady stream of new runs could
/// grab freed slots in the 200 ms gap before an older waiter's next poll,
/// starving the queue (Codex A0-04 review). `max` is clamped to ≥ 1 so a
/// mis-set `0` never wedges every run.
pub fn decide(active: usize, queued: usize, max: usize, cap: usize) -> AdmissionDecision {
    if active < max.max(1) && queued == 0 {
        AdmissionDecision::Admit
    } else if queued < cap {
        AdmissionDecision::Queue
    } else {
        AdmissionDecision::Reject
    }
}

/// Resolve `agent.max_concurrent_runs` (default 2, must be > 0).
pub async fn max_concurrent_runs() -> Result<usize> {
    let raw = ConfigKv::get(MAX_CONCURRENT_RUNS_KEY)
        .await
        .with_context(|| format!("read config '{MAX_CONCURRENT_RUNS_KEY}'"))?
        .map(|entry| entry.value);
    let Some(value) = raw else {
        return Ok(DEFAULT_MAX_CONCURRENT_RUNS);
    };
    let trimmed = value.trim();
    let parsed: usize = trimmed.parse().map_err(|_| {
        anyhow::anyhow!(
            "config '{MAX_CONCURRENT_RUNS_KEY}' must be a positive integer, found {trimmed:?}"
        )
    })?;
    if parsed == 0 {
        anyhow::bail!("config '{MAX_CONCURRENT_RUNS_KEY}' must be greater than 0");
    }
    Ok(parsed)
}

/// The outcome of a single non-blocking admission attempt ([`try_admit`]).
#[derive(Debug)]
pub enum AdmissionOutcome {
    /// A slot was taken; hold [`RunSlot`] for the run's lifetime.
    Admitted(RunSlot),
    /// All slots busy but the queue had room; a reservation was parked.
    /// Call [`QueueTicket::wait_for_slot`] to block until promoted.
    Queued(QueueTicket),
    /// Queue full — fail closed.
    Rejected {
        active: usize,
        queued: usize,
        cap: usize,
    },
}

/// RAII occupied-slot ticket. Its file exists for as long as the run executes;
/// dropping it (completion, cancel, failure, panic) frees the slot.
#[derive(Debug)]
pub struct RunSlot {
    path: PathBuf,
}

impl Drop for RunSlot {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A parked queue reservation. Blocks in [`wait_for_slot`] until a slot frees
/// and this ticket is the oldest waiter, then converts to a [`RunSlot`].
/// Dropping it without promotion removes the reservation (e.g. Ctrl-C on a
/// queued foreground run advances the queue for others).
#[derive(Debug)]
pub struct QueueTicket {
    runs_root: PathBuf,
    path: PathBuf,
    promoted: bool,
    /// Max concurrency captured at admission time, so promotion (a sync
    /// section under the flock) needn't re-read the async config.
    cached_max: usize,
}

impl Drop for QueueTicket {
    fn drop(&mut self) {
        if !self.promoted {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl QueueTicket {
    /// Block (polling) until a slot frees and this ticket is the oldest queued
    /// reservation, then atomically convert to an occupied [`RunSlot`].
    /// `timeout` bounds the wait; `None` waits indefinitely.
    pub async fn wait_for_slot(mut self, timeout: Option<Duration>) -> Result<RunSlot> {
        let deadline = timeout.map(|d| SystemTime::now() + d);
        loop {
            if let Some(slot) = self.try_promote()? {
                self.promoted = true;
                return Ok(slot);
            }
            if let Some(deadline) = deadline
                && SystemTime::now() >= deadline
            {
                anyhow::bail!(
                    "timed out waiting for a run slot after {:?}; too many concurrent \
                     review/investigate runs are active",
                    timeout.unwrap_or_default()
                );
            }
            tokio::time::sleep(QUEUE_POLL_INTERVAL).await;
        }
    }

    /// One promotion attempt under the admission lock: if a slot is free and
    /// this ticket is the oldest queued reservation, move it into `slots/`.
    fn try_promote(&self) -> Result<Option<RunSlot>> {
        let dirs = AdmissionDirs::at(&self.runs_root);
        let _lock = AdmissionLock::acquire(&dirs)?;
        reclaim_stale(&dirs.slots)?;
        reclaim_stale(&dirs.queue)?;
        let max = self.cached_max;
        let active = count_tickets(&dirs.slots)?;
        // Oldest-first fairness: only the earliest-created queue ticket may
        // promote, so waiters don't livelock or overtake each other.
        let oldest = oldest_ticket(&dirs.queue)?;
        let mine = self
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string);
        if active < max && oldest.is_some() && oldest == mine {
            let file_name = self
                .path
                .file_name()
                // INVARIANT: `self.path` is a ticket file this module created
                // under `queue/` via `create_ticket`, so it always has a name.
                .ok_or_else(|| anyhow::anyhow!("queue ticket path has no file name"))?;
            let slot_path = dirs.slots.join(file_name);
            std::fs::rename(&self.path, &slot_path)
                .with_context(|| format!("promote queue ticket to slot at {slot_path:?}"))?;
            return Ok(Some(RunSlot { path: slot_path }));
        }
        Ok(None)
    }
}

/// Resolved admission directory paths under a run store's `agent-runs/` root.
struct AdmissionDirs {
    root: PathBuf,
    slots: PathBuf,
    queue: PathBuf,
    lock: PathBuf,
}

impl AdmissionDirs {
    fn at(runs_root: &Path) -> Self {
        let root = runs_root.join(ADMISSION_DIR);
        Self {
            slots: root.join(SLOTS_DIR),
            queue: root.join(QUEUE_DIR),
            lock: root.join(LOCK_FILE),
            root,
        }
    }

    fn ensure(&self) -> io::Result<()> {
        std::fs::create_dir_all(&self.slots)?;
        std::fs::create_dir_all(&self.queue)?;
        Ok(())
    }
}

/// Attempt admission once, without blocking. Under the admission lock: reclaim
/// stale tickets, count occupancy, [`decide`], and materialize the resulting
/// ticket. The blocking wait (for a `Queued` outcome) is a separate step
/// ([`QueueTicket::wait_for_slot`]) so this core stays deterministic and
/// unit-testable by seeding `slots/` and `queue/`.
pub fn try_admit(runs_root: &Path, max: usize, cap: usize) -> Result<AdmissionOutcome> {
    // Non-Unix has no cross-process advisory lock (`flock` is Unix-only, like
    // the run-store `RunLock`), so a filesystem semaphore cannot be made
    // correct there. Admission is a documented no-op on those platforms —
    // always admit, matching pre-A0-04 behavior (unlimited runs) rather than
    // shipping a racy, un-reclaimable half-semaphore. See the module docs.
    #[cfg(not(unix))]
    {
        let _ = (runs_root, max, cap);
        return Ok(AdmissionOutcome::Admitted(RunSlot {
            path: PathBuf::new(),
        }));
    }
    #[cfg(unix)]
    {
        // Normalize `max` once so the decision AND the cached value a queued
        // ticket promotes against agree (a mis-set `0` clamps to `1`).
        let max = max.max(1);
        try_admit_unix(runs_root, max, cap)
    }
}

#[cfg(unix)]
fn try_admit_unix(runs_root: &Path, max: usize, cap: usize) -> Result<AdmissionOutcome> {
    let dirs = AdmissionDirs::at(runs_root);
    dirs.ensure().context("create admission directories")?;
    let _lock = AdmissionLock::acquire(&dirs)?;
    reclaim_stale(&dirs.slots)?;
    reclaim_stale(&dirs.queue)?;
    let active = count_tickets(&dirs.slots)?;
    let queued = count_tickets(&dirs.queue)?;
    match decide(active, queued, max, cap) {
        AdmissionDecision::Admit => {
            let path = create_ticket(&dirs.slots)?;
            Ok(AdmissionOutcome::Admitted(RunSlot { path }))
        }
        AdmissionDecision::Queue => {
            let path = create_ticket(&dirs.queue)?;
            Ok(AdmissionOutcome::Queued(QueueTicket {
                runs_root: runs_root.to_path_buf(),
                path,
                promoted: false,
                cached_max: max,
            }))
        }
        AdmissionDecision::Reject => Ok(AdmissionOutcome::Rejected {
            active,
            queued,
            cap,
        }),
    }
}

/// High-level admission for a foreground run: attempt admission and, if
/// queued, block until a slot frees. Returns the held [`RunSlot`] (drop it when
/// the run finishes) or a `Rejected` outcome the caller turns into an error.
pub async fn admit_blocking(
    runs_root: &Path,
    max: usize,
    cap: usize,
    timeout: Option<Duration>,
) -> Result<Result<RunSlot, RejectedAdmission>> {
    match try_admit(runs_root, max, cap)? {
        AdmissionOutcome::Admitted(slot) => Ok(Ok(slot)),
        AdmissionOutcome::Queued(ticket) => Ok(Ok(ticket.wait_for_slot(timeout).await?)),
        AdmissionOutcome::Rejected {
            active,
            queued,
            cap,
        } => Ok(Err(RejectedAdmission {
            active,
            queued,
            cap,
        })),
    }
}

/// Fail-closed rejection detail (queue full).
#[derive(Debug, Clone, Copy)]
pub struct RejectedAdmission {
    pub active: usize,
    pub queued: usize,
    pub cap: usize,
}

// ---------------------------------------------------------------------------
// Ticket file helpers
// ---------------------------------------------------------------------------

/// Create a ticket file whose name sorts by creation time and carries the
/// creating pid (for stale reclaim). Name: `<epoch_nanos>-<pid>-<rand>`.
fn create_ticket(dir: &Path) -> Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let rand = fastrand::u64(..);
    let name = format!("{nanos:039}-{pid}-{rand:016x}");
    let path = dir.join(&name);
    std::fs::write(&path, pid.to_string()).with_context(|| format!("write ticket {path:?}"))?;
    Ok(path)
}

/// Count live tickets in a directory (stale ones are reclaimed by the caller
/// first).
fn count_tickets(dir: &Path) -> Result<usize> {
    let mut n = 0;
    for entry in read_dir_opt(dir)? {
        let entry = entry?;
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            n += 1;
        }
    }
    Ok(n)
}

/// The lexicographically-smallest ticket file name (oldest, since the name is
/// epoch-prefixed and zero-padded).
fn oldest_ticket(dir: &Path) -> Result<Option<String>> {
    let mut oldest: Option<String> = None;
    for entry in read_dir_opt(dir)? {
        let entry = entry?;
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if let Some(name) = entry.file_name().to_str().map(str::to_string) {
            match &oldest {
                Some(cur) if *cur <= name => {}
                _ => oldest = Some(name),
            }
        }
    }
    Ok(oldest)
}

/// Remove tickets whose creating process is gone, so a crashed/SIGKILL'd run
/// never permanently holds a slot or wedges the queue.
fn reclaim_stale(dir: &Path) -> Result<()> {
    for entry in read_dir_opt(dir)? {
        let entry = entry?;
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let pid = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok());
        if let Some(pid) = pid
            && !process_alive(pid)
        {
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(())
}

fn read_dir_opt(dir: &Path) -> Result<Vec<io::Result<std::fs::DirEntry>>> {
    match std::fs::read_dir(dir) {
        Ok(rd) => Ok(rd.collect()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err).with_context(|| format!("read admission dir {dir:?}")),
    }
}

#[cfg(unix)]
fn process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // kill(pid, 0): 0 → alive; EPERM → alive (owned by another user); ESRCH →
    // gone. SAFETY: a signal-0 probe with no side effects.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_alive(_pid: i32) -> bool {
    // Non-unix: no cheap liveness probe; treat tickets as live (admission is
    // best-effort on these platforms — documented in agent.md).
    true
}

// ---------------------------------------------------------------------------
// Admission lock (flock, RAII)
// ---------------------------------------------------------------------------

/// RAII exclusive advisory lock serializing the admission critical section
/// across processes. Released on drop (including process death). Mirrors the
/// run-store `RunLock` flock pattern.
struct AdmissionLock {
    #[allow(dead_code)]
    file: std::fs::File,
}

impl AdmissionLock {
    fn acquire(dirs: &AdmissionDirs) -> Result<Self> {
        std::fs::create_dir_all(&dirs.root)
            .with_context(|| format!("create admission dir {:?}", dirs.root))?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&dirs.lock)
            .with_context(|| format!("open admission lock {:?}", dirs.lock))?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // Blocking exclusive lock; released when `file` drops.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if rc != 0 {
                return Err(io::Error::last_os_error()).context("flock admission lock");
            }
        }
        Ok(Self { file })
    }
}

#[cfg(unix)]
impl Drop for AdmissionLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        // SAFETY: plain libc syscall on a fd we own; closing also releases.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_admits_below_max() {
        assert_eq!(decide(0, 0, 2, 10), AdmissionDecision::Admit);
        assert_eq!(decide(1, 0, 2, 10), AdmissionDecision::Admit);
    }

    #[test]
    fn decide_queues_at_capacity_with_queue_room() {
        assert_eq!(decide(2, 0, 2, 10), AdmissionDecision::Queue);
        assert_eq!(decide(2, 9, 2, 10), AdmissionDecision::Queue);
    }

    #[test]
    fn decide_rejects_when_queue_full() {
        assert_eq!(decide(2, 10, 2, 10), AdmissionDecision::Reject);
        assert_eq!(decide(5, 10, 2, 10), AdmissionDecision::Reject);
    }

    #[test]
    fn decide_treats_zero_max_as_one() {
        // A mis-set max of 0 must never wedge every run — clamp to 1.
        assert_eq!(decide(0, 0, 0, 10), AdmissionDecision::Admit);
        assert_eq!(decide(1, 0, 0, 10), AdmissionDecision::Queue);
    }

    #[test]
    fn try_admit_admits_queues_then_rejects() {
        let dir = tempfile::tempdir().unwrap();
        let runs_root = dir.path();

        // Two slots (max=2): both admit.
        let s1 = match try_admit(runs_root, 2, 3).unwrap() {
            AdmissionOutcome::Admitted(slot) => slot,
            other => panic!("expected Admitted, got {other:?}"),
        };
        let _s2 = match try_admit(runs_root, 2, 3).unwrap() {
            AdmissionOutcome::Admitted(slot) => slot,
            other => panic!("expected Admitted, got {other:?}"),
        };

        // Slots full → next three queue up to cap=3.
        let _q1 = match try_admit(runs_root, 2, 3).unwrap() {
            AdmissionOutcome::Queued(t) => t,
            other => panic!("expected Queued, got {other:?}"),
        };
        let _q2 = match try_admit(runs_root, 2, 3).unwrap() {
            AdmissionOutcome::Queued(t) => t,
            other => panic!("expected Queued, got {other:?}"),
        };
        let q3 = match try_admit(runs_root, 2, 3).unwrap() {
            AdmissionOutcome::Queued(t) => t,
            other => panic!("expected Queued, got {other:?}"),
        };

        // Queue full → reject.
        match try_admit(runs_root, 2, 3).unwrap() {
            AdmissionOutcome::Rejected {
                active,
                queued,
                cap,
            } => {
                assert_eq!(active, 2);
                assert_eq!(queued, 3);
                assert_eq!(cap, 3);
            }
            other => panic!("expected Rejected, got {other:?}"),
        }

        // Freeing a queued reservation opens queue room again (FIFO: with
        // waiters still present a freed SLOT promotes a waiter, not a fresh
        // arrival — see `queue_ticket_promotes_when_slot_frees`).
        drop(q3);
        match try_admit(runs_root, 2, 3).unwrap() {
            AdmissionOutcome::Queued(_) => {}
            other => panic!("expected Queued after a queue slot freed, got {other:?}"),
        }
        // Keep the held tickets alive until here.
        let _ = &s1;
    }

    #[test]
    fn decide_queues_new_arrivals_behind_waiters() {
        // FIFO fairness: even with a free slot, a fresh arrival queues behind
        // an existing waiter rather than barging past it.
        assert_eq!(decide(1, 1, 2, 10), AdmissionDecision::Queue);
        assert_eq!(decide(0, 3, 2, 10), AdmissionDecision::Queue);
    }

    #[tokio::test]
    async fn queue_ticket_promotes_when_slot_frees() {
        let dir = tempfile::tempdir().unwrap();
        let runs_root = dir.path();

        let slot = match try_admit(runs_root, 1, 3).unwrap() {
            AdmissionOutcome::Admitted(s) => s,
            other => panic!("expected Admitted, got {other:?}"),
        };
        let ticket = match try_admit(runs_root, 1, 3).unwrap() {
            AdmissionOutcome::Queued(t) => t,
            other => panic!("expected Queued, got {other:?}"),
        };

        // Free the slot, then the queued ticket must promote to a slot.
        drop(slot);
        let promoted = ticket
            .wait_for_slot(Some(Duration::from_secs(5)))
            .await
            .expect("queued ticket promotes once the slot frees");
        drop(promoted);
    }
}
