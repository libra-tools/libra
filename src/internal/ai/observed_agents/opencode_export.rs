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

    let mut command = tokio::process::Command::new(binary);
    command
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
