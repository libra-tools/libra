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
//! - **Environment**: `env_clear()` plus a minimal allowlist (`HOME`,
//!   `XDG_DATA_HOME`, `XDG_CONFIG_HOME`) so the exporter can find its own
//!   session store but never a credential.
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
//!   deadline (default 3 s — expiry kills the child's process group). stderr
//!   is capped and redacted before it can appear in any error text
//!   (GC-DR-13). A child that leaves descendants in its process group after
//!   exit is killed without its output being accepted.
//!
//! Sandbox: the Required offline profile lives in
//! [`run_export_subprocess_sandboxed`] — assembled via
//! `SandboxManager::transform`. Linux: network unshared, host paths and
//! HOME read-only, tmpfs `/tmp`, with ONE probe-verified exception: the
//! opencode data dir is bound read-write because its WAL-mode SQLite store
//! needs write access even for reads. macOS: seatbelt (`sandbox-exec`)
//! denies host writes outside the store and network; reads are not confined.
//! Fail-closed without a usable sandbox backend.

use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use tokio::io::AsyncReadExt;

use crate::internal::ai::observed_agents::{
    Redactor, TranscriptSource,
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
/// stderr retention cap — enough to diagnose, small enough to redact cheaply.
const EXPORT_MAX_STDERR_BYTES: usize = 4 * 1024;
/// File-backed stdout must still be bounded while the child is running. A
/// short interval prevents a runaway trusted exporter from consuming disk for
/// the full subprocess deadline before the post-exit size check can run.
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
    let record = read_trust(OPENCODE_TRUST_SLUG)
        .await
        .context("read opencode trust record")?;
    trusted_opencode_binary_from(record).await
}

/// Injectable core of [`trusted_opencode_binary`] (GC-DR-07): the record
/// lookup is separated so the fail-closed no-record arm is unit-testable
/// without touching the process-wide config store (which may legitimately
/// trust opencode on a dev machine).
async fn trusted_opencode_binary_from(
    record: Option<crate::internal::ai::observed_agents::TrustRecord>,
) -> Result<PathBuf> {
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
    Ok(provenance.canonical_path)
}

fn valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 64
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Redact + truncate captured stderr for diagnostics (GC-DR-13: subprocess
/// stderr must be capped and redacted before display).
fn sanitized_stderr(raw: &[u8]) -> String {
    let capped = &raw[..raw.len().min(EXPORT_MAX_STDERR_BYTES)];
    let (redacted, _) = Redactor::new_default().redact(capped);
    String::from_utf8_lossy(redacted.as_ref()).into_owned()
}

/// Run the sandboxed export AND mint the digest-bound authorization in one
/// step — the ONLY constructor of an export-authorized byte source (ADR-DR-02
/// Bytes trust boundary). Callers receive an opaque [`TranscriptSource`] and
/// must still re-verify via `ExportAuthorized::matches` before use.
pub async fn authorized_sandboxed_export(
    binary: &std::path::Path,
    provider_session_id: &str,
    libra_session_id: &str,
    limits: ExportLimits,
) -> Result<TranscriptSource> {
    let bytes = run_export_subprocess_sandboxed(binary, provider_session_id, limits).await?;
    let auth = ExportAuthorized::issue("opencode", libra_session_id, &bytes);
    Ok(TranscriptSource::Bytes { bytes, auth })
}

/// Kill the process group created for an exporter. The direct child is the
/// group leader, so its pid is also the pgid. This prevents shell-based or
/// multi-process exporters from leaving descendants behind after a byte-cap
/// or deadline failure.
fn kill_export_process_group(pgid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pgid) = pgid.filter(|pid| *pid > 1) {
        // SAFETY: the command is placed in a fresh process group immediately
        // before spawn. A negative pid targets only that group; failure means
        // it has already exited and is benign.
        unsafe {
            libc::kill(-(pgid as libc::pid_t), libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pgid;
}

/// Whether an exporter descendant remains in the process group after the
/// direct child has exited. Accepting the output while this is true would let
/// that descendant keep mutating the inherited stdout file after validation.
fn export_process_group_alive(pgid: Option<u32>) -> bool {
    #[cfg(unix)]
    if let Some(pgid) = pgid.filter(|pid| *pid > 1) {
        // SAFETY: signal 0 performs an existence/permission check without
        // delivering a signal. EPERM still proves the group exists.
        let result = unsafe { libc::kill(-(pgid as libc::pid_t), 0) };
        return result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    }
    #[cfg(not(unix))]
    let _ = pgid;
    false
}

/// Run `<binary> export <session_id>` under the module's bounds and return
/// the raw export bytes. The caller (DR-04b wiring) tags them via
/// `ExportAuthorized::issue` and feeds the seam — this function itself never
/// persists anything.
pub async fn run_export_subprocess(
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

    run_bounded_exporter(binary, &[], session_id, limits, Vec::new()).await
}

/// Fds the caller pinned that must stay open (and inheritable) until the
/// child has been spawned. File descriptors only exist on Unix; elsewhere the
/// alias is an uninhabited placeholder so the runner signature stays portable.
#[cfg(unix)]
type PinnedFds = Vec<std::os::fd::OwnedFd>;
#[cfg(not(unix))]
type PinnedFds = Vec<std::convert::Infallible>;

/// Core bounded runner: `<program> [<pre_args>…] export <session_id>` with
/// the module's env/caps/deadline contract. `pre_args` lets the sandboxed
/// variant prepend the bwrap arg vector while keeping ONE code path for the
/// bounds (GC-DR-04).
async fn run_bounded_exporter(
    program: &std::path::Path,
    pre_args: &[String],
    session_id: &str,
    limits: ExportLimits,
    keep_fds: PinnedFds,
) -> Result<Vec<u8>> {
    // Fds pinned by the caller (e.g. the RW store bind's /proc/self/fd source)
    // must stay OPEN and non-CLOEXEC in this process until the child has been
    // spawned so it inherits them; holding the OwnedFds for the whole function
    // guarantees that and closes them on return.
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
                let lim = libc::rlimit {
                    rlim_cur: fsize_limit,
                    rlim_max: fsize_limit,
                };
                if libc::setrlimit(libc::RLIMIT_FSIZE, &lim) != 0 {
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
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(stdout_for_child))
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    // Minimal env: the exporter must locate its own session store, nothing
    // else. Credentials/endpoints never pass (env_clear + explicit list).
    for name in ["HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }

    let mut child = command.spawn().context("spawn opencode export")?;
    let process_group = child.id();
    let mut stderr = child.stderr.take().expect("stderr piped"); // INVARIANT: piped above

    let mut stderr_reader = tokio::spawn(async move {
        let mut err_buf = Vec::new();
        let _ = (&mut stderr)
            .take(EXPORT_MAX_STDERR_BYTES as u64)
            .read_to_end(&mut err_buf)
            .await;
        err_buf
    });

    enum WaitOutcome {
        Exited(std::io::Result<std::process::ExitStatus>),
        Deadline,
        OverCap(u64),
        SizeReadFailed(std::io::Error),
    }

    let deadline = tokio::time::sleep(limits.deadline);
    tokio::pin!(deadline);
    let mut size_poll = tokio::time::interval(EXPORT_SIZE_POLL_INTERVAL);
    size_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let outcome = loop {
        tokio::select! {
            status = child.wait() => break WaitOutcome::Exited(status),
            _ = &mut deadline => break WaitOutcome::Deadline,
            _ = size_poll.tick() => {
                match stdout_file.metadata() {
                    Ok(metadata) if metadata.len() > limits.max_bytes => {
                        break WaitOutcome::OverCap(metadata.len());
                    }
                    Ok(_) => {}
                    Err(err) => break WaitOutcome::SizeReadFailed(err),
                }
            }
        }
    };

    let (err_buf, status) = match outcome {
        WaitOutcome::Exited(status) => {
            if export_process_group_alive(process_group) {
                kill_export_process_group(process_group);
                stderr_reader.abort();
                let _ = stderr_reader.await;
                bail!(
                    "opencode export left descendant processes running after exit; \
                     killed without accepting mutable output"
                );
            }
            let err_buf = tokio::select! {
                result = &mut stderr_reader => {
                    result.context("join opencode export stderr reader")?
                }
                _ = &mut deadline => {
                    kill_export_process_group(process_group);
                    stderr_reader.abort();
                    let _ = stderr_reader.await;
                    bail!(
                        "opencode export exceeded its {:?} deadline while finishing stderr; \
                         killed without accepting content",
                        limits.deadline
                    );
                }
            };
            (err_buf, status.context("wait for opencode export")?)
        }
        WaitOutcome::Deadline => {
            // Deadline: kill and fail closed — a slow exporter must not eat
            // the hook budget (GC-DR-04).
            kill_export_process_group(process_group);
            let _ = child.kill().await;
            stderr_reader.abort();
            let _ = child.wait().await;
            let _ = stderr_reader.await;
            bail!(
                "opencode export exceeded its {:?} deadline; killed (content \
                 skipped this idle — a later idle retries)",
                limits.deadline
            );
        }
        WaitOutcome::OverCap(observed) => {
            kill_export_process_group(process_group);
            let _ = child.kill().await;
            stderr_reader.abort();
            let _ = child.wait().await;
            let _ = stderr_reader.await;
            bail!(
                "opencode export exceeded the {} byte cap while running \
                 (observed {observed} bytes); killed without returning content",
                limits.max_bytes
            );
        }
        WaitOutcome::SizeReadFailed(err) => {
            kill_export_process_group(process_group);
            let _ = child.kill().await;
            stderr_reader.abort();
            let _ = child.wait().await;
            let _ = stderr_reader.await;
            return Err(err).context("monitor opencode export output size");
        }
    };

    // Byte cap on the flushed file (GC-DR-04): over-cap errors, never a
    // silent truncation.
    let mut stdout_file = stdout_file;
    use std::io::{Read as _, Seek as _, SeekFrom};
    stdout_file
        .seek(SeekFrom::Start(0))
        .context("rewind export output")?;
    // Bounded read + recheck on the bytes ACTUALLY read (Codex M3 R2 P1-1):
    // never trust a pre-measured size and never read unbounded into memory. A
    // `setsid()`-escaped exporter descendant is invisible to the process-group
    // liveness probe and could append between a size measurement and the read;
    // in the non-sandboxed path there is no PID namespace to reap it (the
    // sandboxed path's `--unshare-all` already does). Reading at most cap+1
    // bytes and rejecting any overflow closes that window regardless: content
    // over the cap is refused, never accepted or truncated silently.
    let mut out = Vec::new();
    let read = (&mut stdout_file)
        .take(limits.max_bytes.saturating_add(1))
        .read_to_end(&mut out)
        .context("read export output file")? as u64;
    if read > limits.max_bytes {
        bail!(
            "opencode export exceeded the {} byte cap; refusing content",
            limits.max_bytes
        );
    }
    if !status.success() {
        bail!(
            "opencode export failed (status {status}); stderr (redacted, capped): {}",
            sanitized_stderr(&err_buf)
        );
    }
    Ok(out)
}

/// Run the export under the DR-04b minimal offline sandbox profile
/// (`SandboxEnforcement::Required` semantics). Assembly is delegated to
/// [`crate::internal::ai::sandbox::SandboxManager::transform`]; execution
/// stays in [`run_bounded_exporter`] (file-backed stdout, `RLIMIT_FSIZE`,
/// process group, wall clock, 16 MiB). Linux uses trusted bwrap; macOS uses
/// seatbelt (`sandbox-exec`). Fail-CLOSED when the sandbox cannot be
/// provided: the capability is unavailable — never a degraded unsandboxed
/// run (GC-DR-14).
pub async fn run_export_subprocess_sandboxed(
    binary: &std::path::Path,
    session_id: &str,
    limits: ExportLimits,
) -> Result<Vec<u8>> {
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (binary, session_id, limits);
        bail!(
            "the OpenCode export sandbox profile requires Linux bubblewrap or \
             macOS seatbelt; refusing an unsandboxed export (fail-closed, GC-DR-14)"
        );
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if !valid_session_id(session_id) {
            bail!("invalid OpenCode session id (expected alnum/dash/underscore, ≤64 chars)");
        }
        if !binary.is_absolute() {
            bail!("exporter binary path must be absolute (trusted provenance)");
        }
        #[cfg(target_os = "linux")]
        let trusted_bwrap = Some(resolve_trusted_bwrap()?);
        #[cfg(target_os = "macos")]
        let trusted_bwrap: Option<PathBuf> = {
            if !sandbox_exec_available() {
                bail!(
                    "macOS seatbelt (sandbox-exec) is required for the OpenCode \
                     export sandbox and was not found; refusing an unsandboxed \
                     export (fail-closed, GC-DR-14)"
                );
            }
            None
        };
        let assembled = assemble_sandboxed_export(binary, trusted_bwrap.as_deref())?;
        run_bounded_exporter(
            &assembled.program,
            &assembled.pre_args,
            session_id,
            limits,
            assembled.keep_fds,
        )
        .await
    }
}

/// Assembled Required-sandbox argv plus caller-held store fds.
/// `program` is the sandbox backend (trusted bwrap or `sandbox-exec`);
/// `pre_args` is everything transform placed after it (including `--` and
/// the exporter binary). `export <sid>` is appended by [`run_bounded_exporter`].
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct AssembledExport {
    program: PathBuf,
    pre_args: Vec<String>,
    keep_fds: PinnedFds,
}

/// Assemble the export sandbox vector through `SandboxManager::transform`.
///
/// On Linux, `trusted_bwrap` is the integrity-checked product of
/// [`resolve_trusted_bwrap`] and is consumed via `trusted_bwrap_exe` (no
/// `LIBRA_BWRAP_BINARY` / `linux_sandbox_exe` rediscovery). On macOS it is
/// `None` and `select_initial` chooses seatbelt. Retained store fds stay
/// with the caller (`keep_fds`); they never enter `ExecEnv`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn assemble_sandboxed_export(
    binary: &std::path::Path,
    trusted_bwrap: Option<&std::path::Path>,
) -> Result<AssembledExport> {
    use crate::internal::ai::sandbox::{
        CommandSpec, SandboxEnforcement, SandboxManager, SandboxPermissions, SandboxPolicy,
        SandboxTransformRequest, WritableBind,
    };

    // ReadOnly + Denied network. cwd must not be /tmp (Linux tmpfs shadow).
    // Use the binary's parent, which the bwrap builder already ro-binds as
    // the empty writable-roots fallback.
    let sandbox_cwd = binary
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("/usr"));

    let mut extra_ro = Vec::new();
    for var in ["HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME"] {
        if let Some(dir) = std::env::var_os(var).map(std::path::PathBuf::from)
            && dir.is_absolute()
            && dir.is_dir()
        {
            extra_ro.push(dir);
        }
    }

    let mut keep_fds: PinnedFds = Vec::new();
    let mut writable_binds = Vec::new();
    // WAL-mode SQLite needs WRITE even for reads. Linux binds
    // `/proc/self/fd/N → dest`; macOS seatbelt allows `file-write*` on the
    // F_GETPATH destination (path-level, ADR-SBX-03).
    match pin_opencode_store() {
        Ok(Some((fd, dest))) => {
            let dest_path = PathBuf::from(dest);
            let source = {
                #[cfg(target_os = "linux")]
                {
                    use std::os::fd::AsRawFd;
                    PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()))
                }
                #[cfg(target_os = "macos")]
                {
                    dest_path.clone()
                }
            };
            writable_binds.push(WritableBind {
                source,
                destination: dest_path,
            });
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

    // FIX-SBX-01: real `opencode export` mkdirs `/tmp/opencode` and WAL-
    // checkpoints sqlite there. Bind only that subdirectory (never host
    // `/tmp` / `/private/tmp`). Linux already gets an isolated tmpfs `/tmp`.
    #[cfg(target_os = "macos")]
    writable_binds.push(macos_opencode_scratch_bind()?);

    let spec = CommandSpec {
        program: binary.to_string_lossy().into_owned(),
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
            extra_ro_bind_paths: &extra_ro,
            writable_binds: &writable_binds,
            trusted_bwrap_exe: trusted_bwrap,
            seccomp_policy_path: None,
        })
        .context(
            "failed to assemble the OpenCode export sandbox \
             (SandboxEnforcement::Required); refusing an unsandboxed export",
        )?;

    let mut command = env.command;
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

/// Narrow Darwin scratch for OpenCode: `/tmp/opencode` only (canonical
/// `/private/tmp/opencode`). The last component is pinned with
/// `O_NOFOLLOW|O_DIRECTORY` so a hostile `/tmp/opencode` symlink cannot
/// redirect the writable-bind (FIX-SBX-01 R1 P0).
#[cfg(target_os = "macos")]
fn macos_opencode_scratch_bind() -> Result<crate::internal::ai::sandbox::WritableBind> {
    scratch_bind_under(
        std::path::Path::new("/tmp"),
        std::path::Path::new("/private/tmp/opencode"),
    )
}

/// mkdir 0700 `base/opencode` (an existing dir is tolerated), pin the final
/// component with `O_NOFOLLOW|O_DIRECTORY`, then enforce on the PINNED fd
/// (no path re-lookup): the dir must be owned by our euid — a foreign-owned
/// `/tmp/opencode` is refused outright, since its owner could rename it
/// under sticky `/tmp` after the check and redirect the bind — and any
/// group/other mode bits are tightened to 0700 via `fchmod` on the fd
/// (FIX-SBX-01 R2: the pre-existing dir on a dev host is 0755).
/// `F_GETPATH` must resolve exactly to `expected`.
#[cfg(target_os = "macos")]
fn scratch_bind_under(
    base: &std::path::Path,
    expected: &std::path::Path,
) -> Result<crate::internal::ai::sandbox::WritableBind> {
    use std::os::unix::fs::DirBuilderExt;

    use crate::internal::ai::sandbox::WritableBind;

    let scratch = base.join("opencode");
    match std::fs::DirBuilder::new().mode(0o700).create(&scratch) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(err) => {
            return Err(err).context(
                "cannot create /tmp/opencode for the OpenCode seatbelt scratch bind \
                 (fail-closed)",
            );
        }
    }
    let fd = pin_store_under(base).context(
        "pin /tmp/opencode (O_NOFOLLOW directory); a symlink here is refused \
         (fail-closed)",
    )?;
    enforce_scratch_owner_and_mode(&fd)?;
    let pinned = resolve_pinned_store_path(&fd)?;
    drop(fd);
    if pinned != expected {
        bail!(
            "OpenCode scratch pin resolved to {}, expected {}; refusing \
             (fail-closed)",
            pinned.display(),
            expected.display()
        );
    }
    Ok(WritableBind {
        source: pinned.clone(),
        destination: pinned,
    })
}

/// `fstat` the pinned scratch fd and enforce ownership, then tighten wide
/// modes in place. Operating on the fd keeps the check free of path
/// re-lookup races.
#[cfg(target_os = "macos")]
fn enforce_scratch_owner_and_mode(fd: &std::os::fd::OwnedFd) -> Result<()> {
    use std::os::fd::AsRawFd;

    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: fstat fills `st` for an fd we own; zeroed stat is a valid
    // out-param.
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
        return Err(std::io::Error::last_os_error())
            .context("fstat the OpenCode scratch dir (fail-closed)");
    }
    // SAFETY: geteuid has no failure mode.
    let euid = unsafe { libc::geteuid() };
    validate_scratch_owner(st.st_uid, euid)?;
    if u32::from(st.st_mode) & 0o077 != 0 {
        // We own it (checked above), so tightening through the pinned fd is
        // authoritative and race-free.
        // SAFETY: fchmod on an fd we own.
        if unsafe { libc::fchmod(fd.as_raw_fd(), 0o700) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("fchmod 0700 the OpenCode scratch dir (fail-closed)");
        }
    }
    Ok(())
}

/// Pure ownership decision for the scratch dir: a foreign owner is refused
/// (they could rename the dir under sticky `/tmp` and redirect the
/// writable-bind, and its contents would be exposed to them).
#[cfg(target_os = "macos")]
fn validate_scratch_owner(st_uid: libc::uid_t, euid: libc::uid_t) -> Result<()> {
    if st_uid != euid {
        bail!(
            "/tmp/opencode is owned by uid {st_uid}, not our euid {euid}; \
             refusing the seatbelt scratch bind (fail-closed)"
        );
    }
    Ok(())
}

/// Whether `/usr/bin/sandbox-exec` is present. Tests may force a missing
/// backend via [`macos_test_hooks`].
#[cfg(target_os = "macos")]
fn sandbox_exec_available() -> bool {
    #[cfg(test)]
    if let Some(forced) = macos_test_hooks::SANDBOX_EXEC_AVAILABLE.with(|c| c.get()) {
        return forced;
    }
    std::path::Path::new("/usr/bin/sandbox-exec").is_file()
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
fn resolve_trusted_bwrap() -> Result<std::path::PathBuf> {
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
/// the bwrap binary passes the integrity policy AND can actually create its
/// namespaces (a bounded `--unshare-all … /bin/true` no-op probe). Tests gate
/// on this so they detect "trusted AND usable", not merely "bwrap present" —
/// on a host with unprivileged user namespaces disabled the probe fails and
/// the tests skip instead of running and failing (Codex M3 R4 P2).
#[cfg(target_os = "linux")]
pub fn trusted_bwrap_available() -> bool {
    let Ok(bwrap) = resolve_trusted_bwrap() else {
        return false;
    };
    let Ok(mut child) = std::process::Command::new(&bwrap)
        .args([
            "--unshare-all",
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--",
            "/bin/true",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };
    // Actually bounded (Codex M3 R5 P2): the no-op probe returns in
    // milliseconds, but a trusted bwrap stalled in namespace/mount setup must
    // not hang the caller — poll with a short deadline, then kill and reap.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// Non-Linux hosts have no bwrap sandbox — the export capability is
/// unavailable (fail-closed), so it is never "trusted and usable".
#[cfg(not(target_os = "linux"))]
pub fn trusted_bwrap_available() -> bool {
    false
}

/// Pin the OpenCode WAL store for a race-safe RW bind, returning the pinned fd
/// and the sandbox destination path (where the exporter expects its store).
/// Reads the data root from `XDG_DATA_HOME` (absolute) or `HOME/.local/share`.
///
/// Missing/unpinnable store → `Ok(None)` (skip the RW exception). A successful
/// pin whose destination cannot be resolved (macOS `F_GETPATH` failure) is
/// `Err` (fail-closed).
#[cfg(unix)]
fn pin_opencode_store() -> Result<Option<(std::os::fd::OwnedFd, String)>> {
    pin_opencode_store_with(resolve_store_destination)
}

#[cfg(unix)]
fn pin_opencode_store_with(
    resolve: impl FnOnce(&std::os::fd::OwnedFd, &std::path::Path) -> Result<String>,
) -> Result<Option<(std::os::fd::OwnedFd, String)>> {
    let Some(base) = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .map(|h| h.join(".local/share"))
        })
    else {
        return Ok(None);
    };
    match pin_store_under(&base) {
        Ok(fd) => {
            let dest = resolve(&fd, &base)?;
            Ok(Some((fd, dest)))
        }
        Err(err) => {
            let absent = err.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            });
            if absent {
                tracing::warn!(
                    error = %format!("{err:#}"),
                    base = %base.display(),
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

#[cfg(unix)]
fn resolve_store_destination(fd: &std::os::fd::OwnedFd, base: &std::path::Path) -> Result<String> {
    #[cfg(test)]
    if let Some(forced) = PIN_PATH_RESOLVER.with(|slot| *slot.borrow()) {
        return forced(fd, base);
    }
    #[cfg(target_os = "linux")]
    {
        let _ = fd;
        Ok(base.join("opencode").to_string_lossy().into_owned())
    }
    #[cfg(target_os = "macos")]
    {
        let _ = base;
        resolve_pinned_store_path(fd).map(|p| p.to_string_lossy().into_owned())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (fd, base);
        bail!(
            "OpenCode store pin destination resolution is only implemented on \
             Linux and macOS; refusing (fail-closed)"
        )
    }
}

/// macOS path-level pin: `fcntl(F_GETPATH)` then re-check the snapshot is an
/// existing directory. Failure is fail-closed (ADR-SBX-02/03).
#[cfg(target_os = "macos")]
fn resolve_pinned_store_path(fd: &std::os::fd::OwnedFd) -> Result<PathBuf> {
    use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};

    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes a NUL-terminated path of at most PATH_MAX
    // bytes into our buffer; the fd is owned by us.
    let rc = unsafe {
        libc::fcntl(
            fd.as_raw_fd(),
            libc::F_GETPATH,
            buf.as_mut_ptr() as *mut libc::c_char,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .context("F_GETPATH on pinned OpenCode store fd");
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len]));
    if !path.is_absolute() {
        bail!(
            "F_GETPATH returned a non-absolute path {}; refusing (fail-closed)",
            path.display()
        );
    }
    let meta = std::fs::metadata(&path).with_context(|| {
        format!(
            "pinned store path {} vanished after F_GETPATH; refusing (fail-closed)",
            path.display()
        )
    })?;
    if !meta.is_dir() {
        bail!(
            "pinned store path {} is not a directory; refusing (fail-closed)",
            path.display()
        );
    }
    Ok(path)
}

#[cfg(test)]
type PinPathResolver = fn(&std::os::fd::OwnedFd, &std::path::Path) -> Result<String>;

#[cfg(test)]
thread_local! {
    static PIN_PATH_RESOLVER: std::cell::RefCell<Option<PinPathResolver>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, target_os = "macos"))]
mod macos_test_hooks {
    use std::cell::Cell;

    thread_local! {
        pub static SANDBOX_EXEC_AVAILABLE: Cell<Option<bool>> = const { Cell::new(None) };
    }

    pub struct SandboxExecMissingGuard {
        previous: Option<bool>,
    }

    impl SandboxExecMissingGuard {
        pub fn install() -> Self {
            let previous = SANDBOX_EXEC_AVAILABLE.with(|c| c.replace(Some(false)));
            Self { previous }
        }
    }

    impl Drop for SandboxExecMissingGuard {
        fn drop(&mut self) {
            SANDBOX_EXEC_AVAILABLE.with(|c| c.set(self.previous));
        }
    }

    /// Injects a `F_GETPATH` resolver for `macos_pin_fgetpath_failure`.
    /// Lives here (not in `mod tests`) so Linux `cfg(test)` builds do not
    /// define an unused type under `clippy -D warnings`.
    pub struct PinPathResolverGuard {
        previous: Option<super::PinPathResolver>,
    }

    impl PinPathResolverGuard {
        pub fn install(resolver: super::PinPathResolver) -> Self {
            let previous = super::PIN_PATH_RESOLVER.with(|slot| slot.replace(Some(resolver)));
            Self { previous }
        }
    }

    impl Drop for PinPathResolverGuard {
        fn drop(&mut self) {
            super::PIN_PATH_RESOLVER.with(|slot| slot.replace(self.previous));
        }
    }
}

/// Resolution + pin as ONE atomic `openat`: open the data root, then `openat`
/// the literal `opencode` entry with `O_DIRECTORY|O_NOFOLLOW`. Because the
/// returned fd IS the validated directory, a concurrent rename/exchange of
/// `opencode` cannot make the bound directory differ from the checked one.
/// `O_NOFOLLOW` rejects a symlinked entry; `O_DIRECTORY` requires a directory.
///
/// Linux uses `O_PATH` and clears CLOEXEC so bwrap can bind `/proc/self/fd/N`.
/// macOS has no `O_PATH`; it opens `O_RDONLY` and keeps CLOEXEC — seatbelt
/// matches the `F_GETPATH` snapshot, not the fd (ADR-SBX-03).
#[cfg(unix)]
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
    #[cfg(target_os = "linux")]
    let base_flags = libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC;
    #[cfg(target_os = "macos")]
    let base_flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let base_flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
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
    #[cfg(target_os = "linux")]
    let child_flags = libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let child_flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: base_fd is a valid dir fd; name is a valid C string; the result
    // is wrapped for RAII.
    let raw = unsafe { libc::openat(base_fd.as_raw_fd(), name.as_ptr(), child_flags) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error())
            .context("pin opencode store (openat, no-follow directory)");
    }
    // SAFETY: fresh owned fd from openat(2).
    let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
    #[cfg(target_os = "linux")]
    {
        // Clear CLOEXEC so the bwrap child inherits it and can resolve
        // /proc/self/fd/N when it establishes the bind mount.
        // SAFETY: fcntl on our own fd; F_GETFD/F_SETFD have no memory effects.
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
        if flags < 0 {
            return Err(std::io::Error::last_os_error()).context("F_GETFD on pinned store fd");
        }
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error())
                .context("clear CLOEXEC on pinned store fd");
        }
    }
    Ok(fd)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;

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

    /// Whether an executable named `name` is resolvable on `PATH` (used to skip
    /// tests that depend on an optional system tool such as `setsid`).
    #[cfg(unix)]
    fn binary_on_path(name: &str) -> bool {
        std::env::var_os("PATH")
            .map(|path| {
                std::env::split_paths(&path).any(|dir| {
                    let candidate = dir.join(name);
                    candidate.is_file()
                        && std::fs::metadata(&candidate)
                            .map(|m| m.permissions().mode() & 0o111 != 0)
                            .unwrap_or(false)
                })
            })
            .unwrap_or(false)
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
    #[cfg(unix)]
    #[tokio::test]
    async fn opencode_export_argv_no_shell() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_exporter(dir.path(), r#"printf '%s|%s' "$1" "$2""#);
        let out = run_export_subprocess(&bin, "sess_1-2", ExportLimits::default())
            .await
            .expect("export runs");
        assert_eq!(String::from_utf8_lossy(&out), "export|sess_1-2");
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

    /// A successful direct child cannot leave a background writer holding the
    /// inherited output descriptors after the result has been validated.
    #[cfg(unix)]
    #[tokio::test]
    async fn opencode_export_rejects_surviving_descendant() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_exporter(dir.path(), "sleep 30 & printf 'apparently-done'");
        let started = std::time::Instant::now();
        let err = run_export_subprocess(
            &bin,
            "s1",
            ExportLimits {
                max_bytes: 1024,
                deadline: Duration::from_secs(5),
            },
        )
        .await
        .expect_err("surviving exporter descendants must be killed");
        assert!(
            format!("{err:#}").contains("descendant processes"),
            "got {err:#}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "descendant rejection must be prompt, waited {:?}",
            started.elapsed()
        );
    }

    /// Codex M3 R2 P1-1: a `setsid()`-escaped descendant leaves the child's
    /// process group (so the group-liveness probe cannot see it), yet the
    /// over-cap bytes it writes to the inherited stdout are still refused —
    /// the byte cap is enforced on the bytes, not on group membership. Skips
    /// when `setsid` is unavailable.
    #[cfg(unix)]
    #[tokio::test]
    async fn opencode_export_setsid_escapee_cannot_exceed_cap() {
        if !binary_on_path("setsid") {
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

    /// Codex M3 R3 P2: the integrity policy must ACCEPT a legitimately packaged
    /// bwrap (root-owned ancestry, not user-writable) so trusted deployments do
    /// not silently degrade. When the host has such a bwrap, `trusted_bwrap_
    /// available()` is true and `validate_trusted_bwrap` accepts it; otherwise
    /// the case is skipped rather than asserted.
    #[cfg(target_os = "linux")]
    #[test]
    fn validate_trusted_bwrap_accepts_system_binary() {
        let Some(bwrap) = which_bwrap() else {
            eprintln!("skipped (no bwrap on PATH)");
            return;
        };
        if validate_trusted_bwrap(&bwrap).is_ok() {
            assert!(
                trusted_bwrap_available(),
                "a validatable system bwrap must report available"
            );
        } else {
            eprintln!("skipped (system bwrap is under a user-writable path here)");
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

    /// Codex M3 R4 P1: the pinned-fd RW bind works through real bwrap — a file
    /// the child writes inside the `/proc/self/fd/N`-bound directory lands on
    /// the host at the pinned inode. Skips without a trusted, usable bwrap.
    #[cfg(target_os = "linux")]
    #[test]
    fn pin_store_binds_rw_through_bwrap() {
        use std::os::fd::AsRawFd;
        if !trusted_bwrap_available() {
            eprintln!("skipped (no trusted, usable bwrap)");
            return;
        }
        let bwrap = resolve_trusted_bwrap().expect("resolve trusted bwrap");
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("opencode")).unwrap();

        let fd = pin_store_under(tmp.path()).expect("pin real dir");
        let src = format!("/proc/self/fd/{}", fd.as_raw_fd());
        let status = std::process::Command::new(&bwrap)
            .args([
                "--unshare-all",
                "--die-with-parent",
                "--ro-bind",
                "/",
                "/",
                "--bind",
                &src,
                "/mnt",
                "--",
                "/bin/sh",
                "-c",
                "echo ok > /mnt/probe",
            ])
            .status()
            .expect("spawn bwrap");
        drop(fd);
        assert!(status.success(), "pinned RW bind must let the child write");
        assert!(
            tmp.path().join("opencode/probe").exists(),
            "child write must land on the host store via the pinned fd"
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

    /// A failing exporter surfaces capped, redacted stderr — and secrets in
    /// stderr never appear raw in the error text.
    #[cfg(unix)]
    #[tokio::test]
    async fn opencode_export_failure_redacts_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_exporter(
            dir.path(),
            "echo 'fatal: key AKIAAAAAAAAAAAAAAAAA rejected' >&2; exit 3",
        );
        let err = run_export_subprocess(&bin, "s1", ExportLimits::default())
            .await
            .expect_err("non-zero exit must fail");
        let text = format!("{err:#}");
        assert!(
            !text.contains("AKIAAAAAAAAAAAAAAAAA"),
            "raw secret leaked: {text}"
        );
        assert!(text.contains("status"), "got {text}");
    }

    /// opencode_export_offline_sandbox_profile: the bwrap Required profile
    /// actually runs an exporter offline — network is unshared (a connect
    /// attempt fails instantly), HOME is readable (store locator), /tmp is
    /// writable tmpfs, and stdout flows through the same bounds. Skips when
    /// bwrap is unavailable (the production path then fails closed).
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn opencode_export_offline_sandbox_profile() {
        // Detect "trusted AND usable", not merely present (Codex M3 R3): a
        // bwrap under a user-writable path is refused by the integrity policy,
        // so the sandbox would degrade — skip rather than assert success.
        if !trusted_bwrap_available() {
            eprintln!("skipped (no trusted, usable bwrap)");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // The fake exporter proves: HOME readable, /tmp writable, then emits.
        let bin = fake_exporter(
            dir.path(),
            r#"ls "$HOME" >/dev/null 2>&1 || { echo home-unreadable >&2; exit 4; }
touch /tmp/probe || { echo tmp-unwritable >&2; exit 5; }
printf '{"info":{},"messages":[]}'"#,
        );
        let out = run_export_subprocess_sandboxed(&bin, "sess-1", ExportLimits::default())
            .await
            .expect("sandboxed export must run offline");
        assert_eq!(
            String::from_utf8_lossy(&out),
            r#"{"info":{},"messages":[]}"#
        );

        // Network must be unshared: a resolver/socket attempt fails fast.
        let net_bin = fake_exporter(
            dir.path(),
            r#"if command -v getent >/dev/null 2>&1; then
  getent hosts example.com >/dev/null 2>&1 && { echo net-open >&2; exit 6; }
fi
printf 'offline-ok'"#,
        );
        let out = run_export_subprocess_sandboxed(&net_bin, "sess-2", ExportLimits::default())
            .await
            .expect("offline probe must succeed");
        assert_eq!(String::from_utf8_lossy(&out), "offline-ok");
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
    /// GiB backstop on macOS (FIX-SBX-01) — process group, 3s wall clock).
    /// Linux keep_fds store fd is non-CLOEXEC.
    #[tokio::test]
    async fn runner_controls_preserved() {
        assert_eq!(EXPORT_MAX_BYTES, 16 * 1024 * 1024);
        assert_eq!(EXPORT_DEADLINE, Duration::from_secs(3));

        let dir = tempfile::tempdir().unwrap();
        let bin = fake_exporter(dir.path(), r#"printf 'ok'"#);
        let out = run_bounded_exporter(&bin, &[], "sess1", ExportLimits::default(), Vec::new())
            .await
            .expect("run_bounded_exporter still executes");
        assert_eq!(out, b"ok");

        let over = fake_exporter(dir.path(), "head -c 5000 /dev/zero");
        let err = run_bounded_exporter(
            &over,
            &[],
            "s1",
            ExportLimits {
                max_bytes: 1024,
                deadline: Duration::from_secs(5),
            },
            Vec::new(),
        )
        .await
        .expect_err("byte cap must still fail closed");
        assert!(format!("{err:#}").contains("byte cap"), "got {err:#}");

        let hung = fake_exporter(dir.path(), "sleep 30");
        let started = std::time::Instant::now();
        let err = run_bounded_exporter(
            &hung,
            &[],
            "s1",
            ExportLimits {
                max_bytes: 1024,
                deadline: Duration::from_millis(300),
            },
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

        let descendant = fake_exporter(dir.path(), "sleep 30 & printf 'apparently-done'");
        let err = run_bounded_exporter(
            &descendant,
            &[],
            "s1",
            ExportLimits {
                max_bytes: 1024,
                deadline: Duration::from_secs(5),
            },
            Vec::new(),
        )
        .await
        .expect_err("process-group descendants must still be refused");
        assert!(
            format!("{err:#}").contains("descendant processes"),
            "got {err:#}"
        );

        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let tmp = tempfile::tempdir().unwrap();
            std::fs::create_dir(tmp.path().join("opencode")).unwrap();
            let fd = pin_store_under(tmp.path()).expect("pin store");
            let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
            assert!(flags >= 0, "F_GETFD on pinned store fd");
            assert_eq!(
                flags & libc::FD_CLOEXEC,
                0,
                "keep_fds store fd must not be CLOEXEC"
            );
            let ok = fake_exporter(dir.path(), r#"printf 'pinned'"#);
            let out = run_bounded_exporter(&ok, &[], "s1", ExportLimits::default(), vec![fd])
                .await
                .expect("runner must accept caller-held keep_fds");
            assert_eq!(out, b"pinned");
        }
    }

    /// SBX-04: pin succeeds on a real directory, rejects a non-directory, and
    /// rejects a symlink (O_NOFOLLOW). Production classification
    /// (`pin_opencode_store`) treats only an absent store as `Ok(None)`.
    #[cfg(target_os = "macos")]
    #[serial_test::serial(export_sandbox_env)]
    #[test]
    fn macos_pin_three_states() {
        let tmp = tempfile::tempdir().unwrap();
        let _xdg = EnvVarGuard::set("XDG_DATA_HOME", tmp.path().to_str().expect("utf8"));
        let store = tmp.path().join("opencode");
        std::fs::create_dir(&store).unwrap();
        pin_store_under(tmp.path()).expect("pin real opencode dir");
        assert!(
            pin_opencode_store()
                .expect("present store Result")
                .is_some(),
            "real store must pin"
        );

        std::fs::remove_dir_all(&store).unwrap();
        assert!(
            pin_opencode_store().expect("absent store Result").is_none(),
            "absent store must skip (Ok(None))"
        );

        std::fs::write(&store, b"not-a-dir").unwrap();
        assert!(
            pin_store_under(tmp.path()).is_err(),
            "non-directory opencode entry must be refused"
        );
        assert!(
            pin_opencode_store().is_err(),
            "non-directory store must fail closed, not skip"
        );

        std::fs::remove_file(&store).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("elsewhere"), &store).unwrap();
        assert!(
            pin_store_under(tmp.path()).is_err(),
            "symlinked opencode entry must be refused"
        );
        assert!(
            pin_opencode_store().is_err(),
            "symlink store must fail closed, not skip"
        );
    }

    /// SBX-04: F_GETPATH / canonical path resolution failure is fail-closed.
    #[cfg(target_os = "macos")]
    #[serial_test::serial(export_sandbox_env)]
    #[test]
    fn macos_pin_fgetpath_failure() {
        fn fail_resolve(_: &std::os::fd::OwnedFd, _: &std::path::Path) -> Result<String> {
            bail!("injected F_GETPATH failure");
        }
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("opencode")).unwrap();
        let _xdg = EnvVarGuard::set("XDG_DATA_HOME", tmp.path().to_str().expect("utf8"));
        let _guard = macos_test_hooks::PinPathResolverGuard::install(fail_resolve);
        let err = pin_opencode_store().expect_err("F_GETPATH failure must fail closed");
        let text = format!("{err:#}");
        assert!(
            text.contains("F_GETPATH") || text.contains("injected"),
            "got {text}"
        );
    }

    /// SBX-04: macOS pin shares `pin_store_under` with Linux.
    #[cfg(target_os = "macos")]
    #[serial_test::serial(export_sandbox_env)]
    #[test]
    fn macos_pin_shares_pin_store_under() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("opencode")).unwrap();
        let _xdg = EnvVarGuard::set("XDG_DATA_HOME", tmp.path().to_str().expect("utf8"));
        let under = pin_store_under(tmp.path()).expect("pin_store_under");
        let (fd, dest) = pin_opencode_store()
            .expect("pin_opencode_store Result")
            .expect("store present");
        let via_fgetpath = resolve_pinned_store_path(&under).expect("F_GETPATH");
        assert_eq!(
            PathBuf::from(&dest),
            via_fgetpath,
            "pin_opencode_store must use pin_store_under + F_GETPATH dest"
        );
        drop(fd);
    }

    /// SBX-04: transform on macOS selects seatbelt (`sandbox-exec`).
    #[cfg(target_os = "macos")]
    #[serial_test::serial(export_sandbox_env)]
    #[test]
    fn macos_transform_selects_seatbelt() {
        assert!(
            std::path::Path::new("/usr/bin/sandbox-exec").is_file(),
            "sandbox-exec must be present (DEP-SBX-02)"
        );
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir(&bin_dir).unwrap();
        let bin = fake_exporter(&bin_dir, r#"printf 'ok'"#);
        let store_root = tmp.path().join("xdg");
        std::fs::create_dir_all(store_root.join("opencode")).unwrap();
        let _xdg = EnvVarGuard::set("XDG_DATA_HOME", store_root.to_str().expect("utf8"));
        let assembled = assemble_sandboxed_export(&bin, None).expect("macOS transform");
        assert_eq!(
            assembled.program.as_os_str(),
            "/usr/bin/sandbox-exec",
            "transform must select seatbelt, got {}",
            assembled.program.display()
        );
        let joined = assembled.pre_args.join("\n");
        assert!(
            assembled.pre_args.iter().any(|a| a == "-p"),
            "seatbelt profile missing in {joined}"
        );
        assert!(
            joined.contains("(allow file-write*") || joined.contains("WRITABLE_BIND_"),
            "store write segment missing in {joined}"
        );
        assert!(
            joined.contains("(allow file-ioctl\n"),
            "seatbelt must emit the generated bind-scoped file-ioctl section \
             for SQLite WAL (the base policy's single-line tty ioctl clauses \
             do not count): {joined}"
        );
        assert!(
            joined.contains("/private/tmp/opencode") || joined.contains("/tmp/opencode"),
            "narrow OpenCode scratch bind missing in {joined}"
        );
        assert!(
            !joined
                .lines()
                .any(|l| l.ends_with("=/private/tmp") || l.ends_with("=/tmp")),
            "must not bind whole host /tmp: {joined}"
        );
        assert!(
            !joined.contains("network-outbound"),
            "Denied network must keep the seatbelt network segment empty"
        );
        let scratch = macos_opencode_scratch_bind().expect("scratch pin");
        assert_eq!(
            scratch.destination.as_os_str(),
            "/private/tmp/opencode",
            "scratch writable-bind must be exactly /private/tmp/opencode"
        );
    }

    /// FIX-SBX-01 R2: a pre-existing wide-mode scratch dir we own (the dev
    /// host ships /tmp/opencode as 0755) is tightened to 0700 through the
    /// pinned fd, not accepted as-is.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_scratch_bind_tightens_wide_mode() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let scratch = tmp.path().join("opencode");
        std::fs::create_dir(&scratch).unwrap();
        std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o755)).unwrap();
        let expected = scratch.canonicalize().unwrap();
        let bind = scratch_bind_under(tmp.path(), &expected).expect("owned wide dir tightened");
        assert_eq!(bind.destination, expected);
        let mode = std::fs::metadata(&scratch).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "group/other bits must be stripped");
    }

    /// FIX-SBX-01 R2: a scratch dir owned by another uid is refused —
    /// its owner could rename it under sticky /tmp and redirect the bind.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_scratch_bind_rejects_foreign_owner() {
        // SAFETY: geteuid has no failure mode.
        let euid = unsafe { libc::geteuid() };
        let err = validate_scratch_owner(euid.wrapping_add(1), euid)
            .expect_err("foreign owner must be refused");
        assert!(format!("{err:#}").contains("fail-closed"), "got {err:#}");
        validate_scratch_owner(euid, euid).expect("our own dir is acceptable");
    }

    /// FIX-SBX-01: a symlink planted at the scratch path cannot redirect
    /// the writable-bind (O_NOFOLLOW pin refuses it).
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_scratch_bind_refuses_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, tmp.path().join("opencode")).unwrap();
        let expected = target.canonicalize().unwrap();
        let err = scratch_bind_under(tmp.path(), &expected)
            .expect_err("symlinked scratch must be refused");
        assert!(format!("{err:#}").contains("fail-closed"), "got {err:#}");
    }

    /// SBX-04: missing sandbox-exec → fail-closed, no unsandboxed export.
    #[cfg(target_os = "macos")]
    #[serial_test::serial(export_sandbox_env)]
    #[tokio::test]
    async fn macos_sandbox_exec_missing_degrades() {
        let _missing = macos_test_hooks::SandboxExecMissingGuard::install();
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_exporter(dir.path(), r#"printf 'should-not-run'"#);
        let err = run_export_subprocess_sandboxed(&bin, "sess", ExportLimits::default())
            .await
            .expect_err("missing sandbox-exec must fail closed");
        let text = format!("{err:#}");
        assert!(
            text.contains("sandbox-exec") && text.contains("fail-closed"),
            "got {text}"
        );
        assert!(
            !text.contains("unsandboxed export ran"),
            "must not fall back to unsandboxed execution: {text}"
        );
    }

    /// SBX-03 D-group: a trusted, usable bwrap must be present. Missing or
    /// user-writable bwrap is a hard failure (never a skip-green).
    #[cfg(target_os = "linux")]
    #[serial_test::serial(export_sandbox_env)]
    #[test]
    fn trusted_bwrap_preflight() {
        assert!(
            trusted_bwrap_available(),
            "Linux D-group requires a trusted, usable bwrap (install bubblewrap)"
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
    fn bwrap_bind_index(args: &[String], source_prefix: &str, dest: &str) -> Option<usize> {
        args.windows(3)
            .position(|w| w[0] == "--bind" && w[1].starts_with(source_prefix) && w[2] == dest)
    }

    #[cfg(target_os = "linux")]
    fn bwrap_ro_bind_index(args: &[String], path: &str) -> Option<usize> {
        args.windows(3)
            .position(|w| w[0] == "--ro-bind" && w[1] == path && w[2] == path)
    }

    /// SBX-03: new-path argv matches the same-effect mount table (HOME/XDG
    /// before store bind, FD-pinned `/proc/self/fd/N → store`, binary-parent
    /// via sandbox_cwd, `--` then the exporter).
    #[cfg(target_os = "linux")]
    #[serial_test::serial(export_sandbox_env)]
    #[test]
    fn bwrap_argv_equivalent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let xdg_data = tmp.path().join("xdg-data");
        let xdg_config = tmp.path().join("xdg-config");
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&xdg_data).unwrap();
        std::fs::create_dir(&xdg_config).unwrap();
        std::fs::create_dir(&bin_dir).unwrap();
        std::fs::create_dir(xdg_data.join("opencode")).unwrap();
        let bin = fake_exporter(&bin_dir, r#"printf 'ok'"#);

        let _home = EnvVarGuard::set("HOME", home.to_str().expect("utf8 home"));
        let _xdg_data = EnvVarGuard::set("XDG_DATA_HOME", xdg_data.to_str().expect("utf8 data"));
        let _xdg_config =
            EnvVarGuard::set("XDG_CONFIG_HOME", xdg_config.to_str().expect("utf8 config"));

        let trusted = system_true_binary();
        let assembled =
            assemble_sandboxed_export(&bin, Some(&trusted)).expect("assemble export sandbox");
        let args = &assembled.pre_args;
        let home_s = home.to_string_lossy().into_owned();
        let data_s = xdg_data.to_string_lossy().into_owned();
        let config_s = xdg_config.to_string_lossy().into_owned();
        let cwd_s = bin_dir.to_string_lossy().into_owned();
        let store_s = xdg_data.join("opencode").to_string_lossy().into_owned();

        let home_i = bwrap_ro_bind_index(args, &home_s).expect("HOME ro-bind");
        let data_i = bwrap_ro_bind_index(args, &data_s).expect("XDG_DATA_HOME ro-bind");
        let config_i = bwrap_ro_bind_index(args, &config_s).expect("XDG_CONFIG_HOME ro-bind");
        let cwd_i = bwrap_ro_bind_index(args, &cwd_s).expect("sandbox_cwd/binary-parent ro-bind");
        let store_i = bwrap_bind_index(args, "/proc/self/fd/", &store_s)
            .expect("FD-pinned store writable-bind");
        assert!(
            home_i < store_i && data_i < store_i && config_i < store_i,
            "HOME/XDG ro-bind must precede store bind; home={home_i} data={data_i} \
             config={config_i} store={store_i} args={args:?}"
        );
        assert!(
            cwd_i < store_i,
            "sandbox_cwd (binary parent) must be bound before the store overlay; \
             cwd={cwd_i} store={store_i}"
        );
        let sep = args
            .iter()
            .position(|a| a == "--")
            .expect("bwrap argv must contain --");
        assert!(store_i < sep, "store bind must precede --");
        assert_eq!(
            args.get(sep + 1).map(String::as_str),
            Some(bin.to_string_lossy().as_ref()),
            "command tail must be the exporter binary; args={args:?}"
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
            1,
            "store pin must retain exactly one fd"
        );
    }

    /// SBX-03: user-writable trusted_bwrap_exe → Required fail-closed; no
    /// export bytes (hence no claim) are produced.
    #[cfg(target_os = "linux")]
    #[test]
    fn trusted_bwrap_rejects_user_writable() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_exporter(dir.path(), r#"printf 'should-not-run'"#);
        let fake = dir.path().join("bwrap");
        std::fs::write(&fake, b"#!/bin/sh\nexec \"$@\"\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = assemble_sandboxed_export(&bin, Some(&fake))
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
    fn backend_missing_degrades_metadata_only() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_exporter(dir.path(), r#"printf 'should-not-run'"#);
        let missing = dir.path().join("no-such-bwrap");
        let err = assemble_sandboxed_export(&bin, Some(&missing))
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
    #[serial_test::serial(export_sandbox_env)]
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
        let assembled =
            assemble_sandboxed_export(&bin, Some(&trusted)).expect("injected trusted_bwrap_exe");
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

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

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
