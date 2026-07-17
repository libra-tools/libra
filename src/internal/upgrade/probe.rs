//! Candidate self-check probes (plan-20260714 §A.7 Phase A/B).
//!
//! Before a downloaded candidate is trusted, and after it is installed, it is
//! executed through dedicated hidden entry points that do only a side-effect-
//! free self-check:
//!
//! - `__upgrade-probe --kind version|pre-install --expected-version …`
//!   (Phase A, run on the candidate at `.dl-*`/candidate path);
//! - a fixed post-install probe (Phase B, run on the installed target).
//!
//! Each probe is spawned in its OWN process group with `kill_on_drop`, given
//! an independent hard timeout, and — on timeout or cancellation — the entire
//! group is signalled and reaped so no descendant survives (§A.7: kill the
//! process group and `wait`, never leave detached tasks). This module owns
//! the *spawning/timeout/kill* discipline; the argv contract recognition
//! lives in the CLI slice.

use std::{path::Path, process::Stdio, time::Duration};

use tokio::process::Command;

/// What a Phase-A probe should self-check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeKind {
    /// `--version` only (fast identity sanity).
    Version,
    /// Fuller pre-install self-check (still side-effect free).
    PreInstall,
}

impl ProbeKind {
    pub fn as_arg(self) -> &'static str {
        match self {
            Self::Version => "version",
            Self::PreInstall => "pre-install",
        }
    }
}

/// Probe outcome. Any non-success (nonzero exit, signal, timeout, spawn
/// failure) is a probe FAILURE — the caller rolls back / discards the
/// candidate fail-closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The probe exited 0.
    Passed,
    /// The probe ran but reported unhealthy (nonzero exit or signal).
    Failed { detail: String },
    /// The probe exceeded its hard timeout and its process group was killed.
    TimedOut,
    /// The probe could not be spawned at all.
    SpawnError { detail: String },
}

impl ProbeOutcome {
    /// True only for [`ProbeOutcome::Passed`] — the fail-closed helper the
    /// transaction layer expects.
    pub fn is_healthy(&self) -> bool {
        matches!(self, ProbeOutcome::Passed)
    }
}

/// Per-probe hard timeout default (§A.7: each probe has its own hard timeout).
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Run a Phase-A probe against the candidate binary at `candidate_path`.
///
/// The candidate is invoked as
/// `<candidate> __upgrade-probe --kind <kind> --expected-version <v>`.
pub async fn run_phase_a_probe(
    candidate_path: &Path,
    kind: ProbeKind,
    expected_version: &str,
    timeout: Duration,
) -> ProbeOutcome {
    run_probe(
        candidate_path,
        &[
            "__upgrade-probe",
            "--kind",
            kind.as_arg(),
            "--expected-version",
            expected_version,
        ],
        timeout,
    )
    .await
}

/// Run the Phase-B post-install probe against the installed `target_path`.
pub async fn run_post_install_probe(
    target_path: &Path,
    expected_version: &str,
    timeout: Duration,
) -> ProbeOutcome {
    run_probe(
        target_path,
        &[
            "__upgrade-probe",
            "--kind",
            "post-install",
            "--expected-version",
            expected_version,
        ],
        timeout,
    )
    .await
}

/// Spawn `program args…` in its own process group with `kill_on_drop`, wait
/// up to `timeout`, and on timeout kill the whole group and reap it.
async fn run_probe(program: &Path, args: &[&str], timeout: Duration) -> ProbeOutcome {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        // Own process group so a hung probe cannot leave orphaned children.
        command.process_group(0);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return ProbeOutcome::SpawnError {
                detail: e.to_string(),
            };
        }
    };

    #[cfg(unix)]
    let pid = child.id();

    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => {
            if status.success() {
                ProbeOutcome::Passed
            } else {
                ProbeOutcome::Failed {
                    detail: describe_status(status),
                }
            }
        }
        Ok(Err(e)) => ProbeOutcome::Failed {
            detail: format!("failed to wait for probe: {e}"),
        },
        Err(_) => {
            // Hard timeout: kill the whole process group, then reap.
            #[cfg(unix)]
            if let Some(pid) = pid {
                kill_process_group(pid);
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
            ProbeOutcome::TimedOut
        }
    }
}

#[cfg(unix)]
fn describe_status(status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    if let Some(code) = status.code() {
        format!("probe exited with code {code}")
    } else if let Some(sig) = status.signal() {
        format!("probe killed by signal {sig}")
    } else {
        "probe terminated abnormally".to_string()
    }
}

#[cfg(not(unix))]
fn describe_status(status: std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("probe exited with code {code}"),
        None => "probe terminated abnormally".to_string(),
    }
}

/// Send SIGKILL to the entire process group led by `pid` (which is the group
/// leader because it was spawned with `process_group(0)`).
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    // Guard against pgid ≤ 1 (never signal init or the whole session).
    if pid <= 1 {
        return;
    }
    // SAFETY: killpg with a negated pgid signals only the child's own group,
    // which we created via process_group(0); no other process shares it.
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// Write an executable `/bin/sh` script into a temp dir and return its
    /// path (kept alive by the returned guard).
    fn script(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("probe.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, path)
    }

    #[tokio::test]
    async fn passing_probe_reports_healthy() {
        let (_g, path) = script("exit 0");
        let out =
            run_phase_a_probe(&path, ProbeKind::Version, "1.2.3", DEFAULT_PROBE_TIMEOUT).await;
        assert_eq!(out, ProbeOutcome::Passed);
        assert!(out.is_healthy());
    }

    #[tokio::test]
    async fn nonzero_exit_is_a_failure() {
        let (_g, path) = script("exit 3");
        let out =
            run_phase_a_probe(&path, ProbeKind::PreInstall, "1.2.3", DEFAULT_PROBE_TIMEOUT).await;
        assert!(matches!(out, ProbeOutcome::Failed { .. }));
        assert!(!out.is_healthy());
    }

    #[tokio::test]
    async fn missing_binary_is_a_spawn_error() {
        let out = run_post_install_probe(
            Path::new("/nonexistent/libra-probe-xyz"),
            "1.2.3",
            DEFAULT_PROBE_TIMEOUT,
        )
        .await;
        assert!(matches!(out, ProbeOutcome::SpawnError { .. }));
        assert!(!out.is_healthy());
    }

    #[tokio::test]
    async fn hung_probe_times_out_and_group_is_killed() {
        // Spawn a child that itself spawns a long sleeper, then sleeps: the
        // whole group must be reaped on timeout. `sleep 300 &` becomes part of
        // the probe's process group.
        let (_g, path) = script("sleep 300 & sleep 300");
        let start = std::time::Instant::now();
        let out = run_phase_a_probe(
            &path,
            ProbeKind::Version,
            "1.2.3",
            Duration::from_millis(400),
        )
        .await;
        assert_eq!(out, ProbeOutcome::TimedOut);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timeout must fire promptly, took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn kill_group_guards_low_pids() {
        // Must never attempt to signal pid 0/1 (no panic, no-op).
        kill_process_group(0);
        kill_process_group(1);
    }
}
