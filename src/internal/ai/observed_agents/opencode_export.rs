//! OpenCode `export` subprocess bridge (plan-20260713 DR-04b, GC-DR-04).
//!
//! OpenCode has no on-disk transcript to read — content only exists via
//! `opencode export <sessionID>`. This module runs that subprocess under the
//! capture trust model and returns the raw bytes for the seam:
//!
//! - **Binary trust**: the `opencode` binary must have been explicitly
//!   trusted (`libra agent rpc trust`-style record: absolute path + sha256 +
//!   device/inode/mtime); [`trusted_opencode_binary`] revalidates and fails
//!   CLOSED (capability unavailable) on drift or absence — never a PATH
//!   lookup, never an untrusted spawn.
//! - **Structured argv**: `[<binary>, "export", <session-id>]` — no shell,
//!   no `sh -c`, session id charset-validated before spawn.
//! - **Environment**: `env_clear()` plus fixed, private in-sandbox `HOME`
//!   and XDG roots. The parent may inspect its ambient XDG/HOME only to pin
//!   the literal OpenCode store directory by descriptor; no ambient home or
//!   XDG pathname crosses into the exporter environment or mount table.
//! - **Bounds** (GC-DR-04): the child's stdout is an inherited anonymous FILE
//!   (probe-verified: the CLI truncates large exports into backpressured pipes
//!   while exiting success). The `max_bytes` cap is enforced by actively
//!   polling that file while the child runs and re-checking after exit;
//!   over-cap always kills and errors, never returns truncated content.
//!   `RLIMIT_FSIZE` backs this with a write-time bound — strict
//!   (`max_bytes + 1`) on Linux (GC-SBX-01 pre-plan behavior, isolated
//!   tmpfs scratch), a coarse 8 GiB disk backstop on macOS where the
//!   process-wide limit would SIGXFSZ OpenCode's SQLite WAL checkpoint on
//!   a large store (FIX-SBX-01). The whole run sits under a wall-clock
//!   deadline (default 3 s — expiry kills the still-owned child's process
//!   group). On Linux the outer bwrap process is detached from the invoking
//!   terminal in `pre_exec` with `setsid()`: null stdin alone does not prevent
//!   `/dev/tty` access, and the new session also gives its PID a fresh process
//!   group for cancellation. stderr is capped and drained solely to prevent a
//!   blocked child;
//!   it never appears in errors or telemetry (GC-DR-13). After the direct
//!   child has been reaped, Libra never probes or signals its former PGID:
//!   Linux production relies on bwrap's PID namespace to contain descendants,
//!   avoiding a PID/PGID-reuse kill race. On Unix both core
//!   limits are zero in the child and its descendants, and `SIGXFSZ` is set
//!   to `SIG_IGN` so the `RLIMIT_FSIZE` write-time bound fails over-cap
//!   writes with `EFBIG` instead of terminating the child (no core file, no
//!   "abnormal termination" journal noise); system handlers that honor
//!   `RLIMIT_CORE` still suppress core files from other unexpected crashes.
//!
//! Sandbox: the Required offline profile lives in
//! [`run_export_subprocess_sandboxed`] — assembled via
//! `SandboxManager::transform`. Linux: network unshared, a private tmpfs
//! `/tmp` environment, and ONE probe-verified exception: a descriptor-pinned
//! OpenCode data directory is bound read-write at a fixed in-sandbox path
//! because its WAL-mode SQLite store needs write access even for reads. macOS OpenCode content export is
//! unsupported: seatbelt cannot prove cancellation-safe containment against
//! a forking exporter, so the bridge fails closed before spawning it and hook
//! capture remains metadata-only. There is never an unsandboxed fallback.

use std::{
    path::{Component, Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};

#[cfg(any(target_os = "linux", test))]
use crate::internal::ai::authorized_read::{
    CancellationSafeChild, StrictBoundedRead, read_async_strictly_bounded, read_strictly_bounded,
};
use crate::internal::ai::observed_agents::{
    TranscriptSource,
    transcript_source::ExportAuthorized,
    trust::{OPENCODE_EXPORTER_SLUG, read_trust, revalidate_trust},
};

/// Trust-record slug for the OpenCode exporter binary (shared with the
/// `agent rpc trust` provider-exporter registration path, DR-04b).
const OPENCODE_TRUST_SLUG: &str = OPENCODE_EXPORTER_SLUG;
/// Default stdout byte cap (GC-DR-04 Bytes/export cap).
pub const EXPORT_MAX_BYTES: u64 = 16 * 1024 * 1024;
/// Default subprocess wall-clock deadline (GC-DR-04: ≤3 s, leaving
/// parse/redact/claim headroom inside the hook ceiling).
pub const EXPORT_DEADLINE: Duration = Duration::from_secs(3);
/// Fixed exporter-private paths below the bwrap-provided tmpfs. Ambient HOME
/// and XDG spelling must never be inherited into this namespace: the only
/// host state OpenCode may receive is the separately pinned store descriptor.
#[cfg(any(target_os = "linux", test))]
const OPENCODE_SANDBOX_ROOT: &str = "/tmp/libra-opencode";
#[cfg(any(target_os = "linux", test))]
const OPENCODE_SANDBOX_HOME: &str = "/tmp/libra-opencode/home";
#[cfg(any(target_os = "linux", test))]
const OPENCODE_SANDBOX_DATA_HOME: &str = "/tmp/libra-opencode/data";
#[cfg(any(target_os = "linux", test))]
const OPENCODE_SANDBOX_CONFIG_HOME: &str = "/tmp/libra-opencode/config";
#[cfg(any(target_os = "linux", test))]
const OPENCODE_SANDBOX_BIN_DIR: &str = "/tmp/libra-opencode/bin";
#[cfg(any(target_os = "linux", test))]
const OPENCODE_SANDBOX_EXPORTER: &str = "/tmp/libra-opencode/bin/opencode";
#[cfg(any(target_os = "linux", test))]
const OPENCODE_SANDBOX_STORE: &str = "/tmp/libra-opencode/data/opencode";
/// The host-side bwrap process must not inherit an arbitrary repository cwd.
/// The bwrap argv has its own fixed private filesystem layout; `/usr` is the
/// narrow, immutable host-runtime cwd used while it constructs that layout.
#[cfg(any(target_os = "linux", test))]
const OPENCODE_SANDBOX_OUTER_CWD: &str = "/usr";
/// stderr drain cap — enough to keep an exporter from blocking on a full pipe.
/// The captured bytes never leave this subprocess boundary.
#[cfg(any(target_os = "linux", test))]
const EXPORT_MAX_STDERR_BYTES: usize = 4 * 1024;
/// File-backed stdout must still be bounded while the child is running. A
/// short interval prevents a runaway trusted exporter from consuming disk for
/// the full subprocess deadline before the post-exit size check can run.
#[cfg(any(target_os = "linux", test))]
const EXPORT_SIZE_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Injectable bounds (GC-DR-07).
#[derive(Debug, Clone, Copy)]
pub struct ExportLimits {
    pub max_bytes: u64,
    pub deadline: Duration,
}

impl Default for ExportLimits {
    fn default() -> Self {
        Self {
            max_bytes: EXPORT_MAX_BYTES,
            deadline: EXPORT_DEADLINE,
        }
    }
}

/// Resolve the trusted OpenCode binary, revalidating its provenance
/// (sha256/device/inode/mtime + trusted-dir containment). Fail-closed:
/// no trust record → the capability is unavailable, with an actionable hint.
pub async fn trusted_opencode_binary() -> Result<PathBuf> {
    Ok(trusted_opencode_provenance().await?.canonical_path)
}

/// Revalidate the trust record immediately before sealing an executable
/// descriptor. Keeping the provenance separate lets the Linux exporter path
/// compare the bytes and stat identity of the opened fd rather than reopening
/// an already-authorized pathname at exec time.
async fn trusted_opencode_provenance()
-> Result<crate::internal::ai::observed_agents::trust::Provenance> {
    let record = read_trust(OPENCODE_TRUST_SLUG)
        .await
        .context("read opencode trust record")?;
    trusted_opencode_provenance_from(record).await
}

/// Injectable core of [`trusted_opencode_binary`] (GC-DR-07): the record
/// lookup is separated so the fail-closed no-record arm is unit-testable
/// without touching the process-wide config store (which may legitimately
/// trust opencode on a dev machine).
#[cfg(test)]
async fn trusted_opencode_binary_from(
    record: Option<crate::internal::ai::observed_agents::TrustRecord>,
) -> Result<PathBuf> {
    Ok(trusted_opencode_provenance_from(record)
        .await?
        .canonical_path)
}

async fn trusted_opencode_provenance_from(
    record: Option<crate::internal::ai::observed_agents::TrustRecord>,
) -> Result<crate::internal::ai::observed_agents::trust::Provenance> {
    let record = record.ok_or_else(|| {
        anyhow!(
            "the 'opencode' binary is not trusted for export; register its \
             directory with 'libra agent rpc trust --dir <path>' and then run \
             'libra agent rpc trust opencode' (after verifying the binary) to \
             enable the OpenCode export bridge"
        )
    })?;
    let provenance = revalidate_trust(OPENCODE_TRUST_SLUG, &record)
        .await
        .context("revalidate opencode binary trust")?;
    Ok(provenance)
}

#[cfg(any(target_os = "linux", test))]
fn valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 64
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Run the sandboxed export AND mint the digest-bound authorization in one
/// step — the ONLY constructor of an export-authorized byte source (ADR-DR-02
/// Bytes trust boundary). Before issuing that authorization, bind the export's
/// native session id and working directory to the already verified capture
/// scope. Callers receive an opaque [`TranscriptSource`] and must still
/// re-verify via `ExportAuthorized::matches` before use.
pub async fn authorized_sandboxed_export(
    binary: &Path,
    provider_session_id: &str,
    libra_session_id: &str,
    expected_working_dir: &Path,
    limits: ExportLimits,
) -> Result<TranscriptSource> {
    authorized_sandboxed_export_until(
        binary,
        provider_session_id,
        libra_session_id,
        expected_working_dir,
        limits,
        std::time::Instant::now() + limits.deadline,
    )
    .await
}

/// Deadline-aware form of [`authorized_sandboxed_export`]. The deadline is an
/// absolute capture boundary shared with trust revalidation, sealing, bwrap
/// capability probing and the exporter itself; it is never reset per phase.
pub async fn authorized_sandboxed_export_until(
    binary: &Path,
    provider_session_id: &str,
    libra_session_id: &str,
    expected_working_dir: &Path,
    limits: ExportLimits,
    deadline: std::time::Instant,
) -> Result<TranscriptSource> {
    let bytes =
        run_export_subprocess_sandboxed_until(binary, provider_session_id, limits, deadline)
            .await?;
    validate_opencode_export_identity(&bytes, provider_session_id, expected_working_dir)?;
    let auth = ExportAuthorized::issue("opencode", libra_session_id, &bytes);
    Ok(TranscriptSource::Bytes { bytes, auth })
}

/// Run the configured, already-recorded OpenCode exporter without a caller
/// reopening its pathname. Hook capture uses this form so its absolute deadline
/// begins before any trust revalidation or sealed-descriptor I/O.
pub async fn authorized_trusted_sandboxed_export_until(
    provider_session_id: &str,
    libra_session_id: &str,
    expected_working_dir: &Path,
    limits: ExportLimits,
    deadline: std::time::Instant,
) -> Result<TranscriptSource> {
    let bytes =
        run_trusted_export_subprocess_sandboxed_until(provider_session_id, limits, deadline)
            .await?;
    validate_opencode_export_identity(&bytes, provider_session_id, expected_working_dir)?;
    let auth = ExportAuthorized::issue("opencode", libra_session_id, &bytes);
    Ok(TranscriptSource::Bytes { bytes, auth })
}

/// Bind an OpenCode export's self-reported native identity to the already
/// verified hook/import scope before its bytes become an authorized source.
///
/// Exporter binary trust proves who produced the bytes, not *which* OpenCode
/// session or workspace they describe. A trusted but buggy exporter (or a
/// stale store selection) must therefore fail closed rather than let a
/// different session/workspace be persisted under this capture identity.
/// Errors deliberately omit exporter-supplied ids and paths because this
/// boundary executes before redaction.
fn validate_opencode_export_identity(
    bytes: &[u8],
    expected_session_id: &str,
    expected_working_dir: &Path,
) -> Result<()> {
    let document = crate::internal::ai::observed_agents::parse_canon_value(bytes).map_err(|_| {
        anyhow!(
            "OpenCode export identity is not a valid canonical document; rerun the export for the active session"
        )
    })?;
    let info = document.get("info").ok_or_else(|| {
        anyhow!(
            "OpenCode export is missing its native session identity; rerun the export for the active session"
        )
    })?;
    let native_session_id = info
        .get("id")
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            anyhow!(
                "OpenCode export is missing its native session identity; rerun the export for the active session"
            )
        })?;
    if native_session_id != expected_session_id {
        bail!(
            "OpenCode export session identity does not match the active capture session; rerun the export for the active session"
        );
    }

    let exported_working_dir = info
        .get("directory")
        .or_else(|| info.get("cwd"))
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            anyhow!(
                "OpenCode export is missing its working directory; rerun the export for the active workspace"
            )
        })?;
    let exported_path = Path::new(exported_working_dir);
    if !exported_path.is_absolute()
        || exported_path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!(
            "OpenCode export working directory is not a canonical absolute directory; rerun the export for the active workspace"
        );
    }

    let verified_path = expected_working_dir
        .canonicalize()
        .context("canonicalize verified OpenCode capture working directory")?;
    if !verified_path.is_dir() {
        bail!("verified OpenCode capture working directory is not a directory");
    }
    let exported_path = exported_path
        .canonicalize()
        .context("canonicalize OpenCode export working directory")?;
    if !exported_path.is_dir() {
        bail!(
            "OpenCode export working directory is not a directory; rerun the export for the active workspace"
        );
    }
    if exported_path != verified_path {
        bail!(
            "OpenCode export working directory does not match the active capture workspace; rerun the export for the active workspace"
        );
    }

    Ok(())
}

/// Kill the process group created for a still-owned exporter child.
///
/// This is intentionally called only before Tokio has reaped the direct
/// leader. Until then its PID/PGID cannot be recycled, so the negative-PID
/// signal cannot target an unrelated later process group. `ESRCH` means the
/// group is already absent; permission and all other failures are surfaced to
/// the caller so it cannot claim the exporter was terminated.
#[cfg(any(target_os = "linux", test))]
fn terminate_export_process_group(pgid: Option<u32>) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(pgid) = pgid.filter(|pid| *pid > 1) {
        // SAFETY: the caller still owns the unreaped process-group leader.
        // A negative PID targets that leader's group only.
        if unsafe { libc::kill(-(pgid as libc::pid_t), libc::SIGKILL) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = pgid;
    Ok(())
}

/// Cancellation-safe owner for an exporter and its stderr drainer.
///
/// Production OpenCode export is Linux-only: the required bwrap containment
/// starts in a fresh host session/process group from `pre_exec`, so outer
/// host-deadline cancellation kills that group before the generic direct-child
/// guard starts its detached reap.
#[cfg(any(target_os = "linux", test))]
struct ExporterCancellationGuard {
    process_group: Option<u32>,
    child: CancellationSafeChild,
}

#[cfg(any(target_os = "linux", test))]
impl ExporterCancellationGuard {
    fn new(child: tokio::process::Child, process_group: Option<u32>) -> Self {
        Self {
            process_group,
            child: CancellationSafeChild::new(child),
        }
    }

    fn child_mut(&mut self) -> Option<&mut tokio::process::Child> {
        self.child.child_mut()
    }

    fn register_abort_on_cancel<T>(&mut self, task: &tokio::task::JoinHandle<T>) {
        self.child.register_abort_on_cancel(task);
    }

    /// Returns false when a kill request could not be confirmed. In that
    /// case callers must reject output with a fixed safe error rather than
    /// claiming the exporter was killed or reaped.
    fn terminate_and_reap(&mut self) -> bool {
        let group_result = terminate_export_process_group(self.process_group);
        self.process_group = None;
        let child_result = self.child.terminate_and_reap_checked();
        group_result.is_ok() && child_result.is_ok()
    }

    fn disarm_child_after_wait(&mut self) {
        self.child.disarm_child_after_wait();
    }

    fn finish(&mut self) {
        self.process_group = None;
        self.child.finish();
    }
}

#[cfg(any(target_os = "linux", test))]
impl Drop for ExporterCancellationGuard {
    fn drop(&mut self) {
        // Run before `CancellationSafeChild::drop`: Linux descendants can
        // inherit stdout/stderr and otherwise outlive the direct process
        // leader. Drop cannot report an error to an outer-cancelled future,
        // so emit only a fixed reason if termination was not confirmed.
        if !self.terminate_and_reap() {
            tracing::warn!(
                target: "agent.hook.ingest",
                reason = "opencode_export_termination_unconfirmed",
                "could not confirm cancellation of OpenCode exporter; content was not accepted"
            );
        }
    }
}

/// Test-only raw runner for fixture coverage. Production must enter through
/// [`authorized_sandboxed_export`], which requires a containment backend;
/// an unsandboxed process group alone cannot safely contain a `setsid()`
/// escapee after the direct leader exits.
#[cfg(test)]
pub(crate) async fn run_export_subprocess(
    binary: &std::path::Path,
    session_id: &str,
    limits: ExportLimits,
) -> Result<Vec<u8>> {
    if !valid_session_id(session_id) {
        bail!("invalid OpenCode session id (expected alnum/dash/underscore, ≤64 chars)");
    }
    if !binary.is_absolute() {
        bail!("exporter binary path must be absolute (trusted provenance)");
    }

    let deadline = tokio::time::Instant::now() + limits.deadline;
    run_bounded_exporter(binary, &[], session_id, limits, deadline, Vec::new()).await
}

/// Fds the caller pinned that must stay open (and inheritable) until the
/// child has been spawned. File descriptors only exist on Unix; elsewhere the
/// alias is an uninhabited placeholder so the runner signature stays portable.
#[cfg(all(unix, any(target_os = "linux", test)))]
type PinnedFds = Vec<std::os::fd::OwnedFd>;
#[cfg(all(not(unix), any(target_os = "linux", test)))]
type PinnedFds = Vec<std::convert::Infallible>;

/// Fork-child-only descriptor boundary for the Linux bwrap invocation. Mark
/// every non-stdio inherited descriptor CLOEXEC in one raw syscall, then
/// clear that bit only for the exact setup capabilities bwrap must consume.
/// Callers must preallocate `keep_fds`; this helper performs no allocation,
/// path lookup, logging, or lock-taking after fork.
#[cfg(target_os = "linux")]
fn prepare_bwrap_capability_fds_for_exec(keep_fds: &[std::os::fd::RawFd]) -> std::io::Result<()> {
    // SAFETY: close_range and fcntl act only on the fork child's fd table;
    // the supplied raw fds are held open by the parent-owned PinnedFds until
    // spawn returns. This path is intentionally syscall-only.
    if unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3_u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    for fd in keep_fds {
        // SAFETY: fd is an exact capability owned by the spawning process;
        // fcntl only inspects/updates its descriptor flags in this child.
        let flags = unsafe { libc::fcntl(*fd, libc::F_GETFD) };
        if flags < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: see the F_GETFD call above; this retains only the setup
        // capabilities necessary for bwrap's descriptor-native mounts.
        if unsafe { libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Core bounded runner: `<program> [<pre_args>…] export <session_id>` with
/// the module's env/caps/deadline contract. `pre_args` lets the sandboxed
/// variant prepend the bwrap arg vector while keeping ONE code path for the
/// bounds (GC-DR-04).
#[cfg(any(target_os = "linux", test))]
async fn run_bounded_exporter(
    program: &std::path::Path,
    pre_args: &[String],
    session_id: &str,
    limits: ExportLimits,
    deadline_at: tokio::time::Instant,
    keep_fds: PinnedFds,
) -> Result<Vec<u8>> {
    if tokio::time::Instant::now() >= deadline_at {
        bail!("OpenCode export capture deadline already elapsed; refusing export");
    }
    // Fds pinned by the caller (the sealed exporter and optional RW store
    // bind) stay owned until spawn. They begin CLOEXEC; Linux `pre_exec`
    // applies CLOSE_RANGE_CLOEXEC to every inherited fd and then clears that
    // bit only for these exact capability numbers. This prevents unrelated
    // parent descriptors from bypassing the bwrap mount grammar via
    // `/proc/self/fd`, including descriptors accidentally left inheritable by
    // an embedding host.
    #[cfg(target_os = "linux")]
    let keep_fds_raw: Vec<std::os::fd::RawFd> = {
        use std::os::fd::AsRawFd;

        let raw: Vec<_> = keep_fds.iter().map(AsRawFd::as_raw_fd).collect();
        if raw.iter().any(|fd| *fd < 3) {
            bail!(
                "OpenCode sandbox capability descriptor occupied stdio; refusing export (fail-closed)"
            );
        }
        raw
    };
    let _keep_fds = keep_fds;
    // Probe-verified upstream hazard (opencode 1.17.x, 2026-07-14): the CLI
    // can exit BEFORE flushing stdout into a backpressured pipe — large
    // exports arrive truncated (~64 KiB) with a SUCCESS status. Give the
    // child an inherited anonymous FILE as stdout instead: file writes flush
    // synchronously (verified complete at 370 KiB+), the FD crosses the
    // sandbox's mount namespace untouched, and the byte cap is monitored
    // while the child runs as well as verified after exit.
    let stdout_file = tempfile::tempfile().context("create export stdout tempfile")?;
    let stdout_for_child = stdout_file
        .try_clone()
        .context("clone export stdout handle")?;
    let mut command = tokio::process::Command::new(program);
    // The export *stdout* byte cap is the tempfile poll below on every
    // platform. RLIMIT_FSIZE is per-OS: Linux keeps the strict write-time
    // cap (GC-SBX-01: pre-plan Linux semantics unchanged); macOS raises it
    // to a coarse disk backstop because the limit is process-wide and a
    // max_bytes cap SIGXFSZes OpenCode's WAL checkpoint on a store larger
    // than max_bytes (FIX-SBX-01: a ~1 GiB `opencode.db` failed
    // `PRAGMA wal_checkpoint` under 16 MiB).
    #[cfg(unix)]
    {
        #[cfg(target_os = "macos")]
        let fsize_limit: u64 = {
            const EXPORT_RLIMIT_FSIZE_BACKSTOP: u64 = 8 * 1024 * 1024 * 1024;
            EXPORT_RLIMIT_FSIZE_BACKSTOP.max(limits.max_bytes.saturating_add(1))
        };
        #[cfg(not(target_os = "macos"))]
        let fsize_limit: u64 = limits.max_bytes.saturating_add(1);
        unsafe {
            command.pre_exec(move || {
                #[cfg(target_os = "linux")]
                {
                    // Do not use a /proc scan here: forked-child code must
                    // stay async-signal-safe and allocation-free. A kernel
                    // without close_range cannot provide this boundary, so
                    // spawning fails closed.
                    prepare_bwrap_capability_fds_for_exec(&keep_fds_raw)?;
                }
                // `stdin(null)` does not detach a process from the invoking
                // terminal: an exporter could still open `/dev/tty` and use
                // TIOCSTI. Linux production therefore creates a new session
                // before exec. Do not pair this with an explicit process-group
                // setup: that would make the child a process-group leader and
                // make `setsid()` fail with EPERM. Its PID is the new PGID, so the
                // cancellation guard can still safely signal `-child.id()`
                // while the leader remains unreaped.
                #[cfg(target_os = "linux")]
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                let lim = libc::rlimit {
                    rlim_cur: fsize_limit,
                    rlim_max: fsize_limit,
                };
                if libc::setrlimit(libc::RLIMIT_FSIZE, &lim) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // `RLIMIT_FSIZE` over-run must not be a fatal signal: with the
                // default disposition the kernel SIGXFSZ-kills the child, and
                // system handlers (systemd-coredump) journal it as an
                // "abnormal termination" crash — with whole core files when the
                // handler predates RLIMIT_CORE=0. The byte cap is enforced by
                // the parent's size poll + post-exit recheck, so the child
                // dying adds nothing but noise. Ignoring SIGXFSZ turns each
                // over-cap write into `EFBIG` while the file still cannot grow
                // past `max_bytes + 1`: the cap stays hard, there is just no
                // crash to report. (Ignored dispositions persist across exec,
                // so descendants inherit it too.)
                if libc::signal(libc::SIGXFSZ, libc::SIG_IGN) == libc::SIG_ERR {
                    return Err(std::io::Error::last_os_error());
                }
                // Suppress exporter core files when the system handler honors
                // RLIMIT_CORE (including systemd-coredump). This covers
                // unexpected child crashes (SIGSEGV/SIGABRT etc.); SIGXFSZ is
                // ignored above and can no longer core.
                let core = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if libc::setrlimit(libc::RLIMIT_CORE, &core) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    command
        .args(pre_args)
        .arg("export")
        .arg(session_id)
        .env_clear()
        .current_dir(OPENCODE_SANDBOX_OUTER_CWD)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(stdout_for_child))
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // Never forward ambient HOME/XDG spelling or contents to the exporter.
    // On Linux `assemble_sandboxed_export` creates these exact directories in
    // its private `/tmp` tmpfs and, where present, mounts only the pinned
    // `opencode` store at `OPENCODE_SANDBOX_STORE`.
    command
        .env("HOME", OPENCODE_SANDBOX_HOME)
        .env("XDG_DATA_HOME", OPENCODE_SANDBOX_DATA_HOME)
        .env("XDG_CONFIG_HOME", OPENCODE_SANDBOX_CONFIG_HOME);

    // Assembly and tempfile setup are deliberately covered by the same
    // capture deadline as trust/sealing/probing. Check again immediately
    // before the irreversible spawn so an elapsed hook budget cannot launch
    // an exporter that no caller is still entitled to observe.
    if tokio::time::Instant::now() >= deadline_at {
        bail!("OpenCode export capture deadline already elapsed; refusing export");
    }
    let child = command.spawn().context("spawn opencode export")?;
    #[cfg(target_os = "linux")]
    let process_group = child.id();
    #[cfg(not(target_os = "linux"))]
    let process_group = None;
    let mut child = ExporterCancellationGuard::new(child, process_group);
    let Some(mut stderr) = child.child_mut().and_then(|child| child.stderr.take()) else {
        let terminated = child.terminate_and_reap();
        if !terminated {
            bail!("opencode export termination could not be confirmed; content skipped");
        }
        bail!("opencode export did not provide its required stderr pipe");
    };

    let mut stderr_reader = tokio::spawn(async move {
        read_async_strictly_bounded(&mut stderr, EXPORT_MAX_STDERR_BYTES as u64)
            .await
            .unwrap_or_default()
    });
    child.register_abort_on_cancel(&stderr_reader);

    enum WaitOutcome {
        Exited(std::io::Result<std::process::ExitStatus>),
        Deadline,
        OverCap(u64),
        SizeReadFailed,
    }

    let deadline = tokio::time::sleep_until(deadline_at);
    tokio::pin!(deadline);
    let mut size_poll = tokio::time::interval(EXPORT_SIZE_POLL_INTERVAL);
    size_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let outcome = loop {
        tokio::select! {
            status = async {
                match child.child_mut() {
                    Some(child) => child.wait().await,
                    None => Err(std::io::Error::other("opencode export child was unavailable")),
                }
            } => break WaitOutcome::Exited(status),
            _ = &mut deadline => break WaitOutcome::Deadline,
            _ = size_poll.tick() => {
                match stdout_file.metadata() {
                    Ok(metadata) if metadata.len() > limits.max_bytes => {
                        break WaitOutcome::OverCap(metadata.len());
                    }
                    Ok(_) => {}
                    Err(_) => break WaitOutcome::SizeReadFailed,
                }
            }
        }
    };

    let (_stderr_bytes, status) = match outcome {
        WaitOutcome::Exited(status) => {
            let status = match status {
                Ok(status) => status,
                Err(_) => {
                    let terminated = child.terminate_and_reap();
                    let _ = stderr_reader.await;
                    if !terminated {
                        bail!(
                            "opencode export termination could not be confirmed; content skipped"
                        );
                    }
                    bail!("could not reap opencode export; content skipped");
                }
            };
            // `wait` has consumed the direct child. Clear its group before
            // any more awaits: a post-wait `kill(-pgid, 0)`/kill sequence can
            // race PID/PGID reuse and terminate an unrelated host group.
            // Linux production containment proves no writer can survive this
            // point: bwrap owns a PID namespace. macOS OpenCode export is
            // rejected before this runner is reached.
            child.disarm_child_after_wait();
            child.process_group = None;
            let err_buf = tokio::select! {
                result = &mut stderr_reader => {
                    result.context("join opencode export stderr reader")?
                }
                _ = &mut deadline => {
                    stderr_reader.abort();
                    let _ = stderr_reader.await;
                    bail!(
                        "opencode export exceeded its deadline while finishing stderr; content skipped"
                    );
                }
            };
            child.finish();
            (err_buf, status)
        }
        WaitOutcome::Deadline => {
            // Deadline: terminate and fail closed — a slow exporter must not
            // eat the hook budget (GC-DR-04). Never claim the kill succeeded
            // when the OS rejected it.
            let terminated = child.terminate_and_reap();
            let _ = stderr_reader.await;
            if !terminated {
                bail!("opencode export termination could not be confirmed; content skipped");
            }
            bail!(
                "opencode export exceeded its {:?} deadline; content skipped this idle \
                 (a later idle retries)",
                limits.deadline
            );
        }
        WaitOutcome::OverCap(observed) => {
            let terminated = child.terminate_and_reap();
            let _ = stderr_reader.await;
            if !terminated {
                bail!("opencode export termination could not be confirmed; content skipped");
            }
            bail!(
                "opencode export exceeded the {} byte cap while running \
                 (observed {observed} bytes); content skipped",
                limits.max_bytes
            );
        }
        WaitOutcome::SizeReadFailed => {
            let terminated = child.terminate_and_reap();
            let _ = stderr_reader.await;
            if !terminated {
                bail!("opencode export termination could not be confirmed; content skipped");
            }
            bail!("could not monitor OpenCode export output size; content skipped");
        }
    };

    // Byte cap on the flushed file (GC-DR-04): over-cap errors, never a
    // silent truncation.
    let mut stdout_file = stdout_file;
    use std::io::{Seek as _, SeekFrom};
    stdout_file
        .seek(SeekFrom::Start(0))
        .context("rewind export output")?;
    // Bounded read + recheck on the bytes ACTUALLY read (Codex M3 R2 P1-1):
    // never trust a pre-measured size and never read unbounded into memory.
    // The only production caller is the Linux required sandboxed runner,
    // where bwrap supplies a PID namespace. macOS OpenCode export is
    // fail-closed before spawn; the raw runner is test-only. Reading at most
    // cap+1 bytes and rejecting overflow remains a second independent defense
    // against output growth, so content is never silently truncated. The
    // strict reader uses a stack sentinel for the final EOF probe, so an
    // exact-cap export cannot make its owned buffer over-reserve.
    let out = match read_strictly_bounded(&mut stdout_file, limits.max_bytes) {
        StrictBoundedRead::Complete(bytes) => bytes,
        StrictBoundedRead::Oversize { .. } => {
            bail!(
                "opencode export exceeded the {} byte cap; refusing content",
                limits.max_bytes
            );
        }
        StrictBoundedRead::Failed { error, .. } => {
            return Err(error).context("read export output file");
        }
    };
    if !status.success() {
        bail!("opencode export failed with a non-zero exit status; exporter diagnostics omitted");
    }
    Ok(out)
}

/// Run the export under the DR-04b Linux minimal offline sandbox profile
/// (`SandboxEnforcement::Required` semantics). Assembly is delegated to
/// [`crate::internal::ai::sandbox::SandboxManager::transform`]; execution
/// stays in `run_bounded_exporter` (file-backed stdout, `RLIMIT_FSIZE`,
/// `RLIMIT_CORE=0`, process group, wall clock, 16 MiB). Linux uses trusted
/// bwrap. macOS is explicitly unsupported because Seatbelt cannot guarantee
/// descendant containment after an outer hook cancellation; it fails closed
/// before spawn. No platform has a degraded unsandboxed run (GC-DR-14).
pub async fn run_export_subprocess_sandboxed(
    binary: &std::path::Path,
    session_id: &str,
    limits: ExportLimits,
) -> Result<Vec<u8>> {
    run_export_subprocess_sandboxed_until(
        binary,
        session_id,
        limits,
        std::time::Instant::now() + limits.deadline,
    )
    .await
}

/// Deadline-aware form of [`run_export_subprocess_sandboxed`]. `deadline`
/// must be the caller's absolute capture deadline, not a new per-export
/// duration: no revalidation, sealing, sandbox probe, or child spawn may run
/// once it has elapsed.
pub async fn run_export_subprocess_sandboxed_until(
    binary: &std::path::Path,
    session_id: &str,
    limits: ExportLimits,
    deadline: std::time::Instant,
) -> Result<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        let _ = (binary, session_id, limits, deadline);
        bail!(
            "OpenCode transcript export is unsupported on macOS because its seatbelt \
             backend cannot provide cancellation-safe descendant containment; refusing \
             export (fail-closed)"
        );
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (binary, session_id, limits, deadline);
        bail!(
            "the OpenCode export sandbox profile requires Linux bubblewrap or \
             macOS seatbelt; refusing an unsandboxed export (fail-closed, GC-DR-14)"
        );
    }
    #[cfg(target_os = "linux")]
    {
        if !valid_session_id(session_id) {
            bail!("invalid OpenCode session id (expected alnum/dash/underscore, ≤64 chars)");
        }
        let deadline = tokio::time::Instant::from_std(deadline);
        if tokio::time::Instant::now() >= deadline {
            bail!("OpenCode export capture deadline already elapsed; refusing export");
        }
        let exporter_fd = pin_revalidated_opencode_exporter_until(binary, deadline).await?;
        run_export_subprocess_sandboxed_with_exporter_fd(exporter_fd, session_id, limits, deadline)
            .await
    }
}

/// The configured-trust variant used by hook capture. It starts from the
/// persisted trust record and seals the verified bytes under the caller's
/// absolute deadline, avoiding a preliminary path-returning trust check that
/// could consume budget before the descriptor boundary begins.
async fn run_trusted_export_subprocess_sandboxed_until(
    session_id: &str,
    limits: ExportLimits,
    deadline: std::time::Instant,
) -> Result<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        let _ = (session_id, limits, deadline);
        bail!(
            "OpenCode transcript export is unsupported on macOS because its seatbelt \
             backend cannot provide cancellation-safe descendant containment; refusing \
             export (fail-closed)"
        );
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (session_id, limits, deadline);
        bail!(
            "the OpenCode export sandbox profile requires Linux bubblewrap or \
             macOS seatbelt; refusing an unsandboxed export (fail-closed, GC-DR-14)"
        );
    }
    #[cfg(target_os = "linux")]
    {
        if !valid_session_id(session_id) {
            bail!("invalid OpenCode session id (expected alnum/dash/underscore, ≤64 chars)");
        }
        let deadline = tokio::time::Instant::from_std(deadline);
        if tokio::time::Instant::now() >= deadline {
            bail!("OpenCode export capture deadline already elapsed; refusing export");
        }
        let exporter_fd = pin_trusted_opencode_exporter_until(deadline).await?;
        run_export_subprocess_sandboxed_with_exporter_fd(exporter_fd, session_id, limits, deadline)
            .await
    }
}

/// Test-only seam for sandbox integration fixtures. Production never reaches
/// this path: it seals an fd against the persisted trust record above before
/// the bwrap argv is assembled.
#[cfg(all(test, target_os = "linux"))]
async fn run_export_subprocess_sandboxed_for_test(
    binary: &Path,
    session_id: &str,
    limits: ExportLimits,
) -> Result<Vec<u8>> {
    if !valid_session_id(session_id) {
        bail!("invalid OpenCode session id (expected alnum/dash/underscore, ≤64 chars)");
    }
    let deadline = tokio::time::Instant::now() + limits.deadline;
    let exporter_fd = pin_exporter_fd(binary, None)?;
    run_export_subprocess_sandboxed_with_exporter_fd(exporter_fd, session_id, limits, deadline)
        .await
}

#[cfg(target_os = "linux")]
async fn run_export_subprocess_sandboxed_with_exporter_fd(
    exporter_fd: std::os::fd::OwnedFd,
    session_id: &str,
    limits: ExportLimits,
    deadline: tokio::time::Instant,
) -> Result<Vec<u8>> {
    let trusted_bwrap = Some(resolve_trusted_bwrap_until(deadline).await?);
    let assembled = assemble_sandboxed_export(exporter_fd, trusted_bwrap.as_deref())?;
    if tokio::time::Instant::now() >= deadline {
        bail!(
            "OpenCode export sandbox capability probe exhausted the export deadline; refusing export"
        );
    }
    run_bounded_exporter(
        &assembled.program,
        &assembled.pre_args,
        session_id,
        limits,
        deadline,
        assembled.keep_fds,
    )
    .await
}

/// Assembled Required-sandbox argv plus caller-held store fds.
/// `program` is the sandbox backend (trusted bwrap or `sandbox-exec`);
/// `pre_args` is everything transform placed after it (including `--` and the
/// fixed private exporter target). `export <sid>` is appended by
/// [`run_bounded_exporter`].
#[cfg(target_os = "linux")]
struct AssembledExport {
    program: PathBuf,
    pre_args: Vec<String>,
    keep_fds: PinnedFds,
}

/// Assemble the export sandbox vector through `SandboxManager::transform`.
///
/// On Linux, `trusted_bwrap` is the integrity-checked product of
/// [`resolve_trusted_bwrap`] and is consumed via `trusted_bwrap_exe` (no
/// `LIBRA_BWRAP_BINARY` / `linux_sandbox_exe` rediscovery). The shared bwrap
/// profile's `--new-session` option is deliberately removed from this
/// OpenCode-only invocation after transformation: bwrap can call `setsid()`
/// during setup before its PID-1 parent-death arm is live, letting an outer
/// hook cancellation kill only the monitor. `run_bounded_exporter` instead
/// creates the outer Linux child session in `pre_exec`, before bwrap starts;
/// the child PID remains its process-group ID for cancellation while the
/// exporter cannot retain the invoking terminal. Retained store fds stay with
/// the caller (`keep_fds`); exactly one read-only descriptor bind maps the
/// sealed exporter bytes to a fixed private target, never its host pathname or
/// parent directory.
#[cfg(target_os = "linux")]
fn assemble_sandboxed_export(
    exporter_fd: std::os::fd::OwnedFd,
    trusted_bwrap: Option<&std::path::Path>,
) -> Result<AssembledExport> {
    use std::os::fd::AsRawFd;

    use crate::internal::ai::sandbox::{
        CommandSpec, SandboxEnforcement, SandboxManager, SandboxPermissions, SandboxPolicy,
        SandboxTransformRequest, WritableBind,
    };

    // ReadOnly + Denied network. Use a fixed host-runtime directory rather
    // than the exporter's host parent: a trusted exporter under HOME or `/` must not
    // turn its entire parent tree into a read-only sandbox mount. The exact
    // retained fd is the only exporter executable capability.
    let sandbox_cwd = PathBuf::from(OPENCODE_SANDBOX_OUTER_CWD);
    let exporter_fd_number = exporter_fd.as_raw_fd();

    // Do not expose ambient HOME/XDG trees read-only. The parent may use the
    // ambient roots only to openat-pin the literal `opencode` directory; the
    // resulting descriptor is mounted at a fixed private sandbox pathname.
    let expected_ro_bind_paths = opencode_bwrap_read_only_paths(&sandbox_cwd);

    let mut keep_fds: PinnedFds = vec![exporter_fd];
    let mut writable_binds = Vec::new();
    let mut writable_fd_binds = Vec::new();
    // WAL-mode SQLite needs WRITE even for reads. Linux binds the pinned
    // directory through bwrap's FD-native mount operation to a fixed private
    // destination. `--bind-fd` consumes the setup capability before payload
    // exec, unlike a `/proc/self/fd/N` path-string bind which can leave an
    // O_PATH directory descriptor reachable by the exporter.
    match pin_opencode_store() {
        Ok(Some(fd)) => {
            let dest_path = PathBuf::from(OPENCODE_SANDBOX_STORE);
            let source = PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()));
            writable_binds.push(WritableBind {
                source,
                destination: dest_path.clone(),
            });
            writable_fd_binds.push((fd.as_raw_fd(), dest_path));
            keep_fds.push(fd);
        }
        Ok(None) => {}
        Err(err) => {
            return Err(err).context(
                "failed to resolve the pinned OpenCode store path; refusing \
                 an unsandboxed export (fail-closed)",
            );
        }
    }

    let spec = CommandSpec {
        program: OPENCODE_SANDBOX_EXPORTER.to_string(),
        args: Vec::new(),
        cwd: sandbox_cwd.clone(),
        env: std::collections::HashMap::new(),
        clear_env: true,
        stdin: None,
        timeout_ms: None,
        sandbox_permissions: SandboxPermissions::UseDefault,
        justification: Some("opencode export Required sandbox".to_string()),
    };

    let env = SandboxManager::new()
        .transform(SandboxTransformRequest {
            spec,
            policy: Some(&SandboxPolicy::ReadOnly),
            sandbox_policy_cwd: &sandbox_cwd,
            linux_sandbox_exe: None,
            use_linux_sandbox_bwrap: false,
            enforcement: SandboxEnforcement::Required,
            deny_read_paths: &[],
            extra_ro_bind_paths: &[],
            writable_binds: &writable_binds,
            trusted_bwrap_exe: trusted_bwrap,
            seccomp_policy_path: None,
        })
        .context(
            "failed to assemble the OpenCode export sandbox \
             (SandboxEnforcement::Required); refusing an unsandboxed export",
        )?;

    let mut command = env.command;
    insert_opencode_private_mounts(&mut command, exporter_fd_number)?;
    replace_opencode_path_fd_binds(&mut command, &writable_fd_binds)?;
    remove_opencode_bwrap_new_session(
        &mut command,
        exporter_fd_number,
        &expected_ro_bind_paths,
        &opencode_sandbox_private_dirs(),
        &writable_fd_binds,
    )?;
    if command.is_empty() {
        bail!("sandbox transform produced an empty command");
    }
    let program = PathBuf::from(command.remove(0));
    Ok(AssembledExport {
        program,
        pre_args: command,
        keep_fds,
    })
}

/// Remove exactly the shared bwrap profile's startup-racy `--new-session`
/// flag for OpenCode export only.
///
/// This must remain a narrow post-transform adjustment rather than a change to
/// `SandboxManager`'s generic profile: other sandbox callers may require a
/// new session. Reject drift rather than silently emitting an ambiguous argv;
/// in particular, the PID namespace, network isolation, parent-death controls,
/// mount-option arities, writable FD bind, and exact exporter tail must remain
/// canonical before the bwrap command delimiter.
#[cfg(any(target_os = "linux", test))]
fn remove_opencode_bwrap_new_session(
    command: &mut Vec<String>,
    expected_exporter_fd: i32,
    expected_ro_bind_paths: &[PathBuf],
    expected_private_dirs: &[PathBuf],
    expected_writable_binds: &[(i32, PathBuf)],
) -> Result<()> {
    let separators: Vec<usize> = command
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| (argument == "--").then_some(index))
        .collect();
    let [separator] = separators.as_slice() else {
        bail!("OpenCode export sandbox argv has an invalid command delimiter; refusing export");
    };
    if *separator == 0
        || command.len() != *separator + 2
        || !Path::new(&command[0]).is_absolute()
        || !Path::new(&command[*separator + 1]).is_absolute()
        || Path::new(&command[*separator + 1]) != Path::new(OPENCODE_SANDBOX_EXPORTER)
        || expected_exporter_fd < 3
    {
        bail!(
            "OpenCode export sandbox argv has an invalid exporter descriptor tail; refusing export"
        );
    }

    let absolute_path = |value: &str| Path::new(value).is_absolute();
    let mut index = 1;
    let mut new_session_index = None;
    let mut unshare_all = 0;
    let mut die_with_parent = 0;
    let mut unshare_net = 0;
    let mut proc_mount = 0;
    let mut dev_mount = 0;
    let mut tmpfs_mount = 0;
    let mut tmpfs_index = None;
    let mut next_private_dir = 0;
    let mut exporter_fd_bind = 0;
    let mut remaining_ro_bind_paths: Vec<String> = expected_ro_bind_paths
        .iter()
        .map(|source| source.to_string_lossy().into_owned())
        .collect();
    let expected_exporter_fd = expected_exporter_fd.to_string();
    let mut remaining_writable_binds: Vec<(String, String)> = expected_writable_binds
        .iter()
        .map(|(source, destination)| {
            (
                source.to_string(),
                destination.to_string_lossy().into_owned(),
            )
        })
        .collect();

    while index < *separator {
        let option = command[index].as_str();
        match option {
            "--unshare-all" => {
                unshare_all += 1;
                index += 1;
            }
            "--die-with-parent" => {
                die_with_parent += 1;
                index += 1;
            }
            "--new-session" => {
                if new_session_index.replace(index).is_some() {
                    bail!(
                        "OpenCode export sandbox argv has duplicate containment options; refusing export"
                    );
                }
                index += 1;
            }
            "--unshare-net" => {
                unshare_net += 1;
                index += 1;
            }
            "--proc" | "--dev" | "--tmpfs" | "--remount-ro" => {
                let Some(destination) = command.get(index + 1) else {
                    bail!(
                        "OpenCode export sandbox argv has a malformed mount option; refusing export"
                    );
                };
                if !absolute_path(destination) {
                    bail!(
                        "OpenCode export sandbox argv has a non-absolute mount destination; refusing export"
                    );
                }
                match option {
                    "--proc" if destination == "/proc" => proc_mount += 1,
                    "--dev" if destination == "/dev" => dev_mount += 1,
                    "--tmpfs" if destination == "/tmp" => {
                        tmpfs_mount += 1;
                        tmpfs_index = Some(index);
                    }
                    "--remount-ro" => {}
                    _ => bail!(
                        "OpenCode export sandbox argv has an unexpected mount destination; refusing export"
                    ),
                }
                index += 2;
            }
            "--dir" => {
                let Some(destination) = command.get(index + 1) else {
                    bail!(
                        "OpenCode export sandbox argv has a malformed private directory; refusing export"
                    );
                };
                let Some(tmpfs_index) = tmpfs_index else {
                    bail!(
                        "OpenCode export sandbox argv creates private directories before its tmpfs; refusing export"
                    );
                };
                if index <= tmpfs_index
                    || expected_private_dirs
                        .get(next_private_dir)
                        .is_none_or(|expected| expected != Path::new(destination))
                {
                    bail!(
                        "OpenCode export sandbox argv has an unexpected private directory; refusing export"
                    );
                }
                next_private_dir += 1;
                index += 2;
            }
            "--ro-bind" => {
                let (Some(source), Some(destination)) =
                    (command.get(index + 1), command.get(index + 2))
                else {
                    bail!(
                        "OpenCode export sandbox argv has a malformed read-only bind; refusing export"
                    );
                };
                if !absolute_path(source) || !absolute_path(destination) {
                    bail!(
                        "OpenCode export sandbox argv has an invalid read-only bind; refusing export"
                    );
                }
                if source.starts_with("/proc/self/fd/") {
                    bail!(
                        "OpenCode export sandbox argv retained a path-string descriptor mount; refusing export"
                    );
                }
                if source != destination {
                    bail!(
                        "OpenCode export sandbox argv has an invalid read-only bind; refusing export"
                    );
                }
                let Some(position) = remaining_ro_bind_paths
                    .iter()
                    .position(|expected| expected == source)
                else {
                    bail!(
                        "OpenCode export sandbox argv has an unexpected read-only bind; refusing export"
                    );
                };
                remaining_ro_bind_paths.remove(position);
                index += 3;
            }
            "--ro-bind-fd" => {
                let (Some(fd), Some(destination)) =
                    (command.get(index + 1), command.get(index + 2))
                else {
                    bail!(
                        "OpenCode export sandbox argv has a malformed descriptor read-only bind; refusing export"
                    );
                };
                if fd != &expected_exporter_fd || destination != OPENCODE_SANDBOX_EXPORTER {
                    bail!(
                        "OpenCode export sandbox argv has an unexpected descriptor read-only bind; refusing export"
                    );
                }
                exporter_fd_bind += 1;
                index += 3;
            }
            "--bind-fd" => {
                let (Some(source), Some(destination)) =
                    (command.get(index + 1), command.get(index + 2))
                else {
                    bail!(
                        "OpenCode export sandbox argv has a malformed descriptor writable bind; refusing export"
                    );
                };
                if !source.as_bytes().iter().all(u8::is_ascii_digit) || !absolute_path(destination)
                {
                    bail!(
                        "OpenCode export sandbox argv has an invalid descriptor writable bind; refusing export"
                    );
                }
                let Some(position) = remaining_writable_binds.iter().position(
                    |(expected_source, expected_destination)| {
                        expected_source == source && expected_destination == destination
                    },
                ) else {
                    bail!(
                        "OpenCode export sandbox argv has an unexpected writable bind; refusing export"
                    );
                };
                remaining_writable_binds.remove(position);
                index += 3;
            }
            "--bind" => bail!(
                "OpenCode export sandbox argv retained a path-string writable descriptor mount; refusing export"
            ),
            "--share-net" => {
                bail!("OpenCode export sandbox argv enables network sharing; refusing export")
            }
            _ => bail!(
                "OpenCode export sandbox argv contains an unsupported option; refusing export"
            ),
        }
    }

    let Some(new_session_index) = new_session_index else {
        bail!("OpenCode export sandbox argv is missing its session control; refusing export");
    };
    if unshare_all != 1
        || die_with_parent != 1
        || unshare_net != 1
        || proc_mount != 1
        || dev_mount != 1
        || tmpfs_mount != 1
        || next_private_dir != expected_private_dirs.len()
        || exporter_fd_bind != 1
        || !remaining_ro_bind_paths.is_empty()
        || !remaining_writable_binds.is_empty()
    {
        bail!("OpenCode export sandbox argv has unexpected containment options; refusing export");
    }

    command.remove(new_session_index);
    if command.iter().any(|argument| argument == "--new-session") {
        bail!(
            "OpenCode export sandbox argv retained an unexpected new-session option; refusing export"
        );
    }
    Ok(())
}

/// Insert the exact private directory chain plus one sealed exporter FD bind
/// required by the fixed exporter HOME/XDG environment. The generic bwrap
/// builder owns `/tmp` but knows nothing about this bridge's private layout,
/// so this narrow OpenCode-only adjustment creates no ambient host mount.
#[cfg(target_os = "linux")]
fn insert_opencode_private_mounts(command: &mut Vec<String>, exporter_fd: i32) -> Result<()> {
    if exporter_fd < 3 {
        bail!("OpenCode exporter capability descriptor occupied stdio; refusing export");
    }
    let separators: Vec<usize> = command
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| (argument == "--").then_some(index))
        .collect();
    let [separator] = separators.as_slice() else {
        bail!("OpenCode export sandbox argv has an invalid command delimiter; refusing export");
    };
    let insertion = command
        .iter()
        .take(*separator)
        .position(|argument| argument == "--bind")
        .unwrap_or(*separator);
    let mut private_mount_args: Vec<String> = opencode_sandbox_private_dirs()
        .into_iter()
        .flat_map(|path| ["--dir".to_string(), path.to_string_lossy().into_owned()])
        .collect();
    private_mount_args.extend([
        "--ro-bind-fd".to_string(),
        exporter_fd.to_string(),
        OPENCODE_SANDBOX_EXPORTER.to_string(),
    ]);
    command.splice(insertion..insertion, private_mount_args);
    Ok(())
}

/// Replace only the shared transform's exact `/proc/self/fd/N` writable store
/// bind with bwrap's descriptor-native form. A path-string bind leaves the
/// directory FD inherited into the payload, allowing `openat(fd, "..")` to
/// escape the fixed mount target. `--bind-fd` makes bwrap consume the known
/// setup FD; the feature probe in [`resolve_trusted_bwrap`] verifies that the
/// payload cannot retain or traverse it before this production path is usable.
#[cfg(any(target_os = "linux", test))]
fn replace_opencode_path_fd_binds(
    command: &mut [String],
    expected_writable_binds: &[(i32, PathBuf)],
) -> Result<()> {
    let separators: Vec<usize> = command
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| (argument == "--").then_some(index))
        .collect();
    let [separator] = separators.as_slice() else {
        bail!("OpenCode export sandbox argv has an invalid command delimiter; refusing export");
    };
    let mut remaining: Vec<(String, String)> = expected_writable_binds
        .iter()
        .map(|(fd, destination)| {
            (
                format!("/proc/self/fd/{fd}"),
                destination.to_string_lossy().into_owned(),
            )
        })
        .collect();
    let mut index = 1;
    while index < *separator {
        if command[index] != "--bind" {
            index += 1;
            continue;
        }
        let (Some(source), Some(destination)) = (command.get(index + 1), command.get(index + 2))
        else {
            bail!("OpenCode export sandbox argv has a malformed writable bind; refusing export");
        };
        let Some(position) =
            remaining
                .iter()
                .position(|(expected_source, expected_destination)| {
                    expected_source == source && expected_destination == destination
                })
        else {
            bail!(
                "OpenCode export sandbox argv has an unexpected path-string writable bind; refusing export"
            );
        };
        let (source, _) = remaining.remove(position);
        let Some(fd) = source.strip_prefix("/proc/self/fd/") else {
            bail!("OpenCode export sandbox argv has an invalid descriptor source; refusing export");
        };
        command[index] = "--bind-fd".to_string();
        command[index + 1] = fd.to_string();
        index += 3;
    }
    if !remaining.is_empty() {
        bail!(
            "OpenCode export sandbox argv omitted a required descriptor writable bind; refusing export"
        );
    }
    Ok(())
}

/// Fixed, empty paths within the private bwrap tmpfs. Keep this list ordered:
/// the bwrap `--dir` operands form a parent-before-child chain and the argv
/// grammar below requires this exact sequence.
#[cfg(any(target_os = "linux", test))]
fn opencode_sandbox_private_dirs() -> Vec<PathBuf> {
    [
        OPENCODE_SANDBOX_ROOT,
        OPENCODE_SANDBOX_HOME,
        OPENCODE_SANDBOX_DATA_HOME,
        OPENCODE_SANDBOX_CONFIG_HOME,
        OPENCODE_SANDBOX_BIN_DIR,
        OPENCODE_SANDBOX_STORE,
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

/// The OpenCode Required profile may read only the stable host runtime paths
/// and its fixed `/usr` construction cwd. Keep this local mirror of the shared
/// builder's fixed host list so the post-transform boundary rejects a future
/// ambient-HOME, exporter-parent, or broad host bind rather than silently
/// widening capture's read surface.
#[cfg(target_os = "linux")]
fn opencode_bwrap_read_only_paths(sandbox_cwd: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = [
        "/bin",
        "/usr",
        "/lib",
        "/lib64",
        "/etc/hosts",
        "/etc/resolv.conf",
        "/etc/ssl",
        "/etc/ca-certificates",
    ]
    .into_iter()
    .map(PathBuf::from)
    .filter(|path| path.exists())
    .collect();
    paths.push(sandbox_cwd.to_path_buf());
    paths
}

/// Revalidate the persisted trust record and immediately seal the exact
/// exporter bytes into an inherited descriptor. The path supplied by callers
/// is only an identity check against the just-revalidated canonical record;
/// bwrap later maps the sealed descriptor to its fixed private executable
/// target, never this pathname.
#[cfg(target_os = "linux")]
async fn pin_revalidated_opencode_exporter_until(
    binary: &Path,
    deadline: tokio::time::Instant,
) -> Result<std::os::fd::OwnedFd> {
    pin_trusted_opencode_exporter_for_path_until(Some(binary.to_path_buf()), deadline).await
}

/// Resolve the configured trust record and seal the exact verified exporter
/// bytes under one absolute deadline. The optional expected path exists only
/// for the compatibility API that receives a previously resolved path; hook
/// capture passes none, so no unbounded path-returning trust lookup precedes
/// this descriptor boundary.
#[cfg(target_os = "linux")]
async fn pin_trusted_opencode_exporter_until(
    deadline: tokio::time::Instant,
) -> Result<std::os::fd::OwnedFd> {
    pin_trusted_opencode_exporter_for_path_until(None, deadline).await
}

#[cfg(target_os = "linux")]
async fn pin_trusted_opencode_exporter_for_path_until(
    expected_path: Option<PathBuf>,
    deadline: tokio::time::Instant,
) -> Result<std::os::fd::OwnedFd> {
    match tokio::time::timeout_at(deadline, async move {
        let provenance = trusted_opencode_provenance().await?;
        if let Some(expected_path) = expected_path
            && expected_path != provenance.canonical_path
        {
            bail!(
                "OpenCode exporter provenance changed before descriptor pin; refusing export (fail-closed)"
            );
        }

        // `pin_exporter_fd` copies and hashes an operator-controlled regular
        // file. Even I/O bounded by that file's own length can block
        // indefinitely on FUSE, so keep it on the blocking pool and let the
        // public capture deadline decide whether its result may progress to
        // bwrap. If the deadline wins, dropping this JoinHandle cannot cancel
        // a started blocking task, but that task owns its anonymous
        // tempfile/fd and drops them before returning; no exporter or sandbox
        // child has been spawned.
        let seal_path = provenance.canonical_path.clone();
        let sealed_provenance = provenance.clone();
        seal_exporter_fd_until(deadline, move || {
            pin_exporter_fd(&seal_path, Some(&sealed_provenance))
        })
        .await
    })
    .await
    {
        Ok(result) => result,
        Err(_) => bail!(
            "OpenCode exporter trust revalidation or sealing exceeded the capture deadline; refusing export"
        ),
    }
}

/// Await a blocking sealed-exporter operation under the caller's one absolute
/// capture deadline. If the timer wins, the task has not produced a capability
/// that can reach bwrap; if it is already running, its owned anonymous
/// tempfile/fd is dropped with its eventual result rather than being accepted
/// by a later capture attempt.
#[cfg(target_os = "linux")]
async fn seal_exporter_fd_until<F>(
    deadline: tokio::time::Instant,
    seal: F,
) -> Result<std::os::fd::OwnedFd>
where
    F: FnOnce() -> Result<std::os::fd::OwnedFd> + Send + 'static,
{
    match tokio::time::timeout_at(deadline, tokio::task::spawn_blocking(seal)).await {
        Ok(Ok(result)) => result,
        Ok(Err(err)) => {
            Err(anyhow::Error::new(err).context("join trusted OpenCode exporter sealing"))
        }
        Err(_) => bail!(
            "OpenCode exporter trust revalidation or sealing exceeded the capture deadline; refusing export"
        ),
    }
}

/// Open a regular, non-symlink exporter and copy it into a private anonymous
/// executable descriptor. When provenance is supplied, compare the opened
/// fd's device/inode/mtime and the bytes copied into that descriptor to the
/// trust record first. The copy closes both replacement and same-inode
/// post-hash mutation races: bwrap receives bytes owned only by Libra, not a
/// live fd for a mutable trusted-directory file.
#[cfg(target_os = "linux")]
fn pin_exporter_fd(
    binary: &Path,
    expected: Option<&crate::internal::ai::observed_agents::trust::Provenance>,
) -> Result<std::os::fd::OwnedFd> {
    use std::{
        io::{Read as _, Seek as _, Write as _},
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::{
                ffi::OsStrExt,
                fs::{MetadataExt, PermissionsExt},
            },
        },
    };

    if !binary.is_absolute() {
        bail!("exporter binary path must be absolute (trusted provenance)");
    }
    let binary_c = std::ffi::CString::new(binary.as_os_str().as_bytes())
        .context("trusted OpenCode exporter path contains NUL")?;
    // SAFETY: `binary_c` is a NUL-terminated path; the returned descriptor is
    // immediately wrapped for RAII. O_NOFOLLOW rejects a final symlink.
    let raw = unsafe {
        libc::open(
            binary_c.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error())
            .context("open trusted OpenCode exporter descriptor");
    }
    // SAFETY: `raw` is a fresh owned fd returned by open(2).
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut source = std::fs::File::from(fd);
    let metadata = source
        .metadata()
        .context("stat trusted OpenCode exporter descriptor")?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        bail!(
            "trusted OpenCode exporter descriptor is not an executable regular file; refusing export"
        );
    }
    // No fixed size cap (the real OpenCode CLI is a ~171 MiB single file):
    // like trust hashing, the copy is bounded by this descriptor's own length.
    let reported_len = metadata.len();

    if let Some(expected) = expected
        && (metadata.dev() != expected.device
            || metadata.ino() != expected.inode
            || metadata.mtime() != expected.mtime)
    {
        bail!(
            "OpenCode exporter provenance changed before descriptor pin; refusing export (fail-closed)"
        );
    }

    let mut sealed = tempfile::tempfile().context("create sealed OpenCode exporter descriptor")?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 32 * 1024];
    let mut copied = 0_u64;
    loop {
        let read = source
            .read(&mut buffer)
            .context("read trusted OpenCode exporter descriptor")?;
        if read == 0 {
            break;
        }
        copied = copied.saturating_add(read as u64);
        if copied > reported_len {
            bail!(
                "trusted OpenCode exporter grew past its reported length while sealing; refusing export (fail-closed)"
            );
        }
        hasher.update(&buffer[..read]);
        sealed
            .write_all(&buffer[..read])
            .context("seal trusted OpenCode exporter bytes")?;
    }
    if expected.is_some_and(|expected| hex::encode(hasher.finalize()) != expected.sha256) {
        bail!(
            "OpenCode exporter provenance changed before descriptor pin; refusing export (fail-closed)"
        );
    }
    sealed
        .set_permissions(std::fs::Permissions::from_mode(0o500))
        .context("make sealed OpenCode exporter executable")?;
    sealed
        .rewind()
        .context("rewind sealed OpenCode exporter descriptor")?;

    // The sealing handle is O_RDWR. Re-open the anonymous descriptor through
    // procfs as O_RDONLY, then drop the writable handle before handing the
    // capability to bwrap: the exporter must never inherit a writable alias
    // to Libra's sealed executable bytes.
    let sealed_path = std::ffi::CString::new(format!("/proc/self/fd/{}", sealed.as_raw_fd()))
        .context("format sealed OpenCode exporter descriptor path")?;
    // SAFETY: `sealed_path` names this process's still-owned anonymous file;
    // the returned descriptor is immediately wrapped for RAII.
    let readonly_raw =
        unsafe { libc::open(sealed_path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if readonly_raw < 0 {
        return Err(std::io::Error::last_os_error())
            .context("reopen sealed OpenCode exporter as read-only");
    }
    // SAFETY: `readonly_raw` is a fresh owned fd returned by open(2).
    let readonly = unsafe { OwnedFd::from_raw_fd(readonly_raw) };
    drop(sealed);
    duplicate_capability_fd_at_least_three(readonly, "sealed OpenCode exporter")
}

/// Move a capability descriptor above stdio and make it close-on-exec until
/// the bounded runner explicitly allows it in the fork child. If an embedding
/// host closed stdin/stdout/stderr, a raw `open` can otherwise return 0, 1, or
/// 2 and be overwritten by `Command`'s stdio setup before bwrap consumes its
/// descriptor-native mount input.
#[cfg(target_os = "linux")]
fn duplicate_capability_fd_at_least_three(
    fd: std::os::fd::OwnedFd,
    capability: &str,
) -> Result<std::os::fd::OwnedFd> {
    use std::os::fd::{AsRawFd, FromRawFd};

    // SAFETY: fcntl duplicates this process-owned descriptor atomically with
    // a lower bound of 3 and returns a fresh fd on success.
    let duplicated = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if duplicated < 3 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!("reserve a non-stdio descriptor for {capability}; refusing OpenCode export")
        });
    }
    // `fd` drops here, closing the original (which may have occupied a stdio
    // slot only because the embedding host had closed it).
    // SAFETY: `duplicated` is a new owned fd from fcntl(F_DUPFD_CLOEXEC).
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicated) })
}

#[cfg(target_os = "linux")]
fn which_bwrap() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("bwrap"))
        .find(|candidate| candidate.is_file())
}

/// Resolve the bubblewrap binary for the Required sandbox WITH integrity
/// checks (Codex M3 R2 P1-4). `LIBRA_LINUX_SANDBOX_EXE` / `PATH` may only NAME
/// the candidate — it must then resolve (through every symlink) to a
/// root-owned regular file that is not writable by group or other. Otherwise
/// an attacker who can plant a file on `PATH` or set the env var could supply
/// a fake "bwrap" that ignores its arguments and runs the trusted exporter
/// unsandboxed (network + host writes restored). Fail-closed on any doubt: the
/// capability becomes unavailable, never a degraded unsandboxed run (GC-DR-14).
#[cfg(target_os = "linux")]
async fn resolve_trusted_bwrap_until(deadline: tokio::time::Instant) -> Result<std::path::PathBuf> {
    let candidate = resolve_trusted_bwrap_unprobed()?;
    if !trusted_bwrap_supports_fd_mounts_until(&candidate, deadline).await {
        bail!(
            "bubblewrap lacks a safe descriptor-native mount capability for OpenCode export; \
             refusing export (fail-closed)"
        );
    }
    Ok(candidate)
}

/// Resolve and integrity-check the bwrap executable without attempting its
/// OpenCode-specific FD-mount capability probe. Kept separate so the probe
/// can call it without recursion.
#[cfg(target_os = "linux")]
fn resolve_trusted_bwrap_unprobed() -> Result<std::path::PathBuf> {
    let candidate = std::env::var_os("LIBRA_LINUX_SANDBOX_EXE")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(which_bwrap)
        .ok_or_else(|| {
            anyhow!(
                "bubblewrap (bwrap) is required for the OpenCode export sandbox and was \
                 not found; install bwrap or set LIBRA_LINUX_SANDBOX_EXE to a root-owned \
                 bwrap binary (fail-closed)"
            )
        })?;
    validate_trusted_bwrap(&candidate)
}

/// Whether the current (effective) user could MODIFY this path component, and
/// therefore swap it under us. Portable integrity anchor (Codex M3 R3 P1):
/// instead of demanding `uid == 0` (which both admits a post-validation swap
/// when an ancestor is user-writable, and wrongly rejects safely-packaged
/// binaries whose owner is remapped in a user namespace), we ask the precise
/// question — can the invoking principal write here? If no component of the
/// path is user-writable, the file cannot be replaced, closing the TOCTOU.
#[cfg(target_os = "linux")]
fn modifiable_by_current_user(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    let mode = meta.mode();
    // Group- or world-writable is treated as modifiable regardless of group
    // membership (conservative; standard system paths are never 0o0X2/0o0XX7).
    if mode & 0o022 != 0 {
        return true;
    }
    // SAFETY: geteuid is always successful and has no memory effects.
    let euid = unsafe { libc::geteuid() };
    if euid == 0 {
        // Running as root: root ignores permission bits, so the real threat is
        // a NON-root owner able to rewrite an owner-writable component.
        return meta.uid() != 0 && mode & 0o200 != 0;
    }
    // Non-root: modifiable iff we own it and the owner-write bit is set.
    meta.uid() == euid && mode & 0o200 != 0
}

/// Integrity core (testable without env mutation): resolve every symlink so
/// the checks apply to the file that will actually be exec'd, require a
/// regular file, then require that NO path component (the binary or any
/// ancestor directory) is modifiable by the invoking user. Anything else is
/// refused fail-closed (GC-DR-14).
#[cfg(target_os = "linux")]
fn validate_trusted_bwrap(candidate: &std::path::Path) -> Result<std::path::PathBuf> {
    let canonical = std::fs::canonicalize(candidate).with_context(|| {
        format!(
            "cannot resolve sandbox binary {} (fail-closed)",
            candidate.display()
        )
    })?;
    let file_meta = std::fs::metadata(&canonical)
        .with_context(|| format!("cannot stat sandbox binary {}", canonical.display()))?;
    if !file_meta.file_type().is_file() {
        bail!(
            "sandbox binary {} is not a regular file; refusing (fail-closed)",
            canonical.display()
        );
    }
    // The canonical path has no symlinks, so walking `.parent()` and stat-ing
    // each component is race-consistent with what will be exec'd. Any
    // user-writable component (the file OR a directory above it) means the
    // helper could be swapped for one that runs the exporter unsandboxed.
    let mut component: Option<&std::path::Path> = Some(canonical.as_path());
    while let Some(path) = component {
        let meta = std::fs::metadata(path)
            .with_context(|| format!("cannot stat sandbox path component {}", path.display()))?;
        if modifiable_by_current_user(&meta) {
            bail!(
                "sandbox binary path component {} is modifiable by the current user; a planted \
                 or swapped helper could run the exporter unsandboxed — refusing (fail-closed, \
                 GC-DR-14)",
                path.display()
            );
        }
        component = path.parent();
    }
    Ok(canonical)
}

/// Whether a trusted, usable bubblewrap sandbox is available on this host:
/// the bwrap binary passes integrity policy AND an async, bounded
/// descriptor-native mount/consumption probe. Tests therefore gate on
/// "trusted AND usable", not merely "bwrap present" — hosts without user
/// namespaces or complete `--bind-fd`/`--ro-bind-fd` payload-FD closure report
/// unavailable and production fails closed.
#[cfg(target_os = "linux")]
pub async fn trusted_bwrap_available() -> bool {
    let Ok(bwrap) = resolve_trusted_bwrap_unprobed() else {
        return false;
    };
    // `agent list` has no capture deadline, but must not claim availability
    // merely because a bwrap pathname exists. Keep its definitive probe short
    // and fail closed; the export route reruns it under the hook's absolute
    // deadline immediately before use.
    trusted_bwrap_supports_fd_mounts_until(
        &bwrap,
        tokio::time::Instant::now() + Duration::from_millis(500),
    )
    .await
}

/// Disposable, parent-owned descriptor capability used to prove the exact
/// bwrap mount contract. It stays CLOEXEC in the parent and is made
/// inheritable only in a fork child through
/// [`prepare_bwrap_capability_fds_for_exec`].
#[cfg(target_os = "linux")]
struct BwrapFdMountProbe {
    temp: tempfile::TempDir,
    store_fd: std::os::fd::OwnedFd,
    exporter_fd: std::os::fd::OwnedFd,
    store_fd_text: String,
    exporter_fd_text: String,
    script: String,
}

#[cfg(target_os = "linux")]
fn new_bwrap_fd_mount_probe() -> Option<BwrapFdMountProbe> {
    use std::os::{fd::AsRawFd, unix::fs::PermissionsExt};

    let temp = tempfile::tempdir().ok()?;
    if std::fs::create_dir(temp.path().join("opencode")).is_err()
        || std::fs::write(temp.path().join("host-only-sibling"), b"probe").is_err()
    {
        return None;
    }
    let store_fd = pin_store_under(temp.path()).ok()?;
    let exporter_source = temp.path().join("sealed-exporter-probe");
    if std::fs::write(&exporter_source, b"libra-sealed-exporter-probe").is_err()
        || std::fs::set_permissions(&exporter_source, std::fs::Permissions::from_mode(0o500))
            .is_err()
    {
        return None;
    }
    // Exercise the same sealed regular-file capability form as production,
    // not an arbitrary host pathname. The sealed copy is still parent-owned
    // and CLOEXEC until the probe's fork-child allowlist runs.
    let exporter_fd = pin_exporter_fd(&exporter_source, None).ok()?;
    let store_raw_fd = store_fd.as_raw_fd();
    let exporter_raw_fd = exporter_fd.as_raw_fd();
    if store_raw_fd < 3 || exporter_raw_fd < 3 || store_raw_fd == exporter_raw_fd {
        return None;
    }
    let store_fd_text = store_raw_fd.to_string();
    let exporter_fd_text = exporter_raw_fd.to_string();
    let store_traversal = format!("/proc/self/fd/{store_raw_fd}/../host-only-sibling");
    let exporter_traversal = format!("/proc/self/fd/{exporter_raw_fd}/../host-only-sibling");
    let script = format!(
        "test ! -e /proc/self/fd/{store_raw_fd} && test ! -e {store_traversal} || exit 97; \
         test ! -e /proc/self/fd/{exporter_raw_fd} && test ! -e {exporter_traversal} || exit 98; \
         for candidate in /proc/self/fd/[0-9]*; do \
           test ! -e \"$candidate/../host-only-sibling\" || exit 99; \
         done; \
         test \"$(cat /libra-opencode-exporter-probe)\" = libra-sealed-exporter-probe || exit 100; \
         if printf mutated > /libra-opencode-exporter-probe 2>/dev/null; then exit 101; fi; \
         printf probe > /mnt/probe"
    );
    Some(BwrapFdMountProbe {
        temp,
        store_fd,
        exporter_fd,
        store_fd_text,
        exporter_fd_text,
        script,
    })
}

#[cfg(target_os = "linux")]
fn bwrap_fd_mount_probe_succeeded(probe: &BwrapFdMountProbe, status_success: bool) -> bool {
    status_success
        && std::fs::read(probe.temp.path().join("opencode/probe"))
            .is_ok_and(|bytes| bytes == b"probe")
}

/// Prepare disposable probe state off the async executor. Tempdir creation,
/// descriptor pinning and sealed-file copying all touch the local filesystem;
/// a slow/FUSE filesystem must consume the same deadline as the probe rather
/// than blocking hook capture before bwrap is even spawned.
#[cfg(target_os = "linux")]
async fn new_bwrap_fd_mount_probe_until(
    deadline: tokio::time::Instant,
) -> Option<BwrapFdMountProbe> {
    match tokio::time::timeout_at(
        deadline,
        tokio::task::spawn_blocking(new_bwrap_fd_mount_probe),
    )
    .await
    {
        Ok(Ok(probe)) => probe,
        Ok(Err(_)) | Err(_) => None,
    }
}

/// Run the FD-mount probe in the same cancellation-safe async shape as the
/// production exporter. The deadline is the caller's absolute export
/// deadline, not a second independent timeout: capability discovery cannot
/// consume extra hook budget.
#[cfg(target_os = "linux")]
async fn trusted_bwrap_supports_fd_mounts_until(
    bwrap: &Path,
    deadline: tokio::time::Instant,
) -> bool {
    use std::os::fd::AsRawFd;

    let Some(probe) = new_bwrap_fd_mount_probe_until(deadline).await else {
        return false;
    };
    let probe_fd = [probe.store_fd.as_raw_fd(), probe.exporter_fd.as_raw_fd()];
    let mut command = tokio::process::Command::new(bwrap);
    command
        .args([
            "--unshare-all",
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--bind-fd",
            &probe.store_fd_text,
            "/mnt",
            "--ro-bind-fd",
            &probe.exporter_fd_text,
            "/libra-opencode-exporter-probe",
            "--",
            "/bin/sh",
            "-c",
            &probe.script,
        ])
        .env_clear()
        .current_dir(OPENCODE_SANDBOX_OUTER_CWD)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(if cfg!(test) {
            std::process::Stdio::inherit()
        } else {
            std::process::Stdio::null()
        })
        .kill_on_drop(true);
    // SAFETY: the callback sees only a preallocated raw-fd array and invokes
    // syscall/fcntl/setsid. It does not touch parent state after fork.
    unsafe {
        command.pre_exec(move || {
            prepare_bwrap_capability_fds_for_exec(&probe_fd)?;
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return false,
    };
    let process_group = child.id();
    let mut child = ExporterCancellationGuard::new(child, process_group);
    let result = tokio::time::timeout_at(deadline, async {
        match child.child_mut() {
            Some(child) => child.wait().await,
            None => Err(std::io::Error::other(
                "bwrap capability probe child was unavailable",
            )),
        }
    })
    .await;
    match result {
        Ok(Ok(status)) => {
            child.disarm_child_after_wait();
            child.finish();
            bwrap_fd_mount_probe_succeeded(&probe, status.success())
        }
        Ok(Err(_)) | Err(_) => {
            let _ = child.terminate_and_reap();
            false
        }
    }
}

/// Non-Linux hosts have no bwrap sandbox — the export capability is
/// unavailable (fail-closed), so it is never "trusted and usable".
#[cfg(not(target_os = "linux"))]
pub async fn trusted_bwrap_available() -> bool {
    false
}

/// Pin the OpenCode WAL store for a race-safe RW bind.
///
/// The ambient data root is consulted only in the parent, then the literal
/// `opencode` entry is openat-pinned. The caller mounts that descriptor at the
/// fixed [`OPENCODE_SANDBOX_STORE`] path; an ambient pathname is never passed
/// to bwrap or the exporter. Reads the data root from a structurally safe
/// `XDG_DATA_HOME` or `HOME/.local/share`.
///
/// Missing/unpinnable store → `Ok(None)` (skip the RW exception).
#[cfg(target_os = "linux")]
fn pin_opencode_store() -> Result<Option<std::os::fd::OwnedFd>> {
    let Some(base) = ambient_opencode_data_root() else {
        return Ok(None);
    };
    match pin_store_under(&base) {
        Ok(fd) => Ok(Some(fd)),
        Err(err) => {
            let absent = err.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            });
            if absent {
                tracing::warn!(
                    reason = "opencode_store_not_found",
                    "cannot pin opencode data dir for RW bind; skipping (export may degrade)"
                );
                return Ok(None);
            }
            Err(err).context(
                "OpenCode store exists but could not be pinned (symlink/non-directory \
                 or other pin failure); refusing an unsandboxed export (fail-closed)",
            )
        }
    }
}

/// Select an ambient data root only when every component is an existing,
/// non-symlink directory below `/`. This is intentionally stricter than a
/// normal XDG resolver: the value selects a writable descriptor mount, so
/// relative paths, root, `..`, and symlinked ancestry are all rejected rather
/// than becoming a surprising host capability. Invalid roots simply provide
/// no store to the exporter; it remains sandboxed and fails normally if it
/// cannot serve the requested transcript.
#[cfg(target_os = "linux")]
fn ambient_opencode_data_root() -> Option<PathBuf> {
    let configured = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".local/share"))
        })?;
    structurally_safe_host_directory(&configured).then_some(configured)
}

/// This check intentionally never canonicalizes into a different spelling:
/// a symlink in any ancestor would make the ambient selection ambiguous. The
/// pinned descriptor remains the final race-resistant authority after this
/// preflight succeeds.
#[cfg(target_os = "linux")]
fn structurally_safe_host_directory(path: &Path) -> bool {
    if !path.is_absolute() || path == Path::new("/") {
        return false;
    }
    let text = path.to_string_lossy();
    if text
        .split('/')
        .any(|component| component == "." || component == "..")
    {
        return false;
    }

    let mut current = PathBuf::from("/");
    for component in path.components() {
        let Component::Normal(component) = component else {
            continue;
        };
        current.push(component);
        let Ok(metadata) = std::fs::symlink_metadata(&current) else {
            return false;
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return false;
        }
    }
    true
}

/// Resolution + pin as ONE atomic `openat`: open the data root, then `openat`
/// the literal `opencode` entry with `O_DIRECTORY|O_NOFOLLOW`. Because the
/// returned fd IS the validated directory, a concurrent rename/exchange of
/// `opencode` cannot make the bound directory differ from the checked one.
/// `O_NOFOLLOW` rejects a symlinked entry; `O_DIRECTORY` requires a directory.
///
/// Linux uses `O_PATH`; the descriptor remains CLOEXEC in the parent and is
/// exposed only to bwrap's setup process through a fork-child allowlist.
#[cfg(target_os = "linux")]
fn pin_store_under(base: &std::path::Path) -> Result<std::os::fd::OwnedFd> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };

    let base_c = std::ffi::CString::new(base.as_os_str().as_bytes())
        .context("data root path contains NUL")?;
    // Anchor the child lookup to a handle on the data root. Following symlinks
    // in the root's own ancestry is fine — only the final `opencode` component
    // must not be a symlink, which the openat below enforces.
    let base_flags = libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC;
    // SAFETY: base_c is a valid C string; the fd is wrapped for RAII below.
    let base_raw = unsafe { libc::open(base_c.as_ptr(), base_flags) };
    if base_raw < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("open opencode data root {}", base.display()));
    }
    // SAFETY: fresh owned fd from open(2).
    let base_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(base_raw) };

    // INVARIANT: a constant literal with no interior NUL.
    let name = std::ffi::CString::new("opencode").expect("literal has no NUL");
    let child_flags = libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: base_fd is a valid dir fd; name is a valid C string; the result
    // is wrapped for RAII.
    let raw = unsafe { libc::openat(base_fd.as_raw_fd(), name.as_ptr(), child_flags) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error())
            .context("pin opencode store (openat, no-follow directory)");
    }
    // SAFETY: fresh owned fd from openat(2).
    let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
    // Keep this descriptor CLOEXEC until `run_bounded_exporter`'s Linux
    // pre-exec boundary has marked every non-stdio fd CLOEXEC and explicitly
    // retained only its known capability set. Duplicate even a high-numbered
    // descriptor so a host with closed stdio cannot leave the mount source at
    // 0, 1, or 2 where Command's stdio setup could overwrite it.
    duplicate_capability_fd_at_least_three(fd, "pinned OpenCode store")
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn export_identity_document(session_id: &str, working_dir: &Path) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "info": {
                "id": session_id,
                "directory": working_dir.to_string_lossy(),
            },
            "messages": [],
        }))
        .expect("serialize test OpenCode export")
    }

    #[test]
    fn opencode_export_identity_accepts_matching_native_session_and_directory() {
        let working_dir = tempfile::tempdir().expect("create working directory");
        let bytes = export_identity_document("ses_expected", working_dir.path());

        validate_opencode_export_identity(&bytes, "ses_expected", working_dir.path())
            .expect("matching canonical OpenCode export identity must be accepted");
    }

    #[test]
    fn opencode_export_identity_rejects_missing_native_session_id() {
        let working_dir = tempfile::tempdir().expect("create working directory");
        let bytes = serde_json::to_vec(&serde_json::json!({
            "info": {"directory": working_dir.path().to_string_lossy()},
            "messages": [],
        }))
        .expect("serialize malformed test OpenCode export");

        let error = validate_opencode_export_identity(&bytes, "ses_expected", working_dir.path())
            .expect_err("an export without its native id must be rejected");
        assert!(
            format!("{error:#}").contains("native session identity"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn opencode_export_identity_rejects_mismatched_native_session_id() {
        let working_dir = tempfile::tempdir().expect("create working directory");
        let bytes = export_identity_document("ses_other", working_dir.path());

        let error = validate_opencode_export_identity(&bytes, "ses_expected", working_dir.path())
            .expect_err("an export for another session must be rejected");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("does not match the active capture session"),
            "unexpected error: {rendered}"
        );
        assert!(
            !rendered.contains("ses_other"),
            "exporter-supplied session id leaked: {rendered}"
        );
    }

    #[test]
    fn opencode_export_identity_rejects_duplicate_keys_without_echoing_them() {
        let working_dir = tempfile::tempdir().expect("create working directory");
        let directory = serde_json::to_string(&working_dir.path().to_string_lossy().into_owned())
            .expect("serialize working directory");
        let bytes = format!(
            r#"{{"info":{{"id":"ses_expected","id":"DO_NOT_LEAK","directory":{directory}}},"messages":[]}}"#
        )
        .into_bytes();

        let error = validate_opencode_export_identity(&bytes, "ses_expected", working_dir.path())
            .expect_err("a duplicate native identity field must be rejected");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("not a valid canonical document"),
            "unexpected error: {rendered}"
        );
        assert!(
            !rendered.contains("DO_NOT_LEAK"),
            "duplicate key leaked through the error: {rendered}"
        );
    }

    #[test]
    fn opencode_export_identity_rejects_mismatched_working_directory() {
        let working_dir = tempfile::tempdir().expect("create working directory");
        let other_working_dir = tempfile::tempdir().expect("create other working directory");
        let bytes = export_identity_document("ses_expected", other_working_dir.path());

        let error = validate_opencode_export_identity(&bytes, "ses_expected", working_dir.path())
            .expect_err("an export for another workspace must be rejected");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("does not match the active capture workspace"),
            "unexpected error: {rendered}"
        );
        assert!(
            !rendered.contains(&other_working_dir.path().display().to_string()),
            "exporter-supplied working directory leaked: {rendered}"
        );
    }

    /// Write an executable fake exporter script (tests never touch a real
    /// `opencode`, GC-DR-07). The script body receives argv untouched, which
    /// is exactly what the no-shell contract must preserve. This fixture
    /// requires a POSIX shell and Unix executable permission bits.
    #[cfg(unix)]
    fn fake_exporter(dir: &std::path::Path, body: &str) -> PathBuf {
        let path = dir.join("fake-opencode");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn shell_quote(value: &std::path::Path) -> String {
        format!("'{}'", value.to_string_lossy().replace('\'', "'\"'\"'"))
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    async fn wait_for_exporter_pid(pid_file: &std::path::Path) -> Option<libc::pid_t> {
        for _ in 0..100 {
            if let Ok(value) = std::fs::read_to_string(pid_file)
                && let Ok(pid) = value.trim().parse::<libc::pid_t>()
            {
                return Some(pid);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        None
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    async fn wait_for_exporter_reap(pid: libc::pid_t) -> bool {
        for _ in 0..100 {
            // SAFETY: signal zero probes the exact test descendant PID and
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

    /// Whether an executable named `name` is resolvable on `PATH` (used to skip
    /// tests that depend on an optional system tool such as `setsid`).
    #[cfg(all(unix, not(target_os = "macos")))]
    fn binary_on_path(name: &str) -> Option<PathBuf> {
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path).find_map(|dir| {
                let candidate = dir.join(name);
                (candidate.is_file()
                    && std::fs::metadata(&candidate)
                        .map(|m| m.permissions().mode() & 0o111 != 0)
                        .unwrap_or(false))
                .then_some(candidate)
            })
        })
    }

    #[cfg(target_os = "linux")]
    async fn wait_for_marker_len(marker: &std::path::Path) -> Option<u64> {
        for _ in 0..100 {
            if let Ok(metadata) = std::fs::metadata(marker) {
                return Some(metadata.len());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        None
    }

    /// Test-only kill switch for the intentionally escaped writer fixture.
    /// The writer polls its token, so an assertion panic cannot leave it
    /// running after the test process unwinds.
    #[cfg(target_os = "linux")]
    #[derive(Default)]
    struct EscapedWriterTokens {
        paths: Vec<PathBuf>,
    }

    #[cfg(target_os = "linux")]
    impl EscapedWriterTokens {
        fn arm(&mut self, path: PathBuf) {
            std::fs::write(&path, b"live").expect("arm escaped writer cleanup token");
            self.paths.push(path);
        }

        fn stop(&mut self, path: &std::path::Path) {
            let _ = std::fs::remove_file(path);
            self.paths.retain(|armed| armed != path);
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for EscapedWriterTokens {
        fn drop(&mut self) {
            for path in &self.paths {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    #[cfg(target_os = "linux")]
    const OPENCODE_EXPORT_PTY_DRIVER: &str = "LIBRA_OPENCODE_EXPORT_PTY_DRIVER";
    #[cfg(target_os = "linux")]
    const OPENCODE_EXPORT_PTY_DRIVER_TEST: &str = "internal::ai::observed_agents::opencode_export::tests::opencode_export_sandboxed_detaches_terminal_session_driver";
    #[cfg(target_os = "linux")]
    const OPENCODE_EXPORT_CLOSED_STDIO_DRIVER: &str = "LIBRA_OPENCODE_EXPORT_CLOSED_STDIO_DRIVER";
    #[cfg(target_os = "linux")]
    const OPENCODE_EXPORT_CLOSED_STDIO_DRIVER_TEST: &str = "internal::ai::observed_agents::opencode_export::tests::opencode_export_capability_fds_closed_stdio_driver";

    /// Re-exec the focused driver on a slave PTY. `portable_pty`'s Unix slave
    /// spawn performs `setsid()`, `TIOCSCTTY`, and stdio duplication in its
    /// child `pre_exec`; the driver below independently proves that `/dev/tty`
    /// opens before it invokes the real bwrap path.
    #[cfg(target_os = "linux")]
    fn run_opencode_export_pty_driver() -> (bool, String) {
        use std::{
            io::Read,
            sync::{Arc, Mutex},
        };

        use portable_pty::{CommandBuilder, PtySize, native_pty_system};

        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open PTY for OpenCode exporter session test");
        let executable = std::env::current_exe().expect("locate libtest executable");
        let mut command = CommandBuilder::new(executable);
        for argument in [
            "--exact",
            OPENCODE_EXPORT_PTY_DRIVER_TEST,
            "--nocapture",
            "--test-threads=1",
        ] {
            command.arg(argument);
        }
        command.env(OPENCODE_EXPORT_PTY_DRIVER, "1");
        let mut child = pair
            .slave
            .spawn_command(command)
            .expect("spawn OpenCode exporter PTY driver");
        drop(pair.slave);

        // Drain continuously: an assertion/report from the re-exec must never
        // block on a full PTY buffer and mask the terminal-boundary result.
        let mut reader = pair.master.try_clone_reader().expect("clone PTY reader");
        let sink = Arc::new(Mutex::new(Vec::new()));
        let drain_sink = Arc::clone(&sink);
        let drain = std::thread::spawn(move || {
            let mut buffer = [0_u8; 4096];
            while let Ok(read) = reader.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                drain_sink
                    .lock()
                    .expect("PTY transcript sink")
                    .extend_from_slice(&buffer[..read]);
            }
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let status = loop {
            match child.try_wait().expect("poll OpenCode exporter PTY driver") {
                Some(status) => break status,
                None if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    drop(pair.master);
                    let _ = drain.join();
                    panic!("OpenCode exporter PTY driver did not exit within 30 seconds");
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        drop(pair.master);
        let _ = drain.join();
        let output =
            String::from_utf8_lossy(&sink.lock().expect("PTY transcript sink")).into_owned();
        (status.success(), output)
    }

    /// Exercise descriptor pinning from a process whose 0/1/2 have all been
    /// closed. A descriptor created in that state would otherwise be replaced
    /// by Command's stdio `dup2` setup before bwrap could consume the
    /// descriptor-native mount input. Re-exec isolates the deliberate stdio close
    /// from the libtest process.
    #[cfg(target_os = "linux")]
    #[test]
    fn opencode_export_capability_fds_survive_closed_stdio_reexec() {
        if std::env::var_os(OPENCODE_EXPORT_CLOSED_STDIO_DRIVER).is_some() {
            return;
        }
        let executable = std::env::current_exe().expect("locate libtest executable");
        let status = std::process::Command::new(executable)
            .args([
                "--exact",
                OPENCODE_EXPORT_CLOSED_STDIO_DRIVER_TEST,
                "--nocapture",
                "--test-threads=1",
            ])
            .env(OPENCODE_EXPORT_CLOSED_STDIO_DRIVER, "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("spawn closed-stdio driver");
        assert!(
            status.success(),
            "closed-stdio pinning driver failed: {status}"
        );
    }

    /// Driver for [`opencode_export_capability_fds_survive_closed_stdio_reexec`].
    /// It exits directly after the checks so the test harness never needs a
    /// now-closed stdout/stderr to render its own result.
    #[cfg(target_os = "linux")]
    #[test]
    fn opencode_export_capability_fds_closed_stdio_driver() {
        if std::env::var_os(OPENCODE_EXPORT_CLOSED_STDIO_DRIVER).is_none() {
            return;
        }
        use std::os::fd::AsRawFd;

        let dir = tempfile::tempdir().expect("create closed-stdio driver tempdir");
        let binary = fake_exporter(dir.path(), "printf never-runs");
        std::fs::create_dir(dir.path().join("opencode")).expect("create pinned store");
        // SAFETY: this is an isolated re-exec driver. The outer test has
        // redirected all stdio first, and this child exits immediately after
        // verifying that pinning repairs low descriptor numbers.
        unsafe {
            libc::close(0);
            libc::close(1);
            libc::close(2);
        }
        let exporter = match pin_exporter_fd(&binary, None) {
            Ok(fd) => fd,
            Err(_) => std::process::exit(70),
        };
        let store = match pin_store_under(dir.path()) {
            Ok(fd) => fd,
            Err(_) => std::process::exit(71),
        };
        let exporter_flags = unsafe { libc::fcntl(exporter.as_raw_fd(), libc::F_GETFD) };
        let store_flags = unsafe { libc::fcntl(store.as_raw_fd(), libc::F_GETFD) };
        if exporter.as_raw_fd() < 3
            || store.as_raw_fd() < 3
            || exporter_flags < 0
            || store_flags < 0
            || exporter_flags & libc::FD_CLOEXEC == 0
            || store_flags & libc::FD_CLOEXEC == 0
        {
            std::process::exit(72);
        }
        std::process::exit(0);
    }

    #[tokio::test]
    async fn opencode_export_rejects_bad_session_id() {
        let dir = tempfile::tempdir().unwrap();
        // Invalid IDs must be rejected without spawning any executable.
        let bin = dir.path().join("unused-exporter");
        for bad in ["", "../escape", "id with spaces", "a;b", "$(rm -rf /)"] {
            let err = run_export_subprocess(&bin, bad, ExportLimits::default())
                .await
                .expect_err("invalid session id must fail");
            assert!(
                err.to_string().contains("invalid OpenCode session id"),
                "session id {bad:?} must be rejected before spawn, got {err:#}"
            );
        }
    }

    /// opencode_export_argv_no_shell: metacharacters in a (valid-charset)
    /// session id reach the child as ONE argv element — no shell ever
    /// interprets them. The fake exporter prints its argv verbatim.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[tokio::test]
    async fn opencode_export_argv_no_shell() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_exporter(dir.path(), r#"printf '%s|%s' "$1" "$2""#);
        let out = run_export_subprocess(&bin, "sess_1-2", ExportLimits::default())
            .await
            .expect("export runs");
        assert_eq!(String::from_utf8_lossy(&out), "export|sess_1-2");
    }

    /// Core limits are set on the exporter child, inherited by its children,
    /// and leave the caller process resource limits unchanged.
    #[cfg(unix)]
    #[tokio::test]
    async fn opencode_export_core_limits_are_zero_in_child_and_descendants() {
        let parent_core_limits = || {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: getrlimit writes to our valid rlimit and does not mutate
            // the process resource limits.
            assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut limit) }, 0);
            (limit.rlim_cur, limit.rlim_max)
        };
        let before = parent_core_limits();
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_exporter(
            dir.path(),
            r#"ulimit -Sc; ulimit -Hc; /bin/sh -c 'ulimit -Sc; ulimit -Hc'"#,
        );
        let out = run_export_subprocess(&bin, "core_limits", ExportLimits::default())
            .await
            .expect("exporter can report its inherited core limits");
        assert_eq!(
            out,
            b"0\n0\n0\n0\n",
            "both exporter and descendant must inherit zero soft/hard core limits; got {:?}",
            String::from_utf8_lossy(&out)
        );
        assert_eq!(
            parent_core_limits(),
            before,
            "parent core limits must remain unchanged"
        );
    }

    /// opencode_export_bytes_path_byte_cap: over-cap output kills the run —
    /// error, never a silent truncation.
    #[cfg(unix)]
    #[tokio::test]
    async fn opencode_export_byte_cap_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_exporter(dir.path(), "head -c 5000 /dev/zero");
        let limits = ExportLimits {
            max_bytes: 1024,
            deadline: Duration::from_secs(5),
        };
        let err = run_export_subprocess(&bin, "s1", limits)
            .await
            .expect_err("over-cap output must fail");
        assert!(format!("{err:#}").contains("byte cap"), "got {err:#}");
    }

    /// A non-terminating writer is killed by the byte cap instead of being
    /// allowed to consume disk until the much later wall-clock deadline.
    #[cfg(unix)]
    #[tokio::test]
    async fn opencode_export_byte_cap_kills_runaway_writer() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_exporter(dir.path(), "while :; do head -c 65536 /dev/zero; done");
        let limits = ExportLimits {
            max_bytes: 1024,
            deadline: Duration::from_secs(5),
        };
        let started = std::time::Instant::now();
        let err = run_export_subprocess(&bin, "s1", limits)
            .await
            .expect_err("runaway output must be killed at the byte cap");
        assert!(format!("{err:#}").contains("byte cap"), "got {err:#}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "byte cap must preempt the deadline, waited {:?}",
            started.elapsed()
        );
    }

    /// The hook runtime owns an outer deadline that can drop the exporter
    /// future before its internal deadline. A forked writer in the exporter's
    /// process group must be killed and stop mutating its inherited anonymous
    /// stdout file on that cancellation path.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[tokio::test]
    async fn opencode_export_outer_deadline_kills_and_reaps_forked_descendant() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_file = dir.path().join("outer-timeout-exporter-descendant.pid");
        let marker = dir.path().join("outer-timeout-exporter-descendant.marker");
        let pid_arg = shell_quote(&pid_file);
        let marker_arg = shell_quote(&marker);
        let bin = fake_exporter(
            dir.path(),
            &format!(
                "/bin/sh -c 'printf \"%s\\n\" \"$$\" > \"$1\"; while :; do printf x >> \"$2\"; sleep 0.02; done' sh {pid_arg} {marker_arg} &\nsleep 30"
            ),
        );
        let capture = tokio::time::timeout_at(
            tokio::time::Instant::now() + Duration::from_millis(750),
            run_export_subprocess(
                &bin,
                "s1",
                ExportLimits {
                    max_bytes: 1024 * 1024,
                    deadline: Duration::from_secs(20),
                },
            ),
        );
        let (result, pid) = tokio::join!(capture, wait_for_exporter_pid(&pid_file));
        assert!(
            result.is_err(),
            "the outer host deadline must cancel the still-live exporter"
        );
        let pid = pid.expect("forked exporter descendant must publish its PID");
        assert!(
            wait_for_exporter_reap(pid).await,
            "outer-cancelled exporter descendant was not reaped"
        );
        let before = std::fs::metadata(&marker)
            .expect("forked descendant must have written its marker")
            .len();
        tokio::time::sleep(Duration::from_millis(150)).await;
        let after = std::fs::metadata(&marker)
            .expect("marker remains inspectable after cancellation")
            .len();
        assert_eq!(
            before, after,
            "outer-cancelled descendant continued writing after its process group was killed"
        );
    }

    /// The production Linux route must remain cancellation-safe while bwrap
    /// is starting and after an exporter tries to escape with `setsid()`. The
    /// generic profile's `--new-session` is removed only for this route, so
    /// the host process-group kill covers bwrap's startup window; after bwrap
    /// launches, its PID namespace plus `--die-with-parent` covers the
    /// deliberately session-escaped writer. A short deadline is repeated to
    /// exercise both cases: each marker must either never be created or stop
    /// growing immediately after outer cancellation.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[serial_test::serial(export_sandbox_env, env)]
    async fn opencode_export_sandboxed_outer_cancel_contains_setsid_writer() {
        if !trusted_bwrap_available().await {
            eprintln!("skipped (no trusted, usable bwrap)");
            return;
        }
        let Some(setsid) = binary_on_path("setsid") else {
            eprintln!("skipped (setsid not available)");
            return;
        };

        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let xdg_data = dir.path().join("xdg-data");
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir(&home).expect("create HOME");
        std::fs::create_dir(&xdg_data).expect("create XDG data root");
        std::fs::create_dir(xdg_data.join("opencode")).expect("create OpenCode store");
        std::fs::create_dir(&bin_dir).expect("create fake exporter directory");
        let _home = EnvVarGuard::set("HOME", home.to_str().expect("utf8 HOME"));
        let _xdg_data = EnvVarGuard::set(
            "XDG_DATA_HOME",
            xdg_data.to_str().expect("utf8 XDG data root"),
        );
        let setsid = shell_quote(&setsid);
        let bin = fake_exporter(
            &bin_dir,
            &format!(
                r#"marker="$XDG_DATA_HOME/opencode/opencode-cancel-$2.marker"
token="$XDG_DATA_HOME/opencode/opencode-cancel-$2.live"
{setsid} /bin/sh -c 'while [ -e "$2" ]; do printf x >> "$1"; sleep 0.01; done' sh "$marker" "$token" &
wait"#
            ),
        );
        let limits = ExportLimits {
            max_bytes: 1024 * 1024,
            deadline: Duration::from_secs(20),
        };
        let mut writer_tokens = EscapedWriterTokens::default();

        // First prove that a real bwrap invocation reaches the escaping
        // writer, then cancel it through the same outer deadline that hook
        // capture owns. This prevents a skip-green test that only times out
        // before the exporter begins to run.
        let settled_marker = xdg_data.join("opencode/opencode-cancel-settled.marker");
        let settled_token = xdg_data.join("opencode/opencode-cancel-settled.live");
        writer_tokens.arm(settled_token.clone());
        let capture = tokio::time::timeout_at(
            tokio::time::Instant::now() + Duration::from_millis(750),
            run_export_subprocess_sandboxed_for_test(&bin, "settled", limits),
        );
        let (result, started) = tokio::join!(capture, wait_for_marker_len(&settled_marker));
        assert!(
            result.is_err(),
            "the outer capture deadline must cancel a live sandboxed exporter: {result:?}"
        );
        let before = started.expect("sandboxed setsid writer must reach its marker before cancel");
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            std::fs::metadata(&settled_marker)
                .expect("settled marker remains inspectable")
                .len(),
            before,
            "a sandboxed writer escaped after outer cancellation"
        );
        // Keep the escapee's token live through the stability observation:
        // deleting it earlier would make a surviving writer stop itself and
        // turn this outer-cancellation regression into a false green. The
        // token guard still removes it if an assertion above unwinds.
        writer_tokens.stop(&settled_token);

        // Stress the startup boundary. Every iteration is cancelled before
        // the helper's own deadline, and no writer may materialize after the
        // outer future has been dropped.
        for round in 0..12 {
            let session_id = format!("startup-{round}");
            let marker = xdg_data.join(format!("opencode/opencode-cancel-{session_id}.marker"));
            let token = xdg_data.join(format!("opencode/opencode-cancel-{session_id}.live"));
            writer_tokens.arm(token.clone());
            let result = tokio::time::timeout_at(
                tokio::time::Instant::now() + Duration::from_millis(20),
                run_export_subprocess_sandboxed_for_test(&bin, &session_id, limits),
            )
            .await;
            assert!(
                result.is_err(),
                "outer startup deadline must cancel round {round}: {result:?}"
            );
            let before = std::fs::metadata(&marker)
                .ok()
                .map(|metadata| metadata.len());
            tokio::time::sleep(Duration::from_millis(150)).await;
            let after = std::fs::metadata(&marker)
                .ok()
                .map(|metadata| metadata.len());
            assert_eq!(
                before, after,
                "a cancelled bwrap startup launched or retained an OpenCode writer in round {round}"
            );
            // As above, release the fixture escapee only after proving that
            // outer cancellation, not this cleanup token, stopped it.
            writer_tokens.stop(&token);
        }
    }

    /// Codex M3 R2 P1-1: a `setsid()`-escaped descendant leaves the child's
    /// process group (so the group-liveness probe cannot see it), yet the
    /// over-cap bytes it writes to the inherited stdout are still refused —
    /// the byte cap is enforced on the bytes, not on group membership. Skips
    /// when `setsid` is unavailable.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[tokio::test]
    async fn opencode_export_setsid_escapee_cannot_exceed_cap() {
        if binary_on_path("setsid").is_none() {
            eprintln!("skipped (setsid not available)");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // The escapee runs in its OWN session (setsid) and floods the inherited
        // stdout far past the cap; the parent lingers so those bytes land, then
        // exits success. Pre-P1 the group-liveness probe would miss the escapee
        // and accept the file; the bounded read + recheck now refuses it.
        let bin = fake_exporter(
            dir.path(),
            "setsid sh -c 'head -c 200000 /dev/zero' ; sleep 0.2 ; exit 0",
        );
        let err = run_export_subprocess(
            &bin,
            "s1",
            ExportLimits {
                max_bytes: 1024,
                deadline: Duration::from_secs(5),
            },
        )
        .await
        .expect_err("group-escaped over-cap output must be refused");
        assert!(format!("{err:#}").contains("byte cap"), "got {err:#}");
    }

    /// Codex M3 R3 P1: a "bwrap" living under a user-writable path (a tempdir,
    /// whose ancestry the invoking user can rewrite) must be refused — a
    /// planted or post-check-swapped helper could otherwise run the exporter
    /// unsandboxed. The env-free integrity core walks the ancestry, so this
    /// holds whether the test user is root or not.
    #[cfg(target_os = "linux")]
    #[test]
    fn validate_trusted_bwrap_refuses_untrusted_helper() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("bwrap");
        std::fs::write(&fake, "#!/bin/sh\nexec \"$@\"\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = validate_trusted_bwrap(&fake)
            .expect_err("user-writable sandbox helper must be refused");
        assert!(format!("{err:#}").contains("refusing"), "got {err:#}");
    }

    /// Accept a packaged bwrap that is not user-writable independently of
    /// whether it supports this export's descriptor-native mount capability.
    /// The strict capability check is covered by `trusted_bwrap_preflight`.
    #[cfg(target_os = "linux")]
    #[test]
    fn validate_trusted_bwrap_accepts_system_binary() {
        let Some(bwrap) = which_bwrap() else {
            eprintln!("skipped (no bwrap on PATH)");
            return;
        };
        let canonical = bwrap.canonicalize().expect("resolve the system bwrap path");
        match validate_trusted_bwrap(&bwrap) {
            Ok(trusted) => assert_eq!(trusted, canonical),
            Err(_) => eprintln!("skipped (system bwrap is under a user-writable path here)"),
        }
    }

    #[cfg(target_os = "linux")]
    fn fd_inode(fd: &std::os::fd::OwnedFd) -> u64 {
        use std::os::fd::AsRawFd;
        // SAFETY: fstat on our own valid fd into a zeroed stat buffer.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::fstat(fd.as_raw_fd(), &mut st) }, 0, "fstat");
        st.st_ino as u64
    }

    /// Codex M3 R4 P1: the store pin is a SINGLE atomic `openat`, so a
    /// concurrent rename of `opencode` AFTER the pin cannot make the pinned fd
    /// refer to a different directory — the bound inode stays the checked one.
    /// A symlinked entry is refused at pin time (`O_NOFOLLOW`).
    #[cfg(target_os = "linux")]
    #[test]
    #[serial_test::serial(env)]
    fn pin_store_under_captures_inode_atomically() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let store = base.join("opencode");
        std::fs::create_dir(&store).unwrap();
        let original_ino = std::fs::metadata(&store).unwrap().ino();

        let fd = pin_store_under(base).expect("pin real opencode dir");
        assert_eq!(
            fd_inode(&fd),
            original_ino,
            "pin must capture the real store"
        );

        // Swap a DIFFERENT directory over `opencode` after the pin (the empty
        // target dir is replaced by rename). The pinned fd must not follow it.
        let sensitive = base.join("sensitive");
        std::fs::create_dir(&sensitive).unwrap();
        std::fs::write(sensitive.join("secret"), "s").unwrap();
        std::fs::rename(&sensitive, &store).unwrap();
        assert_ne!(
            std::fs::metadata(&store).unwrap().ino(),
            original_ino,
            "the swap must have replaced the path's inode"
        );
        assert_eq!(
            fd_inode(&fd),
            original_ino,
            "pinned fd must still refer to the ORIGINAL store, not the swapped-in dir"
        );

        // A symlinked `opencode` entry is refused at pin time (O_NOFOLLOW).
        std::fs::remove_dir_all(&store).unwrap();
        std::os::unix::fs::symlink(base.join("elsewhere"), &store).unwrap();
        assert!(
            pin_store_under(base).is_err(),
            "symlinked opencode entry must be refused at pin time"
        );
    }

    /// The pinned store uses bwrap's descriptor-native bind and its setup fd
    /// is consumed before payload exec: the payload cannot traverse the
    /// known fd (or its host parent) while its fixed `/mnt` store remains RW.
    /// Route through `run_bounded_exporter`, not a direct `Command`, so this
    /// exercises the production fork-child CLOEXEC sweep and allowlist.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[serial_test::serial(env)]
    async fn pin_store_binds_rw_through_bwrap() {
        use std::os::fd::AsRawFd;
        if !trusted_bwrap_available().await {
            eprintln!("skipped (no trusted, usable bwrap)");
            return;
        }
        let bwrap =
            resolve_trusted_bwrap_until(tokio::time::Instant::now() + Duration::from_secs(3))
                .await
                .expect("resolve trusted bwrap with descriptor-native mount support");
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("opencode")).unwrap();
        std::fs::write(tmp.path().join("host-only-sibling"), "must-not-read").unwrap();

        let fd = pin_store_under(tmp.path()).expect("pin real dir");
        let raw_fd = fd.as_raw_fd();
        let fd_text = raw_fd.to_string();
        let payload_probe = format!(
            "test ! -e /proc/self/fd/{raw_fd} && \
             test ! -e /proc/self/fd/{raw_fd}/../host-only-sibling && \
             for candidate in /proc/self/fd/[0-9]*; do \
               test ! -e \"$candidate/../host-only-sibling\" || exit 97; \
             done; \
             {{ printf mounted > /mnt/probe; printf mounted; }}"
        );
        let args = vec![
            "--unshare-all".to_string(),
            "--die-with-parent".to_string(),
            "--ro-bind".to_string(),
            "/".to_string(),
            "/".to_string(),
            "--bind-fd".to_string(),
            fd_text,
            "/mnt".to_string(),
            "--".to_string(),
            "/bin/sh".to_string(),
            "-c".to_string(),
            payload_probe,
        ];
        let output = run_bounded_exporter(
            &bwrap,
            &args,
            "store-probe",
            ExportLimits::default(),
            tokio::time::Instant::now() + ExportLimits::default().deadline,
            vec![fd],
        )
        .await
        .expect("bwrap must consume the store capability before payload exec");
        assert_eq!(output, b"mounted");
        assert_eq!(
            std::fs::read(tmp.path().join("opencode/probe"))
                .expect("child write must land on the host store via the pinned fd"),
            b"mounted",
            "the fixed writable mount must carry the exact payload marker"
        );
    }

    /// Deadline kills a hung exporter; the wait stays bounded.
    #[cfg(unix)]
    #[tokio::test]
    async fn opencode_export_deadline_kills_hung_exporter() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_exporter(dir.path(), "sleep 30");
        let limits = ExportLimits {
            max_bytes: 1024,
            deadline: Duration::from_millis(300),
        };
        let started = std::time::Instant::now();
        let err = run_export_subprocess(&bin, "s1", limits)
            .await
            .expect_err("hung exporter must be killed");
        assert!(format!("{err:#}").contains("deadline"), "got {err:#}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "kill must be prompt, waited {:?}",
            started.elapsed()
        );
    }

    /// A failing exporter omits its arbitrary stderr from the error chain.
    #[cfg(unix)]
    #[tokio::test]
    async fn opencode_export_failure_omits_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_exporter(
            dir.path(),
            "echo 'PRIVATE-EXPORTER-STDERR-SENTINEL-9d6c' >&2; exit 3",
        );
        let err = run_export_subprocess(&bin, "s1", ExportLimits::default())
            .await
            .expect_err("non-zero exit must fail");
        let text = format!("{err:#}");
        assert!(
            !text.contains("PRIVATE-EXPORTER-STDERR-SENTINEL-9d6c"),
            "arbitrary exporter stderr leaked: {text}"
        );
        assert_eq!(
            text,
            "opencode export failed with a non-zero exit status; exporter diagnostics omitted"
        );
    }

    /// opencode_export_offline_sandbox_profile: the bwrap Required profile
    /// actually runs an exporter offline — network is unshared (a connect
    /// attempt fails instantly), HOME/XDG are fixed private tmpfs paths, only
    /// the descriptor-pinned OpenCode store is writable, and stdout flows
    /// through the same bounds. Skips when bwrap is unavailable (the
    /// production path then fails closed).
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[serial_test::serial(env)]
    async fn opencode_export_offline_sandbox_profile() {
        // Detect "trusted AND usable", not merely present (Codex M3 R3): a
        // bwrap under a user-writable path is refused by the integrity policy,
        // so the sandbox would degrade — skip rather than assert success.
        if !trusted_bwrap_available().await {
            eprintln!("skipped (no trusted, usable bwrap)");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let ambient_home = dir.path().join("ambient-home");
        let xdg_data = dir.path().join("ambient-xdg-data");
        let xdg_config = dir.path().join("ambient-xdg-config");
        std::fs::create_dir(&ambient_home).expect("create ambient HOME");
        std::fs::create_dir(&xdg_data).expect("create ambient XDG data");
        std::fs::create_dir(&xdg_config).expect("create ambient XDG config");
        std::fs::create_dir(xdg_data.join("opencode")).expect("create OpenCode store");
        // This sibling is reachable only by traversing `..` from the pinned
        // host store descriptor. A payload that inherits that setup fd could
        // escape the fixed writable mount via openat(fd, ".."), so the fake
        // exporter scans all visible descriptors for this exact sentinel.
        let parent_escape_sentinel = "libra-opencode-parent-fd-escape-sentinel";
        std::fs::write(xdg_data.join(parent_escape_sentinel), "must-not-read")
            .expect("write parent traversal sentinel");
        let ambient_secret = ambient_home.join("private-token");
        std::fs::write(&ambient_secret, "ambient-secret").expect("write ambient secret");
        let _home = EnvVarGuard::set("HOME", ambient_home.to_str().expect("utf8 HOME"));
        let _xdg_data =
            EnvVarGuard::set("XDG_DATA_HOME", xdg_data.to_str().expect("utf8 XDG data"));
        let _xdg_config = EnvVarGuard::set(
            "XDG_CONFIG_HOME",
            xdg_config.to_str().expect("utf8 XDG config"),
        );
        let ambient_secret = shell_quote(&ambient_secret);
        // The fake exporter proves that fixed paths replaced the ambient
        // values, its known ambient secret is absent, and only the mounted
        // OpenCode store remains writable.
        let bin = fake_exporter(
            dir.path(),
            &format!(
                r#"test "$HOME" = "{OPENCODE_SANDBOX_HOME}" || {{ echo wrong-home >&2; exit 4; }}
test "$XDG_DATA_HOME" = "{OPENCODE_SANDBOX_DATA_HOME}" || {{ echo wrong-data >&2; exit 5; }}
test "$XDG_CONFIG_HOME" = "{OPENCODE_SANDBOX_CONFIG_HOME}" || {{ echo wrong-config >&2; exit 6; }}
test ! -e {ambient_secret} || {{ echo ambient-secret-visible >&2; exit 7; }}
for fd in /proc/self/fd/[0-9]*; do
  test ! -e "$fd/../{parent_escape_sentinel}" || {{ echo store-fd-escaped >&2; exit 9; }}
done
printf store-write > "$XDG_DATA_HOME/opencode/libra-write-probe" || {{ echo store-unwritable >&2; exit 8; }}
printf '{{"info":{{}},"messages":[]}}'"#
            ),
        );
        let out = run_export_subprocess_sandboxed_for_test(&bin, "sess-1", ExportLimits::default())
            .await
            .expect("sandboxed export must run offline");
        assert_eq!(
            String::from_utf8_lossy(&out),
            r#"{"info":{},"messages":[]}"#
        );
        assert_eq!(
            std::fs::read_to_string(xdg_data.join("opencode/libra-write-probe"))
                .expect("read host store write"),
            "store-write",
            "the sole writable descriptor mount must reach the pinned store"
        );

        // Network must be unshared: a resolver/socket attempt fails fast.
        let net_bin = fake_exporter(
            dir.path(),
            r#"if command -v getent >/dev/null 2>&1; then
  getent hosts example.com >/dev/null 2>&1 && { echo net-open >&2; exit 6; }
fi
printf 'offline-ok'"#,
        );
        let out =
            run_export_subprocess_sandboxed_for_test(&net_bin, "sess-2", ExportLimits::default())
                .await
                .expect("offline probe must succeed");
        assert_eq!(String::from_utf8_lossy(&out), "offline-ok");
    }

    /// Re-exec the assertion on a PTY-owning process, because ordinary CI often
    /// has no controlling terminal and would make `/dev/tty` fail even if the
    /// exporter inherited one. The outer test is gated on a real trusted bwrap
    /// so a raw test runner cannot accidentally certify the production boundary.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[serial_test::serial(export_sandbox_env, env)]
    async fn opencode_export_sandboxed_detaches_terminal_session() {
        if !trusted_bwrap_available().await {
            eprintln!("skipped (no trusted, usable bwrap)");
            return;
        }
        let (success, transcript) = run_opencode_export_pty_driver();
        assert!(
            success,
            "PTY-owning OpenCode exporter driver failed: {transcript}"
        );
        assert!(
            transcript.contains("1 passed; 0 failed"),
            "PTY-owning driver did not run its assertion: {transcript}"
        );
    }

    /// Driver for [`opencode_export_sandboxed_detaches_terminal_session`].
    /// It executes only under the parent-created PTY and first proves that the
    /// driver itself owns a controlling terminal before exercising the real
    /// Required bwrap exporter boundary.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn opencode_export_sandboxed_detaches_terminal_session_driver() {
        if std::env::var_os(OPENCODE_EXPORT_PTY_DRIVER).is_none() {
            return;
        }
        let _driver_tty = std::fs::File::open("/dev/tty")
            .expect("PTY driver must prove it owns a controlling terminal");
        assert!(
            trusted_bwrap_available().await,
            "parent must gate the PTY driver on a trusted, usable bwrap"
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let xdg_data = dir.path().join("xdg-data");
        let xdg_config = dir.path().join("xdg-config");
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir(&home).expect("create HOME");
        std::fs::create_dir(&xdg_data).expect("create XDG data root");
        std::fs::create_dir(&xdg_config).expect("create XDG config root");
        std::fs::create_dir(&bin_dir).expect("create fake exporter directory");
        let _home = EnvVarGuard::set("HOME", home.to_str().expect("utf8 HOME"));
        let _xdg_data = EnvVarGuard::set(
            "XDG_DATA_HOME",
            xdg_data.to_str().expect("utf8 XDG data root"),
        );
        let _xdg_config = EnvVarGuard::set(
            "XDG_CONFIG_HOME",
            xdg_config.to_str().expect("utf8 XDG config root"),
        );
        let bin = fake_exporter(
            &bin_dir,
            r#"if /bin/sh -c 'exec </dev/tty' >/dev/null 2>&1; then
  printf 'tty=attached'
  exit 91
fi
printf 'tty=detached'"#,
        );
        let output = run_export_subprocess_sandboxed_for_test(
            &bin,
            "terminal-session",
            ExportLimits::default(),
        )
        .await
        .expect("trusted bwrap must run a terminal-detached exporter");
        let output = String::from_utf8(output).expect("fixture output is utf-8");
        assert_eq!(
            output, "tty=detached",
            "exporter retained the PTY driver's controlling terminal: {output:?}"
        );
    }

    /// Untrusted binary: no trust record → capability unavailable with an
    /// actionable hint (fail-closed; no PATH fallback). Pinned against the
    /// injectable core (GC-DR-07) — the process-wide config store may
    /// legitimately trust opencode on a dev machine, and its connection is
    /// cached process-wide, so env isolation cannot work here; the
    /// record-present path is exercised by the live agent gate.
    #[tokio::test]
    async fn opencode_export_untrusted_binary_fails_closed() {
        let err = trusted_opencode_binary_from(None)
            .await
            .expect_err("no trust record must fail closed");
        assert!(format!("{err:#}").contains("not trusted"), "got {err:#}");
    }

    /// SBX-03: execution stays `run_bounded_exporter` (file-backed stdout
    /// with the 16 MiB poll cap, per-OS RLIMIT_FSIZE — strict on Linux, 8
    /// GiB backstop on macOS (FIX-SBX-01) — RLIMIT_CORE=0 in the child,
    /// process group, 3s wall clock).
    /// Linux retained exporter/store fds remain CLOEXEC in the parent until
    /// the fork-child allowlist restores only those exact capabilities.
    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    #[serial_test::serial(env)]
    async fn runner_controls_preserved() {
        assert_eq!(EXPORT_MAX_BYTES, 16 * 1024 * 1024);
        assert_eq!(EXPORT_DEADLINE, Duration::from_secs(3));

        let dir = tempfile::tempdir().unwrap();
        let bin = fake_exporter(dir.path(), r#"printf 'ok'"#);
        let default_deadline = tokio::time::Instant::now() + ExportLimits::default().deadline;
        let out = run_bounded_exporter(
            &bin,
            &[],
            "sess1",
            ExportLimits::default(),
            default_deadline,
            Vec::new(),
        )
        .await
        .expect("run_bounded_exporter still executes");
        assert_eq!(out, b"ok");

        let over = fake_exporter(dir.path(), "head -c 5000 /dev/zero");
        let over_limits = ExportLimits {
            max_bytes: 1024,
            deadline: Duration::from_secs(5),
        };
        let err = run_bounded_exporter(
            &over,
            &[],
            "s1",
            over_limits,
            tokio::time::Instant::now() + over_limits.deadline,
            Vec::new(),
        )
        .await
        .expect_err("byte cap must still fail closed");
        assert!(format!("{err:#}").contains("byte cap"), "got {err:#}");

        let hung = fake_exporter(dir.path(), "sleep 30");
        let started = std::time::Instant::now();
        let hung_limits = ExportLimits {
            max_bytes: 1024,
            deadline: Duration::from_millis(300),
        };
        let err = run_bounded_exporter(
            &hung,
            &[],
            "s1",
            hung_limits,
            tokio::time::Instant::now() + hung_limits.deadline,
            Vec::new(),
        )
        .await
        .expect_err("deadline must still kill");
        assert!(format!("{err:#}").contains("deadline"), "got {err:#}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "deadline kill must be prompt, waited {:?}",
            started.elapsed()
        );

        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let tmp = tempfile::tempdir().unwrap();
            std::fs::create_dir(tmp.path().join("opencode")).unwrap();
            let fd = pin_store_under(tmp.path()).expect("pin store");
            assert!(
                fd.as_raw_fd() >= 3,
                "pinned store capability must never occupy stdin/stdout/stderr"
            );
            let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
            assert!(flags >= 0, "F_GETFD on pinned store fd");
            assert_ne!(
                flags & libc::FD_CLOEXEC,
                0,
                "parent capability fd must remain CLOEXEC until the exact pre-exec allowlist"
            );
            let ok = fake_exporter(dir.path(), r#"printf 'pinned'"#);
            let default_deadline = tokio::time::Instant::now() + ExportLimits::default().deadline;
            let out = run_bounded_exporter(
                &ok,
                &[],
                "s1",
                ExportLimits::default(),
                default_deadline,
                vec![fd],
            )
            .await
            .expect("runner must accept caller-held keep_fds");
            assert_eq!(out, b"pinned");
        }
    }

    /// The hook deadline is absolute across trust, probe, assembly, and the
    /// runner. A caller that reaches the runner after its budget is gone must
    /// not create an exporter merely to discover that its relative timeout is
    /// already exhausted.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[tokio::test]
    async fn bounded_runner_refuses_elapsed_absolute_deadline_without_spawning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("must-not-spawn");
        let exporter = fake_exporter(
            dir.path(),
            &format!("printf spawned > {}", shell_quote(&marker)),
        );
        let limits = ExportLimits {
            max_bytes: 1024,
            deadline: Duration::from_secs(5),
        };
        let error = run_bounded_exporter(
            &exporter,
            &[],
            "deadline-expired",
            limits,
            tokio::time::Instant::now() - Duration::from_millis(1),
            Vec::new(),
        )
        .await
        .expect_err("an elapsed absolute deadline must refuse the spawn");
        assert!(
            format!("{error:#}").contains("deadline already elapsed"),
            "unexpected error: {error:#}"
        );
        assert!(
            !marker.exists(),
            "an elapsed capture deadline must not spawn an exporter"
        );
    }

    /// macOS is rejected before the generic seatbelt transform or any
    /// trusted-looking exporter can run. Seatbelt does not provide the
    /// cancellation-safe descendant containment required for this bridge.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_export_is_rejected_before_seatbelt_transform() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("must-not-run");
        let bin = fake_exporter(tmp.path(), &format!("touch {}", marker.display()));
        let error = run_export_subprocess_sandboxed(&bin, "sess", ExportLimits::default())
            .await
            .expect_err("macOS export must be rejected before the transform");
        let text = format!("{error:#}");
        assert!(
            text.contains("unsupported on macOS") && text.contains("fail-closed"),
            "got {text}"
        );
        assert!(!marker.exists(), "rejected exporter must not execute");
    }

    /// The macOS rejection does not depend on whether the deprecated
    /// `sandbox-exec` binary happens to be installed: it is fail-closed for
    /// all hosts, before an exporter is spawned.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_export_remains_unsupported_with_or_without_seatbelt() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_exporter(dir.path(), r#"printf 'should-not-run'"#);
        let err = run_export_subprocess_sandboxed(&bin, "sess", ExportLimits::default())
            .await
            .expect_err("macOS export must fail closed");
        let text = format!("{err:#}");
        assert!(
            text.contains("unsupported on macOS") && text.contains("fail-closed"),
            "got {text}"
        );
        assert!(
            !text.contains("unsandboxed export ran"),
            "must not fall back to unsandboxed execution: {text}"
        );
    }

    /// SBX-03 D-group: a trusted, usable bwrap must be present. Missing or
    /// user-writable bwrap is a hard failure (never a skip-green). The broad
    /// self-hosted compatibility suite does not guarantee this environment;
    /// opencode-export-linux explicitly runs this ignored gate with
    /// `--include-ignored` on its pinned Linux runner.
    #[cfg(target_os = "linux")]
    #[ignore = "Linux D-group environment gate; run via opencode-export-linux"]
    #[serial_test::serial(export_sandbox_env)]
    #[tokio::test]
    async fn trusted_bwrap_preflight() {
        let bwrap = resolve_trusted_bwrap_unprobed()
            .expect("Linux D-group requires a trusted bwrap binary on PATH");
        assert!(
            trusted_bwrap_supports_fd_mounts_until(
                &bwrap,
                tokio::time::Instant::now() + Duration::from_millis(500),
            )
            .await,
            "Linux D-group requires a usable FD-mount bwrap at {}",
            bwrap.display()
        );
    }

    #[cfg(target_os = "linux")]
    fn system_true_binary() -> PathBuf {
        for candidate in ["/usr/bin/true", "/bin/true"] {
            let path = PathBuf::from(candidate);
            if path.is_file() {
                return path;
            }
        }
        panic!("no /usr/bin/true or /bin/true on this Linux host");
    }

    #[cfg(target_os = "linux")]
    fn bwrap_bind_fd_index(args: &[String], dest: &str) -> Option<usize> {
        args.windows(3).position(|window| {
            window[0] == "--bind-fd"
                && window[1].as_bytes().iter().all(u8::is_ascii_digit)
                && window[2] == dest
        })
    }

    #[cfg(target_os = "linux")]
    fn bwrap_ro_bind_index(args: &[String], path: &str) -> Option<usize> {
        args.windows(3)
            .position(|w| w[0] == "--ro-bind" && w[1] == path && w[2] == path)
    }

    #[cfg(target_os = "linux")]
    fn bwrap_ro_bind_fd_index(args: &[String], destination: &str) -> Option<usize> {
        args.windows(3).position(|window| {
            window[0] == "--ro-bind-fd"
                && window[1].as_bytes().iter().all(u8::is_ascii_digit)
                && window[2] == destination
        })
    }

    /// SBX-03: new-path argv exposes no ambient HOME/XDG tree. It creates the
    /// fixed private tmpfs layout, binds only the sealed exporter and
    /// descriptor-pinned store at that layout, retains no exporter parent
    /// path, then invokes the exporter after `--`.
    #[cfg(target_os = "linux")]
    #[test]
    #[serial_test::serial(export_sandbox_env, env)]
    // Default wraps named lanes so concurrent bridges cannot form an ABBA.
    #[serial_test::serial]
    fn bwrap_argv_equivalent() {
        use std::os::fd::AsRawFd;

        let tmp = tempfile::tempdir().unwrap();
        let xdg_data = tmp.path().join("xdg-data");
        let xdg_config = tmp.path().join("xdg-config");
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir(&xdg_data).unwrap();
        std::fs::create_dir(&xdg_config).unwrap();
        std::fs::create_dir(&bin_dir).unwrap();
        std::fs::create_dir(xdg_data.join("opencode")).unwrap();
        let bin = fake_exporter(&bin_dir, r#"printf 'ok'"#);

        // A hostile root HOME must neither become a host mount nor alter the
        // in-sandbox fixed environment. XDG still provides the pinned store.
        let _home = EnvVarGuard::set("HOME", "/");
        let _xdg_data = EnvVarGuard::set("XDG_DATA_HOME", xdg_data.to_str().expect("utf8 data"));
        let _xdg_config =
            EnvVarGuard::set("XDG_CONFIG_HOME", xdg_config.to_str().expect("utf8 config"));

        let trusted = system_true_binary();
        let exporter_fd = pin_exporter_fd(&bin, None).expect("pin fake exporter for test");
        let assembled = assemble_sandboxed_export(exporter_fd, Some(&trusted))
            .expect("assemble export sandbox");
        let args = &assembled.pre_args;
        let exporter_fd_number = assembled
            .keep_fds
            .first()
            .expect("sealed exporter capability retained")
            .as_raw_fd()
            .to_string();
        let store_fd_number = assembled
            .keep_fds
            .get(1)
            .expect("pinned store capability retained")
            .as_raw_fd()
            .to_string();
        let data_s = xdg_data.to_string_lossy().into_owned();
        let config_s = xdg_config.to_string_lossy().into_owned();
        let cwd_i = bwrap_ro_bind_index(args, "/usr").expect("fixed sandbox cwd ro-bind");
        let exporter_i = bwrap_ro_bind_fd_index(args, OPENCODE_SANDBOX_EXPORTER)
            .expect("sealed exporter read-only FD bind");
        let store_i = bwrap_bind_fd_index(args, OPENCODE_SANDBOX_STORE)
            .expect("FD-pinned store writable-bind");
        assert!(
            bwrap_ro_bind_index(args, &data_s).is_none()
                && bwrap_ro_bind_index(args, &config_s).is_none()
                && bwrap_ro_bind_index(args, bin_dir.to_string_lossy().as_ref()).is_none()
                && bwrap_ro_bind_index(args, "/").is_none(),
            "ambient HOME/XDG and exporter-parent roots must never be read-only mounted: {args:?}"
        );
        assert!(
            cwd_i < store_i,
            "fixed safe sandbox cwd must be bound before the store overlay; \
             cwd={cwd_i} store={store_i}"
        );
        assert!(
            exporter_i < store_i,
            "sealed exporter FD bind must precede the writable store bind; exporter={exporter_i} store={store_i}"
        );
        assert_eq!(
            args.windows(3)
                .filter(|window| {
                    window[0] == "--ro-bind-fd"
                        && window[1] == exporter_fd_number
                        && window[2] == OPENCODE_SANDBOX_EXPORTER
                })
                .count(),
            1,
            "exactly one sealed exporter read-only FD bind is allowed: {args:?}"
        );
        assert_eq!(
            args.windows(3)
                .filter(|window| {
                    window[0] == "--bind-fd"
                        && window[1] == store_fd_number
                        && window[2] == OPENCODE_SANDBOX_STORE
                })
                .count(),
            1,
            "exactly one pinned store FD bind is allowed: {args:?}"
        );
        assert!(
            !args.windows(3).any(|window| {
                (window[0] == "--ro-bind" || window[0] == "--bind")
                    && window[1].starts_with("/proc/self/fd/")
            }),
            "path-string descriptor mounts can leak setup fds into payload: {args:?}"
        );
        for private_dir in opencode_sandbox_private_dirs() {
            assert!(
                args.windows(2).any(|window| {
                    window[0] == "--dir" && window[1] == private_dir.to_string_lossy()
                }),
                "private exporter directory {} must be created inside tmpfs: {args:?}",
                private_dir.display()
            );
        }
        let sep = args
            .iter()
            .position(|a| a == "--")
            .expect("bwrap argv must contain --");
        assert_eq!(
            args.iter()
                .filter(|argument| *argument == "--new-session")
                .count(),
            0,
            "OpenCode must remove the generic bwrap session split after assembly: {args:?}"
        );
        assert_eq!(
            args.iter()
                .filter(|argument| *argument == "--unshare-all")
                .count(),
            1,
            "OpenCode must retain exactly one bwrap PID-namespace control: {args:?}"
        );
        assert_eq!(
            args.iter()
                .filter(|argument| *argument == "--die-with-parent")
                .count(),
            1,
            "OpenCode must retain exactly one bwrap parent-death control: {args:?}"
        );
        assert!(store_i < sep, "store bind must precede --");
        assert_eq!(
            args.get(sep + 1).map(String::as_str),
            Some(OPENCODE_SANDBOX_EXPORTER),
            "command tail must be the fixed sealed exporter target; args={args:?}"
        );
        let program = assembled
            .program
            .canonicalize()
            .unwrap_or_else(|_| assembled.program.clone());
        let expected = trusted.canonicalize().unwrap_or_else(|_| trusted.clone());
        assert_eq!(
            program,
            expected,
            "program must be the injected trusted path, got {}",
            assembled.program.display()
        );
        assert_eq!(
            assembled.keep_fds.len(),
            2,
            "exporter and store pins must both remain open through bwrap spawn"
        );
    }

    /// The OpenCode-only argv adjustment accepts only the known Required
    /// profile grammar, removes exactly one startup-racy session flag, and
    /// fails closed on a drifted or capability-widened profile rather than
    /// guessing which flag to remove.
    #[test]
    fn opencode_bwrap_argv_requires_canonical_containment_grammar() {
        let exporter = 8_i32;
        let read_only_bind = PathBuf::from("/usr");
        let private_dirs = opencode_sandbox_private_dirs();
        let writable_destination = PathBuf::from("/var/lib/opencode");
        let writable_bind = (7_i32, writable_destination.clone());
        let baseline = || {
            vec![
                "/usr/bin/bwrap".to_string(),
                "--unshare-all".to_string(),
                "--die-with-parent".to_string(),
                "--new-session".to_string(),
                "--unshare-net".to_string(),
                "--proc".to_string(),
                "/proc".to_string(),
                "--dev".to_string(),
                "/dev".to_string(),
                "--tmpfs".to_string(),
                "/tmp".to_string(),
                "--ro-bind".to_string(),
                "/usr".to_string(),
                "/usr".to_string(),
                "--dir".to_string(),
                OPENCODE_SANDBOX_ROOT.to_string(),
                "--dir".to_string(),
                OPENCODE_SANDBOX_HOME.to_string(),
                "--dir".to_string(),
                OPENCODE_SANDBOX_DATA_HOME.to_string(),
                "--dir".to_string(),
                OPENCODE_SANDBOX_CONFIG_HOME.to_string(),
                "--dir".to_string(),
                OPENCODE_SANDBOX_BIN_DIR.to_string(),
                "--dir".to_string(),
                OPENCODE_SANDBOX_STORE.to_string(),
                "--ro-bind-fd".to_string(),
                "8".to_string(),
                OPENCODE_SANDBOX_EXPORTER.to_string(),
                "--bind-fd".to_string(),
                "7".to_string(),
                writable_destination.to_string_lossy().into_owned(),
                "--".to_string(),
                OPENCODE_SANDBOX_EXPORTER.to_string(),
            ]
        };

        let mut valid = baseline();
        remove_opencode_bwrap_new_session(
            &mut valid,
            exporter,
            std::slice::from_ref(&read_only_bind),
            &private_dirs,
            std::slice::from_ref(&writable_bind),
        )
        .expect("the canonical Required profile is accepted");
        assert!(
            !valid.iter().any(|argument| argument == "--new-session"),
            "the OpenCode argv must not inherit the generic session split"
        );
        assert!(valid.iter().any(|argument| argument == "--unshare-all"));
        assert!(valid.iter().any(|argument| argument == "--die-with-parent"));

        let mut missing = baseline();
        missing.retain(|argument| argument != "--new-session");
        assert!(
            remove_opencode_bwrap_new_session(
                &mut missing,
                exporter,
                std::slice::from_ref(&read_only_bind),
                &private_dirs,
                std::slice::from_ref(&writable_bind),
            )
            .is_err(),
            "missing shared flag is profile drift and must fail closed"
        );

        let mut duplicate = baseline();
        duplicate.insert(4, "--new-session".to_string());
        assert!(
            remove_opencode_bwrap_new_session(
                &mut duplicate,
                exporter,
                std::slice::from_ref(&read_only_bind),
                &private_dirs,
                std::slice::from_ref(&writable_bind),
            )
            .is_err(),
            "duplicate shared flags are ambiguous and must fail closed"
        );

        let mut missing_parent_death = baseline();
        missing_parent_death.retain(|argument| argument != "--die-with-parent");
        assert!(
            remove_opencode_bwrap_new_session(
                &mut missing_parent_death,
                exporter,
                std::slice::from_ref(&read_only_bind),
                &private_dirs,
                std::slice::from_ref(&writable_bind),
            )
            .is_err(),
            "OpenCode must never emit a profile without parent-death containment"
        );

        let mut share_net = baseline();
        let net_option = share_net
            .iter()
            .position(|argument| argument == "--unshare-net")
            .expect("baseline network isolation");
        share_net[net_option] = "--share-net".to_string();
        assert!(
            remove_opencode_bwrap_new_session(
                &mut share_net,
                exporter,
                std::slice::from_ref(&read_only_bind),
                &private_dirs,
                std::slice::from_ref(&writable_bind),
            )
            .is_err(),
            "network sharing is an OpenCode capability escalation and must fail closed"
        );

        let mut injected_unknown = baseline();
        let delimiter = injected_unknown
            .iter()
            .position(|argument| argument == "--")
            .expect("baseline delimiter");
        injected_unknown.splice(
            delimiter..delimiter,
            ["--cap-add".to_string(), "CAP_SYS_ADMIN".to_string()],
        );
        assert!(
            remove_opencode_bwrap_new_session(
                &mut injected_unknown,
                exporter,
                std::slice::from_ref(&read_only_bind),
                &private_dirs,
                std::slice::from_ref(&writable_bind),
            )
            .is_err(),
            "an unknown capability option must fail closed"
        );

        let mut second_delimiter = baseline();
        second_delimiter.insert(1, "--".to_string());
        assert!(
            remove_opencode_bwrap_new_session(
                &mut second_delimiter,
                exporter,
                std::slice::from_ref(&read_only_bind),
                &private_dirs,
                std::slice::from_ref(&writable_bind),
            )
            .is_err(),
            "a second command delimiter must fail closed"
        );

        let mut broad_read_bind = baseline();
        let bind_source = broad_read_bind
            .windows(3)
            .position(|window| window == ["--ro-bind", "/usr", "/usr"])
            .expect("baseline read-only bind");
        broad_read_bind[bind_source + 1] = "/".to_string();
        broad_read_bind[bind_source + 2] = "/".to_string();
        assert!(
            remove_opencode_bwrap_new_session(
                &mut broad_read_bind,
                exporter,
                std::slice::from_ref(&read_only_bind),
                &private_dirs,
                std::slice::from_ref(&writable_bind),
            )
            .is_err(),
            "an injected broad host read bind must fail closed"
        );

        let mut swapped_exporter_fd = baseline();
        let exporter_bind = swapped_exporter_fd
            .windows(3)
            .position(|window| {
                window[0] == "--ro-bind-fd"
                    && window[1] == "8"
                    && window[2] == OPENCODE_SANDBOX_EXPORTER
            })
            .expect("baseline sealed exporter bind");
        swapped_exporter_fd[exporter_bind + 1] = "9".to_string();
        assert!(
            remove_opencode_bwrap_new_session(
                &mut swapped_exporter_fd,
                exporter,
                std::slice::from_ref(&read_only_bind),
                &private_dirs,
                std::slice::from_ref(&writable_bind),
            )
            .is_err(),
            "a substituted exporter descriptor must fail closed"
        );

        let mut widened_exporter_target = baseline();
        let exporter_bind = widened_exporter_target
            .windows(3)
            .position(|window| {
                window[0] == "--ro-bind-fd"
                    && window[1] == "8"
                    && window[2] == OPENCODE_SANDBOX_EXPORTER
            })
            .expect("baseline sealed exporter bind");
        widened_exporter_target[exporter_bind + 2] = "/usr/bin/opencode".to_string();
        assert!(
            remove_opencode_bwrap_new_session(
                &mut widened_exporter_target,
                exporter,
                std::slice::from_ref(&read_only_bind),
                &private_dirs,
                std::slice::from_ref(&writable_bind),
            )
            .is_err(),
            "an exporter descriptor mapped outside the private target must fail closed"
        );

        let mut swapped_store_fd = baseline();
        let store_bind = swapped_store_fd
            .windows(3)
            .position(|window| {
                window[0] == "--bind-fd"
                    && window[1] == "7"
                    && window[2] == writable_destination.to_string_lossy().as_ref()
            })
            .expect("baseline writable store bind");
        swapped_store_fd[store_bind + 1] = "9".to_string();
        assert!(
            remove_opencode_bwrap_new_session(
                &mut swapped_store_fd,
                exporter,
                std::slice::from_ref(&read_only_bind),
                &private_dirs,
                std::slice::from_ref(&writable_bind),
            )
            .is_err(),
            "a substituted writable store descriptor must fail closed"
        );

        let mut retained_path_string_fd = baseline();
        let exporter_bind = retained_path_string_fd
            .windows(3)
            .position(|window| {
                window[0] == "--ro-bind-fd"
                    && window[1] == "8"
                    && window[2] == OPENCODE_SANDBOX_EXPORTER
            })
            .expect("baseline sealed exporter FD bind");
        retained_path_string_fd[exporter_bind] = "--ro-bind".to_string();
        retained_path_string_fd[exporter_bind + 1] = "/proc/self/fd/8".to_string();
        assert!(
            remove_opencode_bwrap_new_session(
                &mut retained_path_string_fd,
                exporter,
                std::slice::from_ref(&read_only_bind),
                &private_dirs,
                std::slice::from_ref(&writable_bind),
            )
            .is_err(),
            "a path-string exporter FD bind could leak the setup capability and must fail closed"
        );

        let mut generated_path_bind = baseline();
        let store_bind = generated_path_bind
            .windows(3)
            .position(|window| {
                window[0] == "--bind-fd"
                    && window[1] == "7"
                    && window[2] == writable_destination.to_string_lossy().as_ref()
            })
            .expect("baseline writable store FD bind");
        generated_path_bind[store_bind] = "--bind".to_string();
        generated_path_bind[store_bind + 1] = "/proc/self/fd/7".to_string();
        replace_opencode_path_fd_binds(
            &mut generated_path_bind,
            std::slice::from_ref(&writable_bind),
        )
        .expect("the exact shared-builder store bind must convert to bind-fd");
        remove_opencode_bwrap_new_session(
            &mut generated_path_bind,
            exporter,
            std::slice::from_ref(&read_only_bind),
            &private_dirs,
            std::slice::from_ref(&writable_bind),
        )
        .expect("the converted descriptor-native store bind must be accepted");
    }

    /// The fd that bwrap receives must be a sealed copy of the exact bytes
    /// authenticated by the trust record. Replacing the path between
    /// revalidation and descriptor pin is rejected instead of executing the
    /// replacement from a trusted directory.
    #[cfg(target_os = "linux")]
    #[test]
    fn sealed_exporter_rejects_replaced_provenance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = fake_exporter(dir.path(), "printf original");
        let provenance = crate::internal::ai::observed_agents::trust::compute_provenance(&binary)
            .expect("compute original provenance");
        let replacement = dir.path().join("replacement-opencode");
        std::fs::write(&replacement, "#!/bin/sh\nprintf replacement\n").expect("write replacement");
        std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o755))
            .expect("make replacement executable");
        std::fs::rename(&replacement, &binary).expect("atomically replace exporter path");

        let error = pin_exporter_fd(&binary, Some(&provenance))
            .expect_err("replacement must fail the fd provenance check");
        assert!(
            error.to_string().contains("provenance changed"),
            "unexpected replacement failure: {error:#}"
        );
    }

    /// Capability descriptors must not collide with command stdio and remain
    /// close-on-exec in the parent; only the runner's fork-child allowlist can
    /// make the exact exporter/store fd inheritable for bwrap mount setup.
    #[cfg(target_os = "linux")]
    #[test]
    fn sealed_exporter_capability_is_read_only_non_stdio_and_cloexec() {
        use std::os::fd::AsRawFd;

        let dir = tempfile::tempdir().expect("tempdir");
        let binary = fake_exporter(dir.path(), "printf sealed");
        let fd = pin_exporter_fd(&binary, None).expect("seal test exporter");
        assert!(fd.as_raw_fd() >= 3, "capability fd must not overlap stdio");
        let fd_flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
        assert!(fd_flags >= 0, "F_GETFD on sealed exporter");
        assert_ne!(
            fd_flags & libc::FD_CLOEXEC,
            0,
            "sealed exporter must stay CLOEXEC until the fork-child allowlist"
        );
        let status_flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        assert!(status_flags >= 0, "F_GETFL on sealed exporter");
        assert_eq!(
            status_flags & libc::O_ACCMODE,
            libc::O_RDONLY,
            "bwrap must receive no writable sealed-exporter alias"
        );
    }

    /// R85: the sealed copy must accept a realistic single-file exporter.
    /// The stock OpenCode CLI is a ~171 MiB Bun binary; a 200 MiB exporter is
    /// trusted, sealed in full and still matched against its provenance, so a
    /// size cap below real binaries fails this test.
    #[cfg(target_os = "linux")]
    #[test]
    fn pin_exporter_seals_large_single_file_exporter_matching_provenance() {
        const LARGE_EXPORTER_BYTES: u64 = 200 * 1024 * 1024;
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = fake_exporter(dir.path(), "exit 0");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&binary)
            .and_then(|file| file.set_len(LARGE_EXPORTER_BYTES))
            .expect("extend exporter sparsely past the real OpenCode size");
        let provenance = crate::internal::ai::observed_agents::trust::compute_provenance(&binary)
            .expect("large exporter must be trustable");

        let fd = pin_exporter_fd(&binary, Some(&provenance))
            .expect("large trusted exporter must be sealed for bwrap");
        let sealed_len = std::fs::File::from(fd)
            .metadata()
            .expect("stat sealed exporter")
            .len();
        assert_eq!(
            sealed_len, LARGE_EXPORTER_BYTES,
            "the whole exporter is sealed"
        );
    }

    /// The public capture deadline begins before the sealed-copy step. A slow
    /// trusted/FUSE file is moved to the blocking pool and timing out there
    /// must leave no capability to hand to bwrap or execute as an exporter.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn sealed_exporter_deadline_prevents_bwrap_or_exporter_continuation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exporter_marker = dir.path().join("exporter-ran");
        let marker = shell_quote(&exporter_marker);
        let binary = fake_exporter(dir.path(), &format!("printf ran > {marker}"));
        let seal_path = binary.clone();
        let sealing_finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sealing_finished_in_worker = std::sync::Arc::clone(&sealing_finished);
        let result = seal_exporter_fd_until(
            tokio::time::Instant::now() + Duration::from_millis(20),
            move || {
                // Model a slow trusted/FUSE read. The delay happens on the
                // blocking pool, so the timeout still returns promptly.
                std::thread::sleep(Duration::from_millis(150));
                let result = pin_exporter_fd(&seal_path, None);
                sealing_finished_in_worker.store(true, std::sync::atomic::Ordering::Release);
                result
            },
        )
        .await;
        let error = result.expect_err("slow sealing must exhaust the capture deadline");
        assert!(
            error.to_string().contains("capture deadline"),
            "unexpected deadline error: {error:#}"
        );
        assert!(
            !sealing_finished.load(std::sync::atomic::Ordering::Acquire),
            "deadline must return before the slow sealing worker completes"
        );
        assert!(
            !exporter_marker.exists(),
            "expired sealing must not reach bwrap/exporter continuation"
        );
        // Keep the fixture directory alive until the detached worker has
        // returned and dropped its result, proving cleanup does not need an
        // accepted capability or a spawned child.
        tokio::time::sleep(Duration::from_millis(180)).await;
        assert!(
            sealing_finished.load(std::sync::atomic::Ordering::Acquire),
            "test worker must finish and release its unaccepted sealed descriptor"
        );
        assert!(
            !exporter_marker.exists(),
            "detached sealing work must never execute the exporter"
        );
    }

    /// SBX-03: user-writable trusted_bwrap_exe → Required fail-closed; no
    /// export bytes (hence no claim) are produced.
    #[cfg(target_os = "linux")]
    #[test]
    #[serial_test::serial(env)]
    fn trusted_bwrap_rejects_user_writable() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_exporter(dir.path(), r#"printf 'should-not-run'"#);
        let fake = dir.path().join("bwrap");
        std::fs::write(&fake, b"#!/bin/sh\nexec \"$@\"\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let exporter_fd = pin_exporter_fd(&bin, None).expect("pin fake exporter for test");
        let err = assemble_sandboxed_export(exporter_fd, Some(&fake))
            .err()
            .expect("user-writable bwrap must fail closed");
        let text = format!("{err:#}");
        assert!(
            text.contains("writable")
                || text.contains("fail-closed")
                || text.contains("refusing")
                || text.contains("Required"),
            "unexpected error: {text}"
        );
    }

    /// SBX-03: missing bwrap backend → transform/assembly fails; the export
    /// capability degrades (no authorized bytes to claim).
    #[cfg(target_os = "linux")]
    #[test]
    #[serial_test::serial(env)]
    fn backend_missing_degrades_metadata_only() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_exporter(dir.path(), r#"printf 'should-not-run'"#);
        let missing = dir.path().join("no-such-bwrap");
        let exporter_fd = pin_exporter_fd(&bin, None).expect("pin fake exporter for test");
        let err = assemble_sandboxed_export(exporter_fd, Some(&missing))
            .err()
            .expect("missing bwrap must fail closed");
        let text = format!("{err:#}");
        assert!(
            text.contains("Required")
                || text.contains("cannot be resolved")
                || text.contains("refusing")
                || text.contains("sandbox"),
            "unexpected error: {text}"
        );
    }

    /// SBX-03: trusted_bwrap_exe is the only bwrap channel — transform must
    /// not consume `LIBRA_BWRAP_BINARY` or `linux_sandbox_exe`.
    #[cfg(target_os = "linux")]
    #[serial_test::serial(export_sandbox_env, env)]
    #[test]
    fn trusted_bwrap_exe_channel_used() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_exporter(dir.path(), r#"printf 'ok'"#);
        let sentinel = dir.path().join("sentinel-bwrap");
        std::fs::write(&sentinel, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _bwrap_bin = EnvVarGuard::set(
            "LIBRA_BWRAP_BINARY",
            sentinel.to_str().expect("utf8 sentinel"),
        );
        let _linux_exe = EnvVarGuard::set(
            "LIBRA_LINUX_SANDBOX_EXE",
            sentinel.to_str().expect("utf8 sentinel"),
        );
        let trusted = system_true_binary();
        let exporter_fd = pin_exporter_fd(&bin, None).expect("pin fake exporter for test");
        let assembled = assemble_sandboxed_export(exporter_fd, Some(&trusted))
            .expect("injected trusted_bwrap_exe");
        let program = assembled
            .program
            .canonicalize()
            .unwrap_or(assembled.program.clone());
        let expected = trusted.canonicalize().unwrap_or(trusted);
        assert_eq!(
            program, expected,
            "transform must exec trusted_bwrap_exe, not LIBRA_BWRAP_BINARY / linux_sandbox_exe"
        );
        let joined = assembled.pre_args.join("\n");
        assert!(
            !joined.contains(sentinel.to_string_lossy().as_ref()),
            "LIBRA_BWRAP_BINARY/linux_sandbox_exe sentinel must not appear in argv: {joined}"
        );
        assert!(
            !assembled.pre_args.iter().any(|a| a == "--sandbox-policy"),
            "linux_sandbox_exe helper protocol must not be used: {:?}",
            assembled.pre_args
        );
    }

    #[cfg(target_os = "linux")]
    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    #[cfg(target_os = "linux")]
    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: callers are serialized with `#[serial_test::serial(export_sandbox_env)]`.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: callers are serialized with `#[serial_test::serial(export_sandbox_env)]`.
            unsafe {
                if let Some(previous) = &self.previous {
                    std::env::set_var(self.key, previous);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }
}
