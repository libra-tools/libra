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
//! - **Bounds** (GC-DR-04): stdout is stream-read with a hard byte cap
//!   (default 16 MiB — over-cap kills the child and errors, never
//!   truncates); the whole run sits under a wall-clock deadline (default
//!   3 s — expiry kills the child). stderr is capped and redacted before it
//!   can appear in any error text (GC-DR-13).
//!
//! Sandbox status: the plan's minimal offline profile
//! (`SandboxEnforcement::Required`, network disabled, read-only store) is the
//! remaining DR-04b hardening step — until it lands, callers MUST NOT wire
//! this bridge into the live hook path; the trust gate + env_clear + bounds
//! above are necessary but not yet the full task-card bar.

use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use tokio::io::AsyncReadExt;

use crate::internal::ai::observed_agents::{
    Redactor,
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
        .context("read opencode trust record")?
        .ok_or_else(|| {
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

    run_bounded_exporter(binary, &[], session_id, limits).await
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
) -> Result<Vec<u8>> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(pre_args)
        .arg("export")
        .arg(session_id)
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // Minimal env: the exporter must locate its own session store, nothing
    // else. Credentials/endpoints never pass (env_clear + explicit list).
    for name in ["HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }

    let mut child = command.spawn().context("spawn opencode export")?;
    let mut stdout = child.stdout.take().expect("stdout piped"); // INVARIANT: piped above
    let mut stderr = child.stderr.take().expect("stderr piped"); // INVARIANT: piped above

    let read_all = async {
        let mut out = Vec::new();
        // Read one past the cap so oversize is detected, never truncated
        // into a silently-partial export.
        let mut limited = (&mut stdout).take(limits.max_bytes.saturating_add(1));
        limited
            .read_to_end(&mut out)
            .await
            .context("read opencode export stdout")?;
        let mut err_buf = Vec::new();
        let _ = (&mut stderr)
            .take(EXPORT_MAX_STDERR_BYTES as u64)
            .read_to_end(&mut err_buf)
            .await;
        let status = child.wait().await.context("wait for opencode export")?;
        Ok::<_, anyhow::Error>((out, err_buf, status))
    };

    let (out, err_buf, status) = match tokio::time::timeout(limits.deadline, read_all).await {
        Ok(result) => result?,
        Err(_elapsed) => {
            // Deadline: kill and fail closed — a slow exporter must not eat
            // the hook budget (GC-DR-04).
            let _ = child.kill().await;
            bail!(
                "opencode export exceeded its {:?} deadline; killed (content \
                 skipped this idle — a later idle retries)",
                limits.deadline
            );
        }
    };

    if out.len() as u64 > limits.max_bytes {
        bail!(
            "opencode export exceeded the {} byte cap; refusing truncated content",
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
        let bwrap = std::env::var_os("LIBRA_LINUX_SANDBOX_EXE")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute() && p.is_file())
            .or_else(|| which_bwrap())
            .ok_or_else(|| {
                anyhow!(
                    "bubblewrap (bwrap) is required for the OpenCode export sandbox and was \
                     not found; install bwrap or set LIBRA_LINUX_SANDBOX_EXE (fail-closed)"
                )
            })?;

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

        run_bounded_exporter(&bwrap, &args, session_id, limits).await
    }
}

#[cfg(target_os = "linux")]
fn which_bwrap() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("bwrap"))
        .find(|candidate| candidate.is_file())
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
        if which_bwrap().is_none() && std::env::var_os("LIBRA_LINUX_SANDBOX_EXE").is_none() {
            eprintln!("skipped (bwrap not installed)");
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
    /// actionable hint (fail-closed; no PATH fallback).
    #[tokio::test]
    async fn opencode_export_untrusted_binary_fails_closed() {
        // No trust record exists in this isolated HOME.
        let home = tempfile::tempdir().unwrap();
        let prior = std::env::var_os("LIBRA_TEST_HOME");
        unsafe { std::env::set_var("LIBRA_TEST_HOME", home.path()) };
        let result = trusted_opencode_binary().await;
        unsafe {
            match prior {
                Some(v) => std::env::set_var("LIBRA_TEST_HOME", v),
                None => std::env::remove_var("LIBRA_TEST_HOME"),
            }
        }
        let err = result.expect_err("no trust record must fail closed");
        assert!(format!("{err:#}").contains("not trusted"), "got {err:#}");
    }
}
