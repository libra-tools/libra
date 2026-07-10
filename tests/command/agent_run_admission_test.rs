//! A0-04: the shared run-level admission queue gates BOTH `libra review` and
//! `libra investigate` — a saturated queue refuses a fresh run of either kind
//! fail-closed with `LBR-AGENT-014`.

use std::{path::Path, process};

use super::{create_committed_repo_via_cli, run_libra_command};

/// Seed the shared admission directory to full: `max_concurrent_runs` (default
/// 2) occupied slots + the queue cap (10) waiting tickets, each owned by this
/// live test process so the spawned CLI's stale-reclaim keeps them.
fn seed_full_admission(repo: &Path) {
    let admission = repo
        .join(".libra")
        .join("sessions")
        .join("agent-runs")
        .join(".admission");
    let pid = process::id().to_string();
    for (dir, n) in [("slots", 2usize), ("queue", 10usize)] {
        let d = admission.join(dir);
        std::fs::create_dir_all(&d).expect("create admission subdir");
        for i in 0..n {
            std::fs::write(d.join(format!("seed-{dir}-{i:03}")), &pid).expect("write ticket");
        }
    }
}

#[test]
fn agent_run_queue_limit() {
    let repo = create_committed_repo_via_cli();
    seed_full_admission(repo.path());

    // A full queue rejects a fresh REVIEW run fail-closed…
    let out = run_libra_command(&["review", "--agent", "codex"], repo.path());
    assert!(
        !out.status.success(),
        "a saturated queue must refuse a review run: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-AGENT-014"),
        "review refusal must carry LBR-AGENT-014: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // …and the SAME shared queue rejects a fresh INVESTIGATE run — the two run
    // kinds share one concurrency budget.
    let out = run_libra_command(
        &[
            "investigate",
            "start",
            "--topic",
            "why is X slow",
            "--agent",
            "codex",
        ],
        repo.path(),
    );
    assert!(
        !out.status.success(),
        "a saturated queue must refuse an investigate run: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-AGENT-014"),
        "investigate refusal must carry LBR-AGENT-014: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
