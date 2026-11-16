//! Shared contract for the private, killable bounded-read helper.
//!
//! The helper is implemented by the main binary before normal CLI startup.
//! Library callers use it when a trusted descriptor can block in filesystem
//! I/O and an in-process cancellation point cannot enforce the host deadline.

#[cfg(any(unix, windows))]
use std::process::Stdio;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Instant,
};

use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
#[cfg(any(unix, windows))]
use tokio::io::AsyncWriteExt;

use crate::internal::ai::observed_agents::{
    MAX_REDACTION_MATCH_SAMPLES, RedactedBytes, RedactionReport, Redactor,
};

/// Private argv token accepted only before normal CLI initialization.
pub const AUTHORIZED_READ_HELPER_ARG: &str = "--libra-internal-authorized-read-helper";
/// Environment variable carrying the explicitly bounded byte cap.
pub const AUTHORIZED_READ_HELPER_CAP_ENV: &str = "LIBRA_INTERNAL_AUTHORIZED_READ_CAP";
/// Environment variable selecting the helper's fixed protocol mode.
pub const AUTHORIZED_READ_HELPER_MODE_ENV: &str = "LIBRA_INTERNAL_AUTHORIZED_READ_MODE";

/// The private helper and all callers share this hard cap. It is deliberately
/// not caller-configurable beyond a smaller cap, so a malformed helper request
/// cannot turn the capture boundary into an unbounded reader.
pub const AUTHORIZED_READ_HELPER_MAX_CAP: u64 = 16 * 1024 * 1024;

// This mode consumes the already-authorized source descriptor inherited as
// stdin.  Its name is intentionally versioned because the previous private
// helper request protocol carried a provider path, which ACF-03 forbids.
const LIVE_CLAUDE_SOURCE_MODE: &str = "live-claude-source-v2";
const LIVE_CLAUDE_REPORT_CAP: usize = 64 * 1024;
// The helper redactor keeps its final output below 1.5x raw input and uses at
// most a 2.5x two-buffer working set. That leaves room for placeholders which
// are modestly longer than short secret formats without violating ACF-03's
// capture-memory budget.
const LIVE_CLAUDE_REDACTED_NUMERATOR: u64 = 3;
const LIVE_CLAUDE_REDACTED_DENOMINATOR: u64 = 2;
const LIVE_CLAUDE_WORKING_SET_NUMERATOR: u64 = 5;
const LIVE_CLAUDE_WORKING_SET_DENOMINATOR: u64 = 2;
/// Fixed slack for metrics produced while the redactor walks its ordered
/// rule passes.  Counts and offsets are not tied to the initial raw buffer:
/// each pass observes the previous pass's output, and `bytes_redacted` is
/// cumulative.  Durable catalog validators share this margin rather than
/// assuming `redacted <= scanned`.
pub(crate) const CAPTURE_REDACTION_REPORT_RULE_PASS_CAP: usize = 64;
const LIVE_CLAUDE_FRAME_HEADER_BYTES: usize = 1 + 8 + 32 + 32 + 8 + 4;

const LIVE_SOURCE_COMPLETE: u8 = 0;
const LIVE_SOURCE_OVERSIZE: u8 = 1;
const LIVE_SOURCE_READ_ERROR: u8 = 2;
const LIVE_SOURCE_ABSENT: u8 = 3;
const LIVE_SOURCE_UNTRUSTED: u8 = 4;

/// Result of reading an untrusted helper stream without allowing `Vec` growth
/// beyond its declared content cap.  Oversize streams are detected with a
/// one-byte stack sentinel, so an EOF probe never needs to reserve a second
/// heap buffer.
pub(crate) enum StrictBoundedRead {
    Complete(Vec<u8>),
    Oversize {
        observed_bytes: u64,
    },
    Failed {
        bytes_read: u64,
        error: std::io::Error,
    },
}

pub(crate) fn read_strictly_bounded<R: Read>(reader: &mut R, cap: u64) -> StrictBoundedRead {
    let Ok(capacity) = usize::try_from(cap) else {
        return StrictBoundedRead::Failed {
            bytes_read: 0,
            error: std::io::Error::other("bounded read cap exceeds address space"),
        };
    };
    let mut bytes = Vec::new();
    if let Err(error) = bytes.try_reserve_exact(capacity) {
        return StrictBoundedRead::Failed {
            bytes_read: 0,
            error: std::io::Error::other(format!("allocate bounded read buffer: {error}")),
        };
    }
    if bytes.capacity() > capacity {
        return StrictBoundedRead::Failed {
            bytes_read: 0,
            error: std::io::Error::other("bounded read allocation exceeded its content cap"),
        };
    }

    let mut chunk = [0_u8; 8192];
    loop {
        let remaining = capacity.saturating_sub(bytes.len());
        if remaining == 0 {
            let mut sentinel = [0_u8; 1];
            loop {
                match reader.read(&mut sentinel) {
                    Ok(0) => return StrictBoundedRead::Complete(bytes),
                    Ok(_) => {
                        return StrictBoundedRead::Oversize {
                            observed_bytes: cap.saturating_add(1),
                        };
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        return StrictBoundedRead::Failed {
                            bytes_read: bytes.len() as u64,
                            error,
                        };
                    }
                }
            }
        }

        let read_len = remaining.min(chunk.len());
        match reader.read(&mut chunk[..read_len]) {
            Ok(0) => return StrictBoundedRead::Complete(bytes),
            Ok(read) => bytes.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return StrictBoundedRead::Failed {
                    bytes_read: bytes.len() as u64,
                    error,
                };
            }
        }
    }
}

/// Async counterpart of [`read_strictly_bounded`] for helper stdout frames.
/// The one-byte stack sentinel keeps exact-capacity responses from triggering
/// `read_to_end` growth during its final EOF probe.
pub(crate) async fn read_async_strictly_bounded<R>(
    reader: &mut R,
    cap: u64,
) -> std::io::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let capacity = usize::try_from(cap)
        .map_err(|_| std::io::Error::other("bounded helper response cap exceeds address space"))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| std::io::Error::other("allocate bounded helper response"))?;
    if bytes.capacity() > capacity {
        return Err(std::io::Error::other(
            "allocate bounded helper response exceeded its frame cap",
        ));
    }

    let mut chunk = [0_u8; 8192];
    loop {
        let remaining = capacity.saturating_sub(bytes.len());
        if remaining == 0 {
            let mut sentinel = [0_u8; 1];
            return match reader.read(&mut sentinel).await? {
                0 => Ok(bytes),
                _ => Err(std::io::Error::other(
                    "bounded helper response exceeds its frame cap",
                )),
            };
        }

        let read_len = remaining.min(chunk.len());
        let read = reader.read(&mut chunk[..read_len]).await?;
        if read == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
}

/// Durable capture projections intentionally retain no source-specific
/// identity.  An unkeyed public digest of a session id or provider-relative
/// path is an offline enumeration oracle, even when the raw string is absent.
pub(crate) const SOURCE_IDENTITY_NOT_RETAINED: &str = "not_retained:v1";

/// Safe result of the deadline-bound Claude source helper. It deliberately
/// carries only already-redacted source bytes; the durable projection uses a
/// fixed noncorrelating identity sentinel and never receives a provider path,
/// raw provider bytes, or raw helper error text.
pub(crate) enum LiveClaudeSourceRead {
    Complete {
        transcript_redacted: RedactedBytes,
        redaction_report: RedactionReport,
        raw_bytes: u64,
        digest_sha256: String,
    },
    Oversize,
    Absent,
    Untrusted,
    DeadlineExceeded,
    Failed,
}

/// Content-free outcome of a private helper invocation. Callers decode only
/// their own strictly bounded frame after this lifecycle layer has enforced
/// the absolute deadline and reaped a timed-out child.
pub(crate) enum RegisteredHelperOutput {
    #[cfg_attr(windows, allow(dead_code))]
    Output(Vec<u8>),
    #[cfg_attr(windows, allow(dead_code))]
    DeadlineExceeded,
    Failed,
}

/// Own a deadline-bound direct child across every cancellation path.
///
/// Tokio drops a future immediately when an outer host deadline cancels it.
/// `kill_on_drop` is a useful final backstop, but it does not give the caller
/// a deterministic reap path and it does not cancel sibling pipe-drain tasks.
/// This guard starts a non-blocking kill and hands the child to the current
/// Tokio runtime for reaping before the future's locals are released. Callers
/// must configure `kill_on_drop(true)` as a last-resort fallback, register
/// every spawned pipe task, and disarm only after `wait()` has completed.
pub(crate) struct CancellationSafeChild {
    child: Option<tokio::process::Child>,
    // Present only for helpers launched into an owned Unix process group.
    // Keep this identifier armed until the caller has drained every pipe and
    // reaped the direct leader; before that point descendants can still hold
    // raw descriptor or pipe handles open.
    process_group: Option<u32>,
    task_abort_handles: Vec<tokio::task::AbortHandle>,
    // Functional scratch must survive cancellation until the child is reaped.
    retained_until_reaped: Option<Box<dyn Send>>,
    #[cfg(windows)]
    job: Option<crate::utils::windows_process::CaptureJob>,
}

/// Runtime shutdown can cancel even an unpolled reaper task. Its resource
/// must survive until process exit unless wait() positively proves reaping.
struct RetainUnlessReaped(Option<Box<dyn Send>>);

impl RetainUnlessReaped {
    fn release_after_wait(&mut self) {
        self.0.take();
    }
}

impl Drop for RetainUnlessReaped {
    fn drop(&mut self) {
        if let Some(resource) = self.0.take() {
            std::mem::forget(resource);
        }
    }
}

impl CancellationSafeChild {
    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn new(child: tokio::process::Child) -> Self {
        Self {
            child: Some(child),
            process_group: None,
            task_abort_handles: Vec::new(),
            retained_until_reaped: None,
            #[cfg(windows)]
            job: None,
        }
    }

    /// Construct a guard for a helper whose command was configured with
    /// `process_group(0)` before `spawn`.  The direct child PID is then also
    /// the dedicated PGID while its leader remains unreaped.
    #[cfg_attr(windows, allow(dead_code))]
    pub(crate) fn new_process_group(child: tokio::process::Child) -> Self {
        Self {
            process_group: child.id(),
            child: Some(child),
            task_abort_handles: Vec::new(),
            retained_until_reaped: None,
            #[cfg(windows)]
            job: None,
        }
    }

    /// Spawn suspended, assign a kill-on-close job, then resume. Failure to
    /// establish containment kills the still-suspended child and returns an error.
    #[cfg(windows)]
    pub(crate) fn spawn_windows(command: &mut tokio::process::Command) -> std::io::Result<Self> {
        command.creation_flags(crate::utils::windows_process::CREATE_SUSPENDED_NO_WINDOW);
        let mut child = command.spawn()?;
        let job = child
            .id()
            .ok_or_else(|| std::io::Error::other("capture helper has no process id"))
            .and_then(crate::utils::windows_process::CaptureJob::attach_and_resume);
        match job {
            Ok(job) => Ok(Self {
                child: Some(child),
                process_group: None,
                task_abort_handles: Vec::new(),
                retained_until_reaped: None,
                job: Some(job),
            }),
            Err(error) => {
                let _ = child.start_kill();
                if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                    runtime.spawn(async move {
                        let _ = child.wait().await;
                    });
                }
                Err(error)
            }
        }
    }

    pub(crate) fn child_mut(&mut self) -> Option<&mut tokio::process::Child> {
        self.child.as_mut()
    }

    /// Register a pipe/read/write task that must not outlive a cancelled
    /// helper. Its typed join handle remains with the caller for normal
    /// result handling; only its type-erased abort capability lives here.
    pub(crate) fn register_abort_on_cancel<T>(&mut self, task: &tokio::task::JoinHandle<T>) {
        self.task_abort_handles.push(task.abort_handle());
    }

    /// Stop the child now and retain it in a detached reaper without waiting
    /// on an uninterruptible filesystem operation in the foreground hook.
    ///
    /// The checked form is for callers whose user-visible outcome must not
    /// claim termination when `start_kill` itself failed. The detached reaper
    /// still receives the child even in that case: it may already have exited,
    /// and retaining it would leak a zombie.
    pub(crate) fn terminate_and_reap_checked(&mut self) -> std::io::Result<()> {
        #[cfg(windows)]
        self.job.take();
        // Kill the owned process group *before* reaping its leader. A helper
        // can fork/exec a descendant that inherits raw stdin or stdout; once
        // `wait()` observes the leader, its PID may be recycled and a later
        // negative-PID signal would be unsafe. Callers therefore drain stdout
        // before `wait()` and only invoke this path while the leader is live
        // or unreaped.
        let group_result = terminate_owned_process_group(self.process_group.take());
        for task in self.task_abort_handles.drain(..) {
            task.abort();
        }
        let Some(mut child) = self.child.take() else {
            return group_result;
        };
        let kill_result = child.start_kill();
        // This guard is constructed and dropped only while polling Tokio
        // capture futures. `try_current` keeps Drop non-panicking during
        // runtime teardown; `kill_on_drop` remains the fallback in that
        // exceptional case.
        let mut retained = RetainUnlessReaped(self.retained_until_reaped.take());
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if child.wait().await.is_ok() {
                    retained.release_after_wait();
                }
            });
        } else {
            // start_kill/group termination already ran above. With no runtime
            // there is no wait proof: retain private scratch until process
            // exit rather than removing it while the helper may still run.
            drop(child);
            drop(retained);
        }
        group_result.and(kill_result)
    }

    /// Best-effort cancellation used by generic callers that map all child
    /// failures to one fixed status. Security-sensitive callers should use
    /// [`Self::terminate_and_reap_checked`] and report an unconfirmed
    /// termination rather than claim a kill succeeded.
    pub(crate) fn terminate_and_reap(&mut self) {
        let _ = self.terminate_and_reap_checked();
    }

    /// `wait()` has completed, so dropping the handle cannot leave a live
    /// leader. Call only after every inherited-output pipe has reached EOF:
    /// after this point the leader PID may be recycled, so the group is
    /// deliberately disarmed and will never receive a stale signal.
    pub(crate) fn disarm_child_after_wait(&mut self) {
        self.child.take();
        self.process_group = None;
        #[cfg(windows)]
        self.job.take();
    }

    /// Every child and registered pipe task has completed normally.
    pub(crate) fn finish(&mut self) {
        self.disarm_child_after_wait();
        self.task_abort_handles.clear();
        self.retained_until_reaped.take();
    }
}

/// Put a private helper in a dedicated process group before spawning it.
///
/// The group makes it possible to terminate forked descendants that inherit
/// a raw descriptor or a response pipe. Non-Unix callers must fail closed at
/// their raw-source boundary rather than rely on this no-op.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn configure_private_helper_process_group(command: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        command.as_std_mut().process_group(0);
    }
    #[cfg(not(unix))]
    {
        let _ = command;
    }
}

#[cfg(unix)]
fn terminate_owned_process_group(pgid: Option<u32>) -> std::io::Result<()> {
    if let Some(pgid) = pgid.filter(|pid| *pid > 1)
        // SAFETY: `new_process_group` is used only after `process_group(0)`;
        // this call runs before reaping that group leader, so negative PID
        // names only the still-owned helper group.
        && unsafe { libc::kill(-(pgid as libc::pid_t), libc::SIGKILL) } != 0
    {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn terminate_owned_process_group(_pgid: Option<u32>) -> std::io::Result<()> {
    Ok(())
}

impl Drop for CancellationSafeChild {
    fn drop(&mut self) {
        self.terminate_and_reap();
    }
}

static RUNNING_LIBRA_PROGRAM: OnceLock<PathBuf> = OnceLock::new();

tokio::task_local! {
    // Only the ordinary prepared-import adapter can opt into this scope.
    // It never changes source-reader or native-producer executable selection.
    static EXPLICIT_IMPORT_CHECKPOINT_HELPER: PathBuf;
}

#[cfg(test)]
tokio::task_local! {
    static TEST_AUTHORIZED_READ_HELPER_PROGRAM: Option<PathBuf>;
}

/// Register the actual executable that entered Libra's `main` function.
///
/// Hook installation permits a canonicalized `--binary-path` with an
/// arbitrary filename. Only the main binary can establish this process-local
/// fact; library callers that were embedded into another host deliberately do
/// not infer that the host understands the private helper argument.
pub fn register_running_program() {
    if let Ok(program) = std::env::current_exe() {
        let _ = RUNNING_LIBRA_PROGRAM.set(program);
    }
}

/// Resolve the Libra binary that owns the private helper entrypoint.
///
/// A normal CLI process resolves to the executable registered by its main
/// entrypoint, even when it was installed under a renamed filename. An
/// unrelated embedded host receives no helper rather than being invoked with
/// a private argument it does not understand.
pub fn helper_program() -> Option<PathBuf> {
    #[cfg(test)]
    if let Ok(program) = TEST_AUTHORIZED_READ_HELPER_PROGRAM.try_with(Clone::clone) {
        return program;
    }

    helper_program_from_registered(RUNNING_LIBRA_PROGRAM.get().map(PathBuf::as_path))
}

/// Construct a private helper command from the program registered by Libra's
/// `main`. This is the one lifecycle entrypoint shared by source reads and
/// deadline-bound CPU workers; embedded hosts deliberately receive no helper.
pub(crate) fn registered_helper_command(argument: &str) -> Option<tokio::process::Command> {
    let explicit = if argument == super::history::CHECKPOINT_OBJECT_IO_HELPER_ARG {
        EXPLICIT_IMPORT_CHECKPOINT_HELPER
            .try_with(Clone::clone)
            .ok()
    } else {
        None
    };
    let program = explicit.or_else(helper_program)?;
    let mut command = tokio::process::Command::new(program);
    command.arg(argument);
    Some(command)
}

/// The ordinary direct-import caller explicitly authorizes a Libra executable
/// for checkpoint object I/O only. This is execution configuration, never
/// binary attestation or native provenance. The caller rejects reasoning before
/// entering; cloud/doctor's VerifyReasoningArtifacts must not run in this scope.
pub(crate) async fn with_explicit_import_checkpoint_helper<F, T>(
    program: Option<&Path>,
    deadline: Instant,
    future: F,
) -> anyhow::Result<T>
where
    F: std::future::Future<Output = anyhow::Result<T>>,
{
    #[cfg(not(unix))]
    {
        let _ = (program, deadline, future);
        anyhow::bail!("explicit import checkpoint helpers require Unix process containment");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if Instant::now() >= deadline {
            anyhow::bail!("explicit import checkpoint helper validation exceeded its deadline");
        }
        let Some(program) = program else {
            if helper_program().is_none() {
                anyhow::bail!(
                    "explicit import requires a trusted Libra checkpoint helper; pass the absolute Libra binary path"
                );
            }
            return future.await;
        };
        if !program.is_absolute() {
            anyhow::bail!(
                "explicit import checkpoint helper must be an absolute trusted Libra binary path"
            );
        }
        let metadata = tokio::time::timeout_at(deadline.into(), tokio::fs::metadata(program))
            .await
            .map_err(|_| anyhow::anyhow!("explicit import checkpoint helper validation exceeded its deadline"))?
            .map_err(|_| anyhow::anyhow!("explicit import checkpoint helper is unavailable; select an existing trusted Libra binary"))?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            anyhow::bail!(
                "explicit import checkpoint helper must be an executable regular Libra binary"
            );
        }
        if Instant::now() >= deadline {
            anyhow::bail!("explicit import checkpoint helper validation exceeded its deadline");
        }
        EXPLICIT_IMPORT_CHECKPOINT_HELPER
            .scope(program.to_path_buf(), future)
            .await
    }
}

fn helper_program_from_registered(running_program: Option<&Path>) -> Option<PathBuf> {
    running_program.map(Path::to_path_buf)
}

/// Launch the private helper that owns the bounded raw-byte phase of one
/// descriptor-pinned live Claude source: read, redaction, and
/// redacted-content digest. The resolver performs scope/root authorization;
/// with a deadline, the helper owns preparer/flush work and the parent only
/// rewinds the held descriptor before transferring it as stdin. No locator,
/// provider/session identity, metadata JSON, or native bytes is serialized
/// into a helper control wire.
pub(crate) async fn read_live_claude_source_until(
    source: std::fs::File,
    cap: u64,
    deadline: Instant,
) -> LiveClaudeSourceRead {
    if Instant::now() >= deadline {
        return LiveClaudeSourceRead::DeadlineExceeded;
    }
    if cap > AUTHORIZED_READ_HELPER_MAX_CAP {
        return LiveClaudeSourceRead::Oversize;
    }
    let Some(mut command) = registered_helper_command(AUTHORIZED_READ_HELPER_ARG) else {
        return LiveClaudeSourceRead::Failed;
    };
    command
        .env_clear()
        .current_dir(std::path::Path::new("/"))
        .env(AUTHORIZED_READ_HELPER_CAP_ENV, cap.to_string())
        .env(AUTHORIZED_READ_HELPER_MODE_ENV, LIVE_CLAUDE_SOURCE_MODE);
    let output_cap = capture_redacted_output_cap(cap)
        .saturating_add(LIVE_CLAUDE_REPORT_CAP as u64)
        .saturating_add(LIVE_CLAUDE_FRAME_HEADER_BYTES as u64);
    match run_registered_descriptor_helper_until(command, source, output_cap, deadline).await {
        RegisteredHelperOutput::Output(output) => decode_live_claude_frame(output, cap),
        RegisteredHelperOutput::DeadlineExceeded => LiveClaudeSourceRead::DeadlineExceeded,
        RegisteredHelperOutput::Failed => LiveClaudeSourceRead::Failed,
    }
}

/// Run a registered helper with a held descriptor as stdin and a strictly
/// bounded stdout frame. The descriptor itself is the capability; environment
/// controls are limited to fixed mode/cap values by the caller. This mirrors
/// the import descriptor handoff rather than re-addressing a provider path in
/// the child.
pub(crate) async fn run_registered_descriptor_helper_until(
    command: tokio::process::Command,
    source: std::fs::File,
    output_cap: u64,
    deadline: Instant,
) -> RegisteredHelperOutput {
    run_registered_descriptor_helper_retaining_until(command, source, output_cap, deadline, None)
        .await
}

/// A parent-owned functional resource follows the helper into its cancellation
/// reaper. It is released after wait(), rather than when the host future drops.
#[cfg_attr(windows, allow(unused_mut))]
pub(crate) async fn run_registered_descriptor_helper_retaining_until(
    mut command: tokio::process::Command,
    source: std::fs::File,
    output_cap: u64,
    deadline: Instant,
    retained: Option<Box<dyn Send>>,
) -> RegisteredHelperOutput {
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (command, source, output_cap, deadline, retained);
        // A descriptor-bearing helper must remain contained as one owned
        // process group. Platforms without that guarantee fail closed.
        return RegisteredHelperOutput::Failed;
    }

    #[cfg(any(unix, windows))]
    {
        if Instant::now() >= deadline {
            return RegisteredHelperOutput::DeadlineExceeded;
        }
        command
            .stdin(Stdio::from(source))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        configure_private_helper_process_group(&mut command);
        #[cfg(unix)]
        let child = match command.spawn() {
            Ok(child) => child,
            Err(_) => return RegisteredHelperOutput::Failed,
        };
        #[cfg(unix)]
        let mut child = CancellationSafeChild::new_process_group(child);
        #[cfg(windows)]
        let mut child = match CancellationSafeChild::spawn_windows(&mut command) {
            Ok(child) => child,
            Err(_) => return RegisteredHelperOutput::Failed,
        };
        child.retained_until_reaped = retained;
        let Some(mut stdout) = child.child_mut().and_then(|child| child.stdout.take()) else {
            child.terminate_and_reap();
            return RegisteredHelperOutput::Failed;
        };

        let mut stdout_task =
            tokio::spawn(async move { read_async_strictly_bounded(&mut stdout, output_cap).await });
        child.register_abort_on_cancel(&stdout_task);

        // Drain before reaping: a descendant can retain stdout after the
        // leader exits, and the unreaped group leader is our safe kill anchor.
        let output = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            &mut stdout_task,
        )
        .await
        {
            Ok(Ok(Ok(output))) => output,
            Ok(Ok(Err(_))) | Ok(Err(_)) => {
                child.terminate_and_reap();
                return RegisteredHelperOutput::Failed;
            }
            Err(_) => {
                stdout_task.abort();
                child.terminate_and_reap();
                return RegisteredHelperOutput::DeadlineExceeded;
            }
        };
        let status = match child.child_mut() {
            Some(child_process) => match tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                child_process.wait(),
            )
            .await
            {
                Ok(Ok(status)) => status,
                Ok(Err(_)) => {
                    child.terminate_and_reap();
                    return RegisteredHelperOutput::Failed;
                }
                Err(_) => {
                    child.terminate_and_reap();
                    return RegisteredHelperOutput::DeadlineExceeded;
                }
            },
            None => return RegisteredHelperOutput::Failed,
        };
        child.disarm_child_after_wait();
        child.finish();
        if Instant::now() >= deadline {
            return RegisteredHelperOutput::DeadlineExceeded;
        }
        if !status.success() {
            return RegisteredHelperOutput::Failed;
        }
        RegisteredHelperOutput::Output(output)
    }
}

/// Run a private helper with one bounded stdin frame and a bounded stdout
/// frame under an absolute deadline. Both source I/O and CPU-only workers use
/// this path so they cannot diverge on pipe draining, kill/reap, or deadline
/// behavior.
#[cfg_attr(windows, allow(unused_mut))]
pub(crate) async fn run_registered_bounded_helper_until(
    mut command: tokio::process::Command,
    request_parts: &[&[u8]],
    output_cap: u64,
    deadline: Instant,
) -> RegisteredHelperOutput {
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (command, request_parts, output_cap, deadline);
        // Private helper descendants cannot be safely contained with the
        // Unix process-group contract on this platform. Callers must retain
        // their fixed fail-closed result instead of spawning a raw-pipe
        // helper that could outlive the host deadline.
        return RegisteredHelperOutput::Failed;
    }

    #[cfg(any(unix, windows))]
    {
        if Instant::now() >= deadline {
            return RegisteredHelperOutput::DeadlineExceeded;
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        configure_private_helper_process_group(&mut command);
        #[cfg(unix)]
        let child = match command.spawn() {
            Ok(child) => child,
            Err(_) => return RegisteredHelperOutput::Failed,
        };
        #[cfg(unix)]
        let mut child = CancellationSafeChild::new_process_group(child);
        #[cfg(windows)]
        let mut child = match CancellationSafeChild::spawn_windows(&mut command) {
            Ok(child) => child,
            Err(_) => return RegisteredHelperOutput::Failed,
        };
        let Some(mut stdin) = child.child_mut().and_then(|child| child.stdin.take()) else {
            child.terminate_and_reap();
            return RegisteredHelperOutput::Failed;
        };
        let Some(mut stdout) = child.child_mut().and_then(|child| child.stdout.take()) else {
            child.terminate_and_reap();
            return RegisteredHelperOutput::Failed;
        };

        // Start draining immediately, before attempting a potentially large
        // request write. A helper is allowed to fill its bounded response before
        // consuming stdin; delaying this task would deadlock on a full pipe.
        let mut stdout_task =
            tokio::spawn(async move { read_async_strictly_bounded(&mut stdout, output_cap).await });
        child.register_abort_on_cancel(&stdout_task);

        // Finish writing and close stdin before waiting. A helper that needs EOF
        // to produce output would otherwise deadlock with its parent.
        let request_result =
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
                for part in request_parts {
                    stdin.write_all(part).await?;
                }
                stdin.shutdown().await
            })
            .await;
        match request_result {
            Ok(Ok(())) => drop(stdin),
            Ok(Err(_)) => {
                child.terminate_and_reap();
                return RegisteredHelperOutput::Failed;
            }
            Err(_) => {
                child.terminate_and_reap();
                return RegisteredHelperOutput::DeadlineExceeded;
            }
        }

        // Do not reap the group leader before stdout reaches EOF. A forked
        // descendant can retain both pipes after the leader exits; keeping that
        // leader unreaped lets timeout/Drop kill the still-owned process group.
        let output = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            &mut stdout_task,
        )
        .await
        {
            Ok(Ok(Ok(output))) => output,
            Ok(Ok(Err(_))) | Ok(Err(_)) => {
                child.terminate_and_reap();
                return RegisteredHelperOutput::Failed;
            }
            Err(_) => {
                stdout_task.abort();
                child.terminate_and_reap();
                return RegisteredHelperOutput::DeadlineExceeded;
            }
        };
        let status = match child.child_mut() {
            Some(child_process) => match tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                child_process.wait(),
            )
            .await
            {
                Ok(Ok(status)) => status,
                Ok(Err(_)) => {
                    child.terminate_and_reap();
                    return RegisteredHelperOutput::Failed;
                }
                Err(_) => {
                    child.terminate_and_reap();
                    return RegisteredHelperOutput::DeadlineExceeded;
                }
            },
            None => return RegisteredHelperOutput::Failed,
        };
        // stdout has reached EOF and `wait()` reaped the leader, so this is the
        // one safe point to disarm the PGID before any later await/error path.
        child.disarm_child_after_wait();
        child.finish();
        if Instant::now() >= deadline {
            return RegisteredHelperOutput::DeadlineExceeded;
        }
        if !status.success() {
            return RegisteredHelperOutput::Failed;
        }
        RegisteredHelperOutput::Output(output)
    }
}

fn decode_live_claude_frame(mut frame: Vec<u8>, cap: u64) -> LiveClaudeSourceRead {
    if frame.len() < LIVE_CLAUDE_FRAME_HEADER_BYTES {
        return LiveClaudeSourceRead::Failed;
    }
    let status = frame[0];
    let Ok(raw_bytes) = <[u8; 8]>::try_from(&frame[1..9]).map(u64::from_le_bytes) else {
        return LiveClaudeSourceRead::Failed;
    };
    let Ok(identity) = <[u8; 32]>::try_from(&frame[9..41]) else {
        return LiveClaudeSourceRead::Failed;
    };
    let Ok(redacted_digest) = <[u8; 32]>::try_from(&frame[41..73]) else {
        return LiveClaudeSourceRead::Failed;
    };
    let Ok(redacted_len) = <[u8; 8]>::try_from(&frame[73..81]).map(u64::from_le_bytes) else {
        return LiveClaudeSourceRead::Failed;
    };
    let Ok(report_len) =
        <[u8; 4]>::try_from(&frame[81..LIVE_CLAUDE_FRAME_HEADER_BYTES]).map(u32::from_le_bytes)
    else {
        return LiveClaudeSourceRead::Failed;
    };
    let payload = &frame[LIVE_CLAUDE_FRAME_HEADER_BYTES..];
    match status {
        LIVE_SOURCE_COMPLETE
            if raw_bytes <= cap
                && identity == [0; 32]
                && redacted_len <= capture_redacted_output_cap(cap)
                && report_len as usize <= LIVE_CLAUDE_REPORT_CAP
                && payload.len()
                    == usize::try_from(redacted_len)
                        .ok()
                        .and_then(|redacted_len| {
                            usize::try_from(report_len)
                                .ok()
                                .and_then(|report_len| redacted_len.checked_add(report_len))
                        })
                        .unwrap_or(usize::MAX) =>
        {
            let Ok(redacted_len) = usize::try_from(redacted_len) else {
                return LiveClaudeSourceRead::Failed;
            };
            let report = {
                let redacted_payload = &payload[..redacted_len];
                let actual_digest: [u8; 32] = Sha256::digest(redacted_payload).into();
                if actual_digest != redacted_digest {
                    return LiveClaudeSourceRead::Failed;
                }
                match serde_json::from_slice::<RedactionReport>(&payload[redacted_len..]) {
                    Ok(report)
                        if valid_helper_redaction_report(&report, raw_bytes, cap, redacted_len) =>
                    {
                        report
                    }
                    _ => return LiveClaudeSourceRead::Failed,
                }
            };
            // Retain the exact stdout allocation for redacted bytes. A
            // `to_vec` here would briefly duplicate up to 1.5x the source
            // cap in the hook parent. The report was parsed above, so the
            // trailing bytes can now be discarded in place.
            frame.copy_within(
                LIVE_CLAUDE_FRAME_HEADER_BYTES..LIVE_CLAUDE_FRAME_HEADER_BYTES + redacted_len,
                0,
            );
            frame.truncate(redacted_len);
            LiveClaudeSourceRead::Complete {
                transcript_redacted: RedactedBytes::new_unchecked(frame),
                redaction_report: report,
                raw_bytes,
                digest_sha256: format!("sha256:{}", hex::encode(redacted_digest)),
            }
        }
        LIVE_SOURCE_OVERSIZE
            if raw_bytes > cap
                && identity == [0; 32]
                && redacted_len == 0
                && report_len == 0
                && redacted_digest == [0; 32]
                && payload.is_empty() =>
        {
            LiveClaudeSourceRead::Oversize
        }
        LIVE_SOURCE_READ_ERROR
            if redacted_len == 0
                && report_len == 0
                && redacted_digest == [0; 32]
                && payload.is_empty() =>
        {
            LiveClaudeSourceRead::Failed
        }
        LIVE_SOURCE_ABSENT
            if raw_bytes == 0
                && identity == [0; 32]
                && redacted_digest == [0; 32]
                && redacted_len == 0
                && report_len == 0
                && payload.is_empty() =>
        {
            LiveClaudeSourceRead::Absent
        }
        LIVE_SOURCE_UNTRUSTED
            if raw_bytes == 0
                && identity == [0; 32]
                && redacted_digest == [0; 32]
                && redacted_len == 0
                && report_len == 0
                && payload.is_empty() =>
        {
            LiveClaudeSourceRead::Untrusted
        }
        _ => LiveClaudeSourceRead::Failed,
    }
}

fn valid_helper_redaction_report(
    report: &RedactionReport,
    raw_bytes: u64,
    requested_cap: u64,
    redacted_len: usize,
) -> bool {
    let coordinate_cap =
        usize::try_from(capture_redacted_output_cap(requested_cap)).unwrap_or(usize::MAX);
    let metric_cap = coordinate_cap.saturating_mul(CAPTURE_REDACTION_REPORT_RULE_PASS_CAP);
    report.matches.len() <= MAX_REDACTION_MATCH_SAMPLES
        && report.bytes_scanned == usize::try_from(raw_bytes).unwrap_or(usize::MAX)
        && redacted_len <= coordinate_cap
        && report.bytes_redacted <= metric_cap
        && report.dropped_matches <= metric_cap
        && report.matches.iter().all(|matched| {
            matched.rule_id.len() <= 128
                && matched.start <= matched.end
                && matched.end <= coordinate_cap
        })
}

const CODEX_SOURCE_MODE: &str = "codex-live-source-v1";
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct CodexSourceControl {
    mode: String,
    session_id: String,
    working_dir: PathBuf,
    remaining_ms: u64,
}

pub(crate) async fn read_live_codex_source_until(
    source: std::fs::File,
    cap: u64,
    session_id: &str,
    working_dir: &Path,
    deadline: Instant,
) -> LiveClaudeSourceRead {
    let remaining_ms = deadline
        .saturating_duration_since(Instant::now())
        .as_millis()
        .min(60_000) as u64;
    if remaining_ms == 0 {
        return LiveClaudeSourceRead::DeadlineExceeded;
    }
    if cap > AUTHORIZED_READ_HELPER_MAX_CAP {
        return LiveClaudeSourceRead::Oversize;
    }
    let control = CodexSourceControl {
        mode: CODEX_SOURCE_MODE.to_owned(),
        session_id: session_id.to_owned(),
        working_dir: working_dir.to_owned(),
        remaining_ms,
    };
    let Ok(mode) = serde_json::to_string(&control) else {
        return LiveClaudeSourceRead::Failed;
    };
    if mode.len() > 16 * 1024 {
        return LiveClaudeSourceRead::Failed;
    }
    let Some(mut command) = registered_helper_command(AUTHORIZED_READ_HELPER_ARG) else {
        return LiveClaudeSourceRead::Failed;
    };
    command
        .env_clear()
        .current_dir(Path::new("/"))
        .env(AUTHORIZED_READ_HELPER_CAP_ENV, cap.to_string())
        .env(AUTHORIZED_READ_HELPER_MODE_ENV, mode);
    let output_cap = capture_redacted_output_cap(cap)
        .saturating_add(LIVE_CLAUDE_REPORT_CAP as u64)
        .saturating_add(LIVE_CLAUDE_FRAME_HEADER_BYTES as u64);
    match run_registered_descriptor_helper_until(command, source, output_cap, deadline).await {
        // Only the ordinary frame codec is shared; the Codex worker performs
        // its own scope check and decoded-value scan before emitting bytes.
        RegisteredHelperOutput::Output(bytes) => decode_live_claude_frame(bytes, cap),
        RegisteredHelperOutput::DeadlineExceeded => LiveClaudeSourceRead::DeadlineExceeded,
        RegisteredHelperOutput::Failed => LiveClaudeSourceRead::Failed,
    }
}

pub(crate) fn normalized_codex_cwd(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => return None,
            Component::CurDir => {}
            #[cfg(windows)]
            Component::Prefix(prefix) => match prefix.kind() {
                std::path::Prefix::Disk(drive) => {
                    normalized.push(format!("{}:", char::from(drive.to_ascii_uppercase())))
                }
                _ => return None,
            },
            #[cfg(not(windows))]
            Component::Prefix(_) => return None,
            other => normalized.push(other.as_os_str()),
        }
    }
    #[cfg(target_os = "macos")]
    for (alias, canonical) in [
        ("/var", "/private/var"),
        ("/tmp", "/private/tmp"),
        ("/etc", "/private/etc"),
    ] {
        if let Ok(suffix) = normalized.strip_prefix(alias) {
            return Some(Path::new(canonical).join(suffix));
        }
    }
    Some(normalized)
}
fn codex_header_matches(source: &[u8], control: &CodexSourceControl) -> bool {
    use crate::internal::ai::observed_agents::{CanonValue, parse_canon_value};
    let Some(line) = source
        .split(|b| *b == b'\n')
        .find(|line| !line.iter().all(u8::is_ascii_whitespace))
    else {
        return false;
    };
    if line.len() > 1024 * 1024 {
        return false;
    }
    let Ok(header) = parse_canon_value(line) else {
        return false;
    };
    if header.get("type").and_then(CanonValue::as_str) != Some("session_meta") {
        return false;
    }
    let Some(payload) = header.get("payload") else {
        return false;
    };
    let cwd = payload
        .get("cwd")
        .and_then(CanonValue::as_str)
        .and_then(|cwd| normalized_codex_cwd(Path::new(cwd)));
    payload.get("id").and_then(CanonValue::as_str) == Some(control.session_id.as_str())
        && cwd.is_some()
        && cwd == normalized_codex_cwd(&control.working_dir)
}

/// Private modes only, dispatched before any CLI/log initialization. Source
/// bytes are read solely from the held stdin descriptor; controls cannot name
/// a transcript to reopen. The locator returns only a bounded relative path.
pub fn run_codex_capture_helper_if_requested(cap: u64, mode: &str) -> Option<i32> {
    if let Some(code) =
        crate::internal::ai::observed_agents::pi_live::run_pi_session_source_helper_if_requested(
            cap, mode,
        )
    {
        return Some(code);
    }
    if mode == crate::internal::ai::observed_agents::codex_index::LOCATE_MODE {
        return Some(crate::internal::ai::observed_agents::codex_index::run_locator_helper(cap));
    }
    if let Some(result) = super::provider_index_io::run_helper_if_requested(cap, mode) {
        return Some(result);
    }
    if !mode.starts_with('{') {
        return None;
    }
    if mode.len() > 16 * 1024 {
        return Some(2);
    }
    let Ok(control) = serde_json::from_str::<CodexSourceControl>(mode) else {
        return Some(2);
    };
    if control.mode != CODEX_SOURCE_MODE
        || !(1..=60_000).contains(&control.remaining_ms)
        || control.session_id.is_empty()
        || control.session_id.len() > 64
        || !control
            .session_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        || normalized_codex_cwd(&control.working_dir).is_none()
    {
        return Some(2);
    }
    let response = duplicate_helper_stdin_descriptor().map_or_else(
        || LiveClaudeHelperResponse::read_error(0),
        |file| read_codex_source(file, cap, &control),
    );
    Some(if write_live_claude_frame(response) {
        0
    } else {
        1
    })
}
fn read_codex_source(
    mut file: std::fs::File,
    cap: u64,
    control: &CodexSourceControl,
) -> LiveClaudeHelperResponse {
    use zeroize::Zeroizing;
    if !rewind_file(&file) {
        return LiveClaudeHelperResponse::read_error(0);
    }
    let raw = match read_strictly_bounded(&mut file, cap) {
        StrictBoundedRead::Complete(bytes) => Zeroizing::new(bytes),
        StrictBoundedRead::Oversize { observed_bytes } => {
            return LiveClaudeHelperResponse {
                status: LIVE_SOURCE_OVERSIZE,
                raw_bytes: observed_bytes,
                ..LiveClaudeHelperResponse::read_error(0)
            };
        }
        StrictBoundedRead::Failed { bytes_read, .. } => {
            return LiveClaudeHelperResponse::read_error(bytes_read);
        }
    };
    if !codex_header_matches(&raw, control) {
        return LiveClaudeHelperResponse {
            status: LIVE_SOURCE_UNTRUSTED,
            ..LiveClaudeHelperResponse::read_error(0)
        };
    }
    let raw_bytes = raw.len() as u64;
    let deadline = Instant::now() + std::time::Duration::from_millis(control.remaining_ms);
    let Ok((sanitized, decoded_report)) =
        crate::internal::ai::observed_agents::codex_reasoning::prepare_ordinary(
            &raw,
            Some(deadline),
        )
    else {
        return LiveClaudeHelperResponse::read_error(raw_bytes);
    };
    drop(raw);
    let mut response = redact_live_claude_source(sanitized, cap);
    if response.status != LIVE_SOURCE_COMPLETE {
        return LiveClaudeHelperResponse::read_error(raw_bytes);
    }
    let Ok(mut report) = serde_json::from_slice::<RedactionReport>(&response.redaction_report)
    else {
        return LiveClaudeHelperResponse::read_error(raw_bytes);
    };
    report.bytes_scanned = raw_bytes as usize;
    report.bytes_redacted = report
        .bytes_redacted
        .saturating_add(decoded_report.bytes_redacted);
    report.dropped_matches = report
        .dropped_matches
        .saturating_add(decoded_report.match_count());
    let Ok(report) = serde_json::to_vec(&report) else {
        return LiveClaudeHelperResponse::read_error(raw_bytes);
    };
    if report.len() > LIVE_CLAUDE_REPORT_CAP {
        return LiveClaudeHelperResponse::read_error(raw_bytes);
    }
    response.raw_bytes = raw_bytes;
    response.redaction_report = report;
    response
}

/// Execute the descriptor-only live-source helper mode before normal CLI
/// startup. stdin is the parent-pinned source capability, never a request
/// frame. This helper does not receive or reopen a provider path.
pub fn run_live_claude_source_helper(cap: u64) -> i32 {
    let response = duplicate_helper_stdin_descriptor().map_or_else(
        || LiveClaudeHelperResponse::read_error(0),
        |source| run_live_claude_source_read(source, cap),
    );
    if write_live_claude_frame(response) {
        0
    } else {
        1
    }
}

#[cfg(unix)]
pub(crate) fn duplicate_helper_stdin_descriptor() -> Option<std::fs::File> {
    use std::os::fd::FromRawFd;

    // SAFETY: dup creates one owned descriptor referring to the helper's
    // inherited stdin capability. It is transferred exactly once to File.
    let fd = unsafe { libc::dup(libc::STDIN_FILENO) };
    if fd < 0 {
        return None;
    }
    // SAFETY: `fd` was freshly returned by dup and is now owned by File.
    Some(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn duplicate_helper_stdin_descriptor() -> Option<std::fs::File> {
    None
}

#[cfg(windows)]
pub(crate) fn duplicate_helper_stdin_descriptor() -> Option<std::fs::File> {
    use std::os::windows::io::{AsRawHandle, BorrowedHandle};
    let stdin = std::io::stdin();
    // SAFETY: stdin remains live while its borrowed handle is duplicated.
    let owned = unsafe { BorrowedHandle::borrow_raw(stdin.as_raw_handle()) }
        .try_clone_to_owned()
        .ok()?;
    Some(std::fs::File::from(owned))
}

struct LiveClaudeHelperResponse {
    status: u8,
    raw_bytes: u64,
    source_identity: [u8; 32],
    redacted_digest: [u8; 32],
    transcript_redacted: Vec<u8>,
    redaction_report: Vec<u8>,
}

impl LiveClaudeHelperResponse {
    fn read_error(raw_bytes: u64) -> Self {
        Self {
            status: LIVE_SOURCE_READ_ERROR,
            raw_bytes,
            source_identity: [0; 32],
            redacted_digest: [0; 32],
            // Do not carry a filesystem/helper error in the frame: callers
            // map this only to the typed safe partial reason.
            transcript_redacted: Vec::new(),
            redaction_report: Vec::new(),
        }
    }
}

#[cfg_attr(windows, allow(unused_mut, unused_variables, unreachable_code))]
fn run_live_claude_source_read(mut file: std::fs::File, cap: u64) -> LiveClaudeHelperResponse {
    #[cfg(unix)]
    {
        crate::internal::ai::observed_agents::builtin::claude_code::prepare_held_file_for_capture(
            &file,
        );
        if !rewind_file(&file) {
            return LiveClaudeHelperResponse::read_error(0);
        }
    }
    #[cfg(windows)]
    if !rewind_file(&file) {
        return LiveClaudeHelperResponse::read_error(0);
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = &file;
        return LiveClaudeHelperResponse::read_error(0);
    }
    match read_strictly_bounded(&mut file, cap) {
        StrictBoundedRead::Oversize { observed_bytes } => LiveClaudeHelperResponse {
            status: LIVE_SOURCE_OVERSIZE,
            raw_bytes: observed_bytes,
            source_identity: [0; 32],
            redacted_digest: [0; 32],
            transcript_redacted: Vec::new(),
            redaction_report: Vec::new(),
        },
        StrictBoundedRead::Complete(bytes) => redact_live_claude_source(bytes, cap),
        StrictBoundedRead::Failed { bytes_read, .. } => {
            LiveClaudeHelperResponse::read_error(bytes_read)
        }
    }
}

fn redact_live_claude_source(bytes: Vec<u8>, requested_cap: u64) -> LiveClaudeHelperResponse {
    let raw_bytes = bytes.len() as u64;
    let max_output_bytes =
        usize::try_from(capture_redacted_output_cap(raw_bytes)).unwrap_or(usize::MAX);
    let max_working_set_bytes =
        usize::try_from(capture_redaction_working_set_cap(raw_bytes)).unwrap_or(usize::MAX);
    let Some((transcript_redacted, redaction_report)) = Redactor::new_default()
        .redact_owned_bounded(bytes, max_output_bytes, max_working_set_bytes)
    else {
        return LiveClaudeHelperResponse::read_error(raw_bytes);
    };
    let transcript_redacted = transcript_redacted.into_inner();
    let redacted_digest: [u8; 32] = Sha256::digest(&transcript_redacted).into();
    let max_redacted = capture_redacted_output_cap(requested_cap);
    if transcript_redacted.len() as u64 > max_redacted {
        return LiveClaudeHelperResponse::read_error(raw_bytes);
    }
    let Ok(redaction_report) = serde_json::to_vec(&redaction_report) else {
        return LiveClaudeHelperResponse::read_error(raw_bytes);
    };
    if redaction_report.len() > LIVE_CLAUDE_REPORT_CAP {
        return LiveClaudeHelperResponse::read_error(raw_bytes);
    }
    LiveClaudeHelperResponse {
        status: LIVE_SOURCE_COMPLETE,
        raw_bytes,
        source_identity: [0; 32],
        redacted_digest,
        transcript_redacted,
        redaction_report,
    }
}

/// Maximum redacted output accepted for one bounded source.  It is shared
/// with durable snapshot validation so a valid expanding placeholder is not
/// rejected after the helper has already admitted it.
pub(crate) fn capture_redacted_output_cap(raw_bytes: u64) -> u64 {
    ceil_scaled(
        raw_bytes,
        LIVE_CLAUDE_REDACTED_NUMERATOR,
        LIVE_CLAUDE_REDACTED_DENOMINATOR,
    )
}

/// Maximum two-buffer working set for a redaction of one bounded source.
/// This is shared by synchronous authorized capture so the historical-import
/// path cannot produce a snapshot that the durable catalog later rejects.
pub(crate) fn capture_redaction_working_set_cap(raw_bytes: u64) -> u64 {
    ceil_scaled(
        raw_bytes,
        LIVE_CLAUDE_WORKING_SET_NUMERATOR,
        LIVE_CLAUDE_WORKING_SET_DENOMINATOR,
    )
}

fn ceil_scaled(value: u64, numerator: u64, denominator: u64) -> u64 {
    value
        .saturating_mul(numerator)
        .saturating_add(denominator.saturating_sub(1))
        / denominator
}

#[cfg(unix)]
fn rewind_file(file: &std::fs::File) -> bool {
    use std::os::fd::AsRawFd;

    // SAFETY: `file` is the helper-owned, descriptor-pinned regular source.
    // Seeking occurs only in this killable child, never in the hook parent.
    unsafe { libc::lseek(file.as_raw_fd(), 0, libc::SEEK_SET) >= 0 }
}

#[cfg(windows)]
fn rewind_file(mut file: &std::fs::File) -> bool {
    use std::io::Seek;
    file.rewind().is_ok()
}

fn write_live_claude_frame(response: LiveClaudeHelperResponse) -> bool {
    let Ok(redacted_len) = u64::try_from(response.transcript_redacted.len()) else {
        return false;
    };
    let Ok(report_len) = u32::try_from(response.redaction_report.len()) else {
        return false;
    };
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&[response.status]).is_ok()
        && stdout.write_all(&response.raw_bytes.to_le_bytes()).is_ok()
        && stdout.write_all(&response.source_identity).is_ok()
        && stdout.write_all(&response.redacted_digest).is_ok()
        && stdout.write_all(&redacted_len.to_le_bytes()).is_ok()
        && stdout.write_all(&report_len.to_le_bytes()).is_ok()
        && stdout.write_all(&response.transcript_redacted).is_ok()
        && stdout.write_all(&response.redaction_report).is_ok()
        && stdout.flush().is_ok()
}

pub(crate) fn provider_file_identity(_source_id: &str) -> String {
    SOURCE_IDENTITY_NOT_RETAINED.to_string()
}

#[cfg(test)]
#[cfg(feature = "test-live-agent")]
mod codex_index_source_tests {
    use super::*;

    fn control(cwd: &str) -> CodexSourceControl {
        CodexSourceControl {
            mode: CODEX_SOURCE_MODE.into(),
            session_id: "scope-session".into(),
            working_dir: cwd.into(),
            remaining_ms: 1000,
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_index_scope_requires_exact_subdirectory_and_valid_first_header() {
        let body = |cwd: &str| {
            serde_json::to_vec(&serde_json::json!({"type":"session_meta", "payload":{"id":"scope-session", "cwd":cwd}})).unwrap()
        };
        let cwd = if cfg!(windows) {
            "C:/work/repo/sub"
        } else {
            "/work/repo/sub"
        };
        let parent = if cfg!(windows) {
            "C:/work/repo"
        } else {
            "/work/repo"
        };
        assert!(codex_header_matches(&body(cwd), &control(cwd)));
        assert!(!codex_header_matches(&body(parent), &control(cwd)));
        assert!(!codex_header_matches(&body(cwd), &control(parent)));
        assert!(!codex_header_matches(
            b"{\"type\":\"session_meta\",\"type\":\"session_meta\"}",
            &control(cwd)
        ));
        let mut late = b"{\"type\":\"event_msg\"}\n".to_vec();
        late.extend_from_slice(&body(cwd));
        assert!(!codex_header_matches(&late, &control(cwd)));
        let mut truncated = body(cwd);
        truncated.pop();
        assert!(!codex_header_matches(&truncated, &control(cwd)));
        assert!(!codex_header_matches(
            &vec![b' '; 1024 * 1024 + 1],
            &control(cwd)
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_index_scope_accepts_only_known_system_aliases() {
        for (alias, canonical) in [
            ("/tmp/repo/sub", "/private/tmp/repo/sub"),
            ("/var/repo/sub", "/private/var/repo/sub"),
            ("/etc/repo/sub", "/private/etc/repo/sub"),
        ] {
            assert_eq!(
                normalized_codex_cwd(Path::new(alias)),
                normalized_codex_cwd(Path::new(canonical))
            );
            assert_eq!(
                normalized_codex_cwd(Path::new(&format!("{alias}/./"))),
                normalized_codex_cwd(Path::new(canonical))
            );
        }
        assert!(normalized_codex_cwd(Path::new("/tmp/repo/../other")).is_none());
        assert!(normalized_codex_cwd(Path::new("repo/sub")).is_none());
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_index_no_runtime_drop_keeps_resource_until_termination_is_proven() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct Resource(Arc<AtomicUsize>);
        impl Drop for Resource {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        for shutdown_in_context in [false, true] {
            let dropped = Arc::new(AtomicUsize::new(0));
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (pid, guard) = runtime.block_on(async {
                let mut command = tokio::process::Command::new("/bin/sh");
                command.args(["-c", "exec sleep 30"]).kill_on_drop(true);
                configure_private_helper_process_group(&mut command);
                let mut guard = CancellationSafeChild::new_process_group(command.spawn().unwrap());
                let pid = guard.child_mut().unwrap().id().unwrap() as i32;
                guard.retained_until_reaped = Some(Box::new(Resource(dropped.clone())));
                if shutdown_in_context {
                    // Enqueue the reaper without yielding. Runtime shutdown then
                    // cancels it unpolled: its scratch guard must retain ownership.
                    drop(guard);
                    (pid, None)
                } else {
                    (pid, Some(guard))
                }
            });
            drop(runtime);
            drop(guard); // Also covers dropping a child with no runtime context.
            assert_eq!(
                dropped.load(Ordering::SeqCst),
                0,
                "context={shutdown_in_context}"
            );
            let end = Instant::now() + std::time::Duration::from_secs(2);
            loop {
                let mut status = 0;
                // SAFETY: this test owns this direct child; WNOHANG never blocks.
                let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
                if result == pid
                    || (result == -1
                        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD))
                {
                    break;
                }
                assert!(
                    Instant::now() < end,
                    "no-runtime drop did not terminate child"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(windows, allow(dead_code))]
pub(crate) async fn with_test_helper_program<F>(program: PathBuf, future: F) -> F::Output
where
    F: std::future::Future,
{
    TEST_AUTHORIZED_READ_HELPER_PROGRAM
        .scope(Some(program), future)
        .await
}

/// Test-only embedded-host seam.  A scoped `None` must override any global
/// program registration so callers can prove that an unrelated host never
/// falls back to `current_exe` for a private helper argument.
#[cfg(test)]
pub(crate) async fn with_no_test_helper_program<F>(future: F) -> F::Output
where
    F: std::future::Future,
{
    TEST_AUTHORIZED_READ_HELPER_PROGRAM
        .scope(None, future)
        .await
}

#[cfg(test)]
mod tests {
    use std::{io::Cursor, path::Path};
    #[cfg(unix)]
    use std::{
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };

    use regex::bytes::Regex;
    use sha2::{Digest, Sha256};

    #[cfg(unix)]
    use super::run_registered_bounded_helper_until;
    use super::{
        AUTHORIZED_READ_HELPER_MAX_CAP, LIVE_SOURCE_COMPLETE, LIVE_SOURCE_OVERSIZE,
        LiveClaudeSourceRead, SOURCE_IDENTITY_NOT_RETAINED, StrictBoundedRead,
        capture_redacted_output_cap, decode_live_claude_frame, helper_program_from_registered,
        provider_file_identity, read_strictly_bounded, redact_live_claude_source,
        valid_helper_redaction_report,
    };
    use crate::internal::ai::observed_agents::{RedactionRule, Redactor};

    fn complete_frame(redacted: &[u8]) -> Vec<u8> {
        let report = serde_json::to_vec(&crate::internal::ai::observed_agents::RedactionReport {
            bytes_scanned: redacted.len(),
            ..Default::default()
        })
        .expect("serialize bounded test report");
        let digest: [u8; 32] = Sha256::digest(redacted).into();
        let mut frame = Vec::new();
        frame.push(LIVE_SOURCE_COMPLETE);
        frame.extend_from_slice(&(redacted.len() as u64).to_le_bytes());
        frame.extend_from_slice(&[0; 32]);
        frame.extend_from_slice(&digest);
        frame.extend_from_slice(&(redacted.len() as u64).to_le_bytes());
        frame.extend_from_slice(&(report.len() as u32).to_le_bytes());
        frame.extend_from_slice(redacted);
        frame.extend_from_slice(&report);
        frame
    }

    fn oversize_frame(raw_bytes: u64) -> Vec<u8> {
        let mut frame = Vec::new();
        frame.push(LIVE_SOURCE_OVERSIZE);
        frame.extend_from_slice(&raw_bytes.to_le_bytes());
        // A future helper may retain a source-local proof internally, but
        // decoding must never expose it as durable metadata.
        frame.extend_from_slice(&[0; 32]);
        frame.extend_from_slice(&[0; 32]);
        frame.extend_from_slice(&0u64.to_le_bytes());
        frame.extend_from_slice(&0u32.to_le_bytes());
        frame
    }

    #[cfg(unix)]
    fn cov2_real_checkpoint_helper() -> std::path::PathBuf {
        let program = crate::utils::test::real_libra_helper_binary();
        assert!(
            program.is_file(),
            "build the same-source Libra helper before this test"
        );
        program
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_explicit_checkpoint_helper_scope_is_narrow_and_task_local() {
        let program = cov2_real_checkpoint_helper();
        let argument = crate::internal::ai::history::CHECKPOINT_OBJECT_IO_HELPER_ARG;
        super::with_no_test_helper_program(async {
            super::with_explicit_import_checkpoint_helper(
                Some(&program),
                Instant::now() + Duration::from_secs(10),
                async {
                    assert_eq!(super::helper_program(), None);
                    let selected = super::registered_helper_command(argument).unwrap();
                    assert_eq!(selected.as_std().get_program(), program.as_os_str());
                    assert!(
                        super::registered_helper_command(super::AUTHORIZED_READ_HELPER_ARG)
                            .is_none()
                    );
                    assert!(
                        super::registered_helper_command(
                            crate::internal::ai::history::REJECTED_CLEANUP_INDEX_HELPER_ARG
                        )
                        .is_none()
                    );
                    tokio::spawn(async move {
                        super::with_no_test_helper_program(async move {
                            assert!(
                                super::registered_helper_command(argument).is_none(),
                                "other tasks must not inherit executable authority"
                            );
                        })
                        .await;
                    })
                    .await
                    .unwrap();
                    Ok(())
                },
            )
            .await
            .unwrap();
            assert!(super::registered_helper_command(argument).is_none());
        })
        .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_explicit_checkpoint_helper_restores_after_error_and_cancel() {
        let program = cov2_real_checkpoint_helper();
        let argument = crate::internal::ai::history::CHECKPOINT_OBJECT_IO_HELPER_ARG;
        super::with_no_test_helper_program(async {
            let error: anyhow::Result<()> = super::with_explicit_import_checkpoint_helper(Some(&program), Instant::now()+Duration::from_secs(10), async {
                assert!(super::registered_helper_command(argument).is_some());
                anyhow::bail!("synthetic scoped caller failure")
            }).await;
            assert!(error.is_err());
            assert!(super::registered_helper_command(argument).is_none());
            let entered = std::sync::Arc::new(tokio::sync::Notify::new());
            let notify = entered.clone();
            let mut future = Box::pin(super::with_explicit_import_checkpoint_helper(Some(&program), Instant::now()+Duration::from_secs(10), async move {
                assert!(super::registered_helper_command(argument).is_some());
                notify.notify_one();
                std::future::pending::<anyhow::Result<()>>().await
            }));
            tokio::select! {
                _ = entered.notified() => {}
                result = &mut future => panic!("scope must reach its pending operation: {result:?}"),
            }
            drop(future);
            assert!(super::registered_helper_command(argument).is_none());
        }).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_explicit_checkpoint_helper_rejects_before_polling_import() {
        let tmp = tempfile::tempdir().unwrap();
        let absent = tmp.path().join("absent-libra");
        let non_executable = tmp.path().join("not-executable");
        std::fs::write(&non_executable, b"ordinary fixture, never executed").unwrap();
        std::fs::set_permissions(&non_executable, std::fs::Permissions::from_mode(0o600)).unwrap();
        for program in [
            None,
            Some(Path::new("relative-libra")),
            Some(absent.as_path()),
            Some(non_executable.as_path()),
        ] {
            let polled = std::sync::atomic::AtomicBool::new(false);
            let result =
                super::with_no_test_helper_program(super::with_explicit_import_checkpoint_helper(
                    program,
                    Instant::now() + Duration::from_secs(10),
                    async {
                        polled.store(true, std::sync::atomic::Ordering::SeqCst);
                        Ok(())
                    },
                ))
                .await;
            assert!(result.is_err());
            assert!(!polled.load(std::sync::atomic::Ordering::SeqCst));
        }
    }

    #[test]
    fn registered_renamed_program_wins_over_fallback_resolution() {
        let renamed = Path::new("/opt/libra/bin/capture-hook-custom-name");
        assert_eq!(
            helper_program_from_registered(Some(renamed)),
            Some(renamed.to_path_buf())
        );
    }

    #[test]
    fn unregistered_host_has_no_helper_program() {
        assert_eq!(helper_program_from_registered(None), None);
    }

    #[test]
    fn helper_redaction_handles_short_expanding_secret_within_shared_cap() {
        let raw = b"AKIAIOSFODNN7EXAMPLE".to_vec();
        let response = redact_live_claude_source(raw.clone(), raw.len() as u64);

        assert_eq!(response.status, LIVE_SOURCE_COMPLETE);
        assert!(
            response.transcript_redacted.len() as u64
                <= capture_redacted_output_cap(raw.len() as u64)
        );
        assert!(
            !String::from_utf8_lossy(&response.transcript_redacted)
                .contains("AKIAIOSFODNN7EXAMPLE")
        );
    }

    #[test]
    fn strict_reader_redacts_an_exact_cap_source_without_capacity_growth() {
        const SECRET: &[u8] = b"ghp_abcdefghijklmnopqrstuvwxyz0123456789AB";
        let cap =
            usize::try_from(AUTHORIZED_READ_HELPER_MAX_CAP).expect("test helper cap fits usize");
        let mut source = Vec::new();
        source
            .try_reserve_exact(cap)
            .expect("allocate exact-cap source fixture");
        source.resize(cap, b'x');
        source[..SECRET.len()].copy_from_slice(SECRET);
        source[SECRET.len()] = b'\n';

        let mut reader = Cursor::new(source);
        let StrictBoundedRead::Complete(raw) =
            read_strictly_bounded(&mut reader, AUTHORIZED_READ_HELPER_MAX_CAP)
        else {
            panic!("an exact-cap source must be accepted without a heap sentinel");
        };
        drop(reader);
        assert_eq!(raw.len(), cap);
        assert_eq!(
            raw.capacity(),
            cap,
            "the strict reader must not retain geometric or EOF-probe slack"
        );

        let response = redact_live_claude_source(raw, AUTHORIZED_READ_HELPER_MAX_CAP);
        assert_eq!(response.status, LIVE_SOURCE_COMPLETE);
        assert_eq!(response.raw_bytes, AUTHORIZED_READ_HELPER_MAX_CAP);
        assert!(
            !response
                .transcript_redacted
                .windows(SECRET.len())
                .any(|window| window == SECRET),
            "an exact-cap source must remain redacted rather than becoming SourceReadError"
        );
    }

    #[test]
    fn helper_report_accepts_evolving_offsets_and_cumulative_lengths() {
        const EXPANDING_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let mut input = b"x".to_vec();
        input.extend(std::iter::repeat_n(b'q', 20));
        input.push(b'z');
        let redactor = Redactor::with_rules(vec![
            RedactionRule {
                id: EXPANDING_ID,
                regex: Regex::new("x").expect("compile first expansion rule"),
                replacement: "unused",
            },
            RedactionRule {
                id: "shifted-z",
                regex: Regex::new("z").expect("compile shifted offset rule"),
                replacement: "unused",
            },
            RedactionRule {
                id: "placeholder-id-content",
                regex: Regex::new("a+").expect("compile cumulative count rule"),
                replacement: "unused",
            },
        ]);

        let (redacted, report) = redactor.redact(&input);
        assert!(
            report.matches[1].end > report.bytes_scanned,
            "later rules record coordinates in the evolving buffer"
        );
        assert!(
            report.bytes_redacted > report.bytes_scanned,
            "cumulative metrics may include distinct rule-pass spans"
        );
        assert!(valid_helper_redaction_report(
            &report,
            input.len() as u64,
            128,
            redacted.len(),
        ));
    }

    #[test]
    fn decoder_rejects_a_mismatched_redacted_digest() {
        let redacted = b"safe-redacted-payload";
        let mut frame = complete_frame(redacted);
        frame[41..73].fill(0);

        assert!(matches!(
            decode_live_claude_frame(frame, 128),
            LiveClaudeSourceRead::Failed
        ));
    }

    #[test]
    fn decoder_reuses_the_bounded_stdout_allocation_for_redacted_bytes() {
        let frame = complete_frame(b"safe-redacted-payload");
        let allocation = frame.as_ptr();

        let LiveClaudeSourceRead::Complete {
            transcript_redacted,
            ..
        } = decode_live_claude_frame(frame, 128)
        else {
            panic!("valid bounded helper frame must decode");
        };
        assert_eq!(
            transcript_redacted.bytes().as_ptr(),
            allocation,
            "decoder must move the stdout allocation instead of cloning redacted payload bytes"
        );
    }

    #[test]
    fn helper_complete_and_oversize_results_use_a_noncorrelating_identity() {
        let LiveClaudeSourceRead::Complete { .. } =
            decode_live_claude_frame(complete_frame(b"safe"), 128)
        else {
            panic!("complete helper frame must decode");
        };

        let LiveClaudeSourceRead::Oversize = decode_live_claude_frame(oversize_frame(129), 128)
        else {
            panic!("oversize helper frame must decode");
        };
        assert_eq!(
            provider_file_identity("provider-relative/session-secret.jsonl"),
            SOURCE_IDENTITY_NOT_RETAINED,
            "all helper-complete/oversize source projections use the fixed sentinel"
        );
    }

    /// An outer hook deadline drops this future before the helper's own
    /// absolute deadline. The cancellation guard must still start kill and
    /// retain a reaper instead of relying only on Tokio's `kill_on_drop`.
    #[cfg(unix)]
    #[tokio::test]
    async fn registered_helper_outer_deadline_cancels_and_reaps_child() {
        let directory = tempfile::tempdir().expect("tempdir");
        let helper = directory
            .path()
            .join("outer-timeout-authorized-read-helper");
        let pid_file = directory.path().join("outer-timeout-authorized-read.pid");
        let pid_path = pid_file.to_string_lossy().replace('\'', "'\"'\"'");
        std::fs::write(
            &helper,
            format!("#!/bin/sh\nprintf '%s\\n' \"$$\" > '{pid_path}'\nexec /bin/sleep 30\n"),
        )
        .expect("write stalled helper");
        let mut permissions = std::fs::metadata(&helper)
            .expect("read helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions).expect("mark helper executable");

        let (result, pid) = {
            let capture = run_registered_bounded_helper_until(
                tokio::process::Command::new(&helper),
                &[b"bounded-request"],
                1024,
                Instant::now() + Duration::from_secs(20),
            );
            tokio::pin!(capture);
            let pid_wait = async {
                for _ in 0..500 {
                    if let Ok(value) = std::fs::read_to_string(&pid_file)
                        && let Ok(pid) = value.trim().parse::<libc::pid_t>()
                    {
                        return Some(pid);
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                None
            };
            tokio::pin!(pid_wait);
            let pid = tokio::select! {
                _ = &mut capture => panic!("stalled authorized-read helper completed before outer cancellation"),
                pid = &mut pid_wait => {
                    pid.expect("stalled authorized-read helper must start before outer cancellation")
                }
            };
            let result = tokio::time::timeout_at(
                tokio::time::Instant::now() + Duration::from_millis(750),
                &mut capture,
            )
            .await;
            (result, pid)
        };
        assert!(
            result.is_err(),
            "the outer deadline must cancel the still-live authorized-read helper"
        );
        let reaped = async {
            for _ in 0..200 {
                // SAFETY: signal zero probes the exact test child PID and
                // never delivers a signal.
                if unsafe { libc::kill(pid, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            false
        }
        .await;
        assert!(
            reaped,
            "outer-cancelled authorized-read helper was not reaped"
        );
    }

    /// The bounded runner must start its stdout drain before writing stdin.
    /// This fixture fills stdout before it reads the deliberately pipe-sized
    /// request. If the parent writes first, both sides can block forever.
    #[cfg(unix)]
    #[tokio::test]
    async fn registered_helper_drains_stdout_before_a_pre_stdin_oversize_write() {
        let directory = tempfile::tempdir().expect("create helper tempdir");
        let helper = directory.path().join("pre-stdin-stdout-helper.sh");
        std::fs::write(
            &helper,
            "#!/bin/sh\ndd if=/dev/zero bs=65536 count=2 2>/dev/null\ncat >/dev/null\n",
        )
        .expect("write output-first helper");
        let mut permissions = std::fs::metadata(&helper)
            .expect("read helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions).expect("mark helper executable");

        let request = vec![b'x'; 256 * 1024];
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            run_registered_bounded_helper_until(
                tokio::process::Command::new(&helper),
                &[&request],
                4,
                Instant::now() + Duration::from_secs(10),
            ),
        )
        .await
        .expect("stdout-first helper must not deadlock request write");
        assert!(matches!(result, super::RegisteredHelperOutput::Failed));
    }

    /// The direct leader exits while its forked child retains inherited stdin
    /// and stdout. Outer cancellation must kill the owned group before the
    /// unreaped leader PID can be recycled.
    #[cfg(unix)]
    #[tokio::test]
    async fn registered_helper_outer_cancellation_kills_descendant_holding_pipes() {
        let directory = tempfile::tempdir().expect("create helper tempdir");
        let helper = directory.path().join("forking-authorized-read-helper.sh");
        let descendant_file = directory.path().join("authorized-read-descendant.pid");
        let descendant_path = descendant_file.to_string_lossy().replace('\'', "'\"'\"'");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\n/bin/sleep 60 &\nprintf '%s\\n' \"$!\" > '{descendant_path}'\nexit 0\n"
            ),
        )
        .expect("write forking helper");
        let mut permissions = std::fs::metadata(&helper)
            .expect("read helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions).expect("mark helper executable");

        let (result, descendant) = {
            let capture = run_registered_bounded_helper_until(
                tokio::process::Command::new(&helper),
                &[b"bounded-request"],
                1024,
                Instant::now() + Duration::from_secs(20),
            );
            tokio::pin!(capture);
            let descendant_wait = async {
                for _ in 0..500 {
                    if let Ok(value) = std::fs::read_to_string(&descendant_file)
                        && let Ok(pid) = value.trim().parse::<libc::pid_t>()
                    {
                        return Some(pid);
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                None
            };
            tokio::pin!(descendant_wait);
            let descendant = tokio::select! {
                _ = &mut capture => panic!("pipe-holding authorized-read helper completed before outer cancellation"),
                pid = &mut descendant_wait => {
                    pid.expect("pipe-holding authorized-read descendant must start before outer cancellation")
                }
            };
            let result = tokio::time::timeout_at(
                tokio::time::Instant::now() + Duration::from_millis(750),
                &mut capture,
            )
            .await;
            (result, descendant)
        };
        assert!(
            result.is_err(),
            "outer cancellation must interrupt the pipe-holding descendant path"
        );
        let reaped = async {
            for _ in 0..200 {
                // SAFETY: signal zero only probes the exact fixture PID.
                if unsafe { libc::kill(descendant, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            false
        }
        .await;
        assert!(reaped, "outer cancellation left a helper descendant alive");
    }
}
