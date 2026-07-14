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
//!   while exiting success), bounded three ways — a `RLIMIT_FSIZE` write-time
//!   cap terminates a runaway child at the byte limit before it fills disk,
//!   the file size is actively polled against the same cap while the child
//!   runs, and it is re-checked after exit; over-cap always kills and errors,
//!   never returns truncated content. The whole run sits under a wall-clock
//!   deadline (default 3 s — expiry kills the child's process group). stderr
//!   is capped and redacted before it can appear in any error text
//!   (GC-DR-13). A child that leaves descendants in its process group after
//!   exit is killed without its output being accepted.
//!
//! Sandbox: the Required bwrap offline profile lives in
//! [`run_export_subprocess_sandboxed`] — network unshared, host paths and
//! HOME read-only, tmpfs `/tmp`, with ONE probe-verified exception: the
//! opencode data dir is bound read-write because its WAL-mode SQLite store
//! needs write access even for reads. Fail-closed without bwrap/non-Linux.

use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use tokio::io::AsyncReadExt;

use crate::internal::ai::observed_agents::{
    Redactor, TranscriptSource,
    transcript_source::ExportAuthorized,
    trust::{read_trust, revalidate_trust},
};

/// Trust-record slug for the OpenCode exporter binary.
const OPENCODE_TRUST_SLUG: &str = "opencode";
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
            "the 'opencode' binary is not trusted for export; run \
             'libra agent rpc trust opencode' (after verifying the binary) \
             to enable the OpenCode export bridge"
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

/// Core bounded runner: `<program> [<pre_args>…] export <session_id>` with
/// the module's env/caps/deadline contract. `pre_args` lets the sandboxed
/// variant prepend the bwrap arg vector while keeping ONE code path for the
/// bounds (GC-DR-04).
async fn run_bounded_exporter(
    program: &std::path::Path,
    pre_args: &[String],
    session_id: &str,
    limits: ExportLimits,
    keep_fds: Vec<std::os::fd::OwnedFd>,
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
    // GC-DR-04 write-time bound: cap the child's file writes at the byte cap
    // (+1 so overflow is DETECTED, not silently truncated). The kernel
    // delivers SIGXFSZ / EFBIG at the limit, terminating a runaway exporter
    // immediately instead of letting it fill disk until the deadline.
    #[cfg(unix)]
    {
        let fsize_limit = limits.max_bytes.saturating_add(1);
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
/// (`SandboxEnforcement::Required` semantics): Linux bubblewrap with
/// `--unshare-net` (no network), read-only host paths + read-only HOME (the
/// exporter's session store/config), tmpfs `/tmp` as the only writable
/// location, `--die-with-parent`, no shell anywhere. Fail-CLOSED when the
/// sandbox cannot be provided (bwrap missing, non-Linux): the capability is
/// unavailable — never a degraded unsandboxed run (GC-DR-14).
pub async fn run_export_subprocess_sandboxed(
    binary: &std::path::Path,
    session_id: &str,
    limits: ExportLimits,
) -> Result<Vec<u8>> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (binary, session_id, limits);
        bail!(
            "the OpenCode export sandbox profile is Linux-only for now; \
             refusing an unsandboxed export (fail-closed, GC-DR-14)"
        );
    }
    #[cfg(target_os = "linux")]
    {
        if !valid_session_id(session_id) {
            bail!("invalid OpenCode session id (expected alnum/dash/underscore, ≤64 chars)");
        }
        if !binary.is_absolute() {
            bail!("exporter binary path must be absolute (trusted provenance)");
        }
        let bwrap = resolve_trusted_bwrap()?;

        use crate::internal::ai::sandbox::{
            policy::SandboxPolicy, proxy::NetworkAccessMode, runtime::create_bwrap_command_args,
        };
        // ReadOnly policy + Denied network: the policy cwd is ro-bound,
        // /tmp is tmpfs (the ONLY writable location), net is unshared. The
        // policy cwd must NOT be /tmp — a later ro-bind would shadow the
        // tmpfs mount and leave the exporter with no writable scratch (bwrap
        // applies mounts in order). Use the binary's parent, which is
        // ro-bound anyway. The command tail handed to the builder is only
        // the binary; `export <sid>` is appended by the shared runner.
        let sandbox_cwd = binary
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| std::path::PathBuf::from("/usr"));
        let mut args = create_bwrap_command_args(
            vec![binary.to_string_lossy().into_owned()],
            NetworkAccessMode::Denied,
            &SandboxPolicy::ReadOnly,
            &sandbox_cwd,
            &[],
        );
        // The exporter must READ its own session store/config: ro-bind HOME
        // (and the XDG dirs when set) before the `--` separator.
        let sep = args
            .iter()
            .position(|a| a == "--")
            .ok_or_else(|| anyhow!("bwrap arg builder produced no separator"))?;
        let mut extra: Vec<String> = Vec::new();
        for var in ["HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME"] {
            if let Some(dir) = std::env::var_os(var).map(std::path::PathBuf::from)
                && dir.is_absolute()
                && dir.is_dir()
            {
                let d = dir.to_string_lossy().into_owned();
                extra.extend(["--ro-bind".to_string(), d.clone(), d]);
            }
        }
        // Task-card-verified exception (opencode 1.17.x probe, 2026-07-14):
        // the session store is WAL-mode SQLite, whose -wal/-shm side files
        // need WRITE access even for pure reads — a read-only bind makes
        // `export` fail with a generic error. Bind ONLY the opencode data
        // dir read-write (mounted after the ro HOME bind so it wins);
        // network stays unshared and everything else read-only.
        //
        // The store is resolved and PINNED in a SINGLE atomic `openat`
        // (Codex M3 R4 P1) that rejects a symlinked `opencode` entry
        // (`O_NOFOLLOW`) and requires a directory (`O_DIRECTORY`); the pinned
        // fd IS the validated directory, so a concurrent rename/exchange of
        // `opencode` cannot make the bound directory differ from the checked
        // one. It is bound via `/proc/self/fd/N` so the RW mount references the
        // exact pinned inode.
        let mut keep_fds: Vec<std::os::fd::OwnedFd> = Vec::new();
        if let Some((fd, dest)) = pin_opencode_store() {
            use std::os::fd::AsRawFd;
            let src = format!("/proc/self/fd/{}", fd.as_raw_fd());
            extra.extend(["--bind".to_string(), src, dest]);
            keep_fds.push(fd);
        }
        // The trusted binary itself may live outside the standard host paths
        // (e.g. ~/.opencode/bin) — HOME ro-bind above usually covers it; add
        // its parent dir defensively when it does not.
        if let Some(parent) = binary.parent() {
            let p = parent.to_string_lossy().into_owned();
            extra.extend(["--ro-bind".to_string(), p.clone(), p]);
        }
        let tail = args.split_off(sep);
        args.extend(extra);
        args.extend(tail);

        run_bounded_exporter(&bwrap, &args, session_id, limits, keep_fds).await
    }
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
    std::process::Command::new(&bwrap)
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
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
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
#[cfg(target_os = "linux")]
fn pin_opencode_store() -> Option<(std::os::fd::OwnedFd, String)> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .map(|h| h.join(".local/share"))
        })?;
    match pin_store_under(&base) {
        Ok(fd) => {
            let dest = base.join("opencode").to_string_lossy().into_owned();
            Some((fd, dest))
        }
        Err(err) => {
            tracing::warn!(
                error = %format!("{err:#}"),
                base = %base.display(),
                "cannot pin opencode data dir for RW bind; skipping (export may degrade)"
            );
            None
        }
    }
}

/// Resolution + pin as ONE atomic `openat` (Codex M3 R4 P1): open the data
/// root, then `openat` the literal `opencode` entry with
/// `O_PATH|O_DIRECTORY|O_NOFOLLOW`. Because the returned fd IS the validated
/// directory — there is no separate `stat` then re-`open` of the same path —
/// a concurrent rename/exchange of `opencode` between validation and bind
/// cannot make the bound directory differ from the checked one. `O_NOFOLLOW`
/// rejects a symlinked entry; `O_DIRECTORY` requires a directory. CLOEXEC is
/// cleared so the bwrap child inherits the fd for `/proc/self/fd/N` resolution.
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
    // SAFETY: base_c is a valid C string; the fd is wrapped for RAII below.
    let base_raw = unsafe {
        libc::open(
            base_c.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if base_raw < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("open opencode data root {}", base.display()));
    }
    // SAFETY: fresh owned fd from open(2).
    let base_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(base_raw) };

    // INVARIANT: a constant literal with no interior NUL.
    let name = std::ffi::CString::new("opencode").expect("literal has no NUL");
    // SAFETY: base_fd is a valid dir fd; name is a valid C string; the result
    // is wrapped for RAII.
    let raw = unsafe {
        libc::openat(
            base_fd.as_raw_fd(),
            name.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error())
            .context("pin opencode store (openat, no-follow directory)");
    }
    // SAFETY: fresh owned fd from openat(2).
    let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
    // Clear CLOEXEC so the bwrap child inherits it and can resolve
    // /proc/self/fd/N when it establishes the bind mount.
    // SAFETY: fcntl on our own fd; F_GETFD/F_SETFD have no memory effects.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error()).context("F_GETFD on pinned store fd");
    }
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error()).context("clear CLOEXEC on pinned store fd");
    }
    Ok(fd)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// Write an executable fake exporter script (tests never touch a real
    /// `opencode`, GC-DR-07). The script body receives argv untouched, which
    /// is exactly what the no-shell contract must preserve.
    fn fake_exporter(dir: &std::path::Path, body: &str) -> PathBuf {
        let path = dir.join("fake-opencode");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Whether an executable named `name` is resolvable on `PATH` (used to skip
    /// tests that depend on an optional system tool such as `setsid`).
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
        let bin = fake_exporter(dir.path(), "echo '{}'");
        for bad in ["", "../escape", "id with spaces", "a;b", "$(rm -rf /)"] {
            assert!(
                run_export_subprocess(&bin, bad, ExportLimits::default())
                    .await
                    .is_err(),
                "session id {bad:?} must be rejected before spawn"
            );
        }
    }

    /// opencode_export_argv_no_shell: metacharacters in a (valid-charset)
    /// session id reach the child as ONE argv element — no shell ever
    /// interprets them. The fake exporter prints its argv verbatim.
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
}
