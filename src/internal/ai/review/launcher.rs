//! Reviewer process launcher: §0.3.2 argv builder + minimal-allowlist
//! spawn skeleton.
//!
//! # Production argv (docs/development/tracing/plan.md §0.3.2, frozen)
//!
//! | slug | argv |
//! |---|---|
//! | `codex` | `codex exec -C <workspace> --skip-git-repo-check --sandbox read-only --json -o <file> <prompt>` |
//! | `claude-code` | `claude -p --permission-mode plan --output-format stream-json --verbose --include-hook-events --max-budget-usd <small> <prompt>` |
//! | `opencode` | `opencode run --dir <workspace> --format json --title <name> <prompt>` |
//!
//! The forbidden flags in [`FORBIDDEN_REVIEWER_FLAGS`] must never appear
//! (codex `--ephemeral`; claude `--bare` / `--safe-mode` /
//! `--no-session-persistence`; opencode
//! `--dangerously-skip-permissions`). When these upstream CLIs change,
//! re-verify against §0.3.2 before touching the argv.
//!
//! Launchability gates on the capability matrix's `launchable_review`
//! flag ([`crate::internal::ai::observed_agents::launchable_review_slugs`]
//! — the first-batch trio `claude-code` / `codex` / `opencode` since
//! AG-22), **not** on `supported`: supported ≠ launchable, and this is
//! the same fact source `agent list --json` renders, so the CLI roster
//! and the launcher can never disagree. Every other slug is a structured
//! [`ReviewerLaunchError::UnsupportedSlug`] — no process is ever spawned
//! for it (plan.md:945).
//!
//! # Spawn security contract (modeled on `observed_agents/rpc.rs`)
//!
//! - `env_clear()` + explicit allowlist ([`REVIEWER_ENV_ALLOWLIST`]):
//!   - `PATH` — required to resolve the reviewer CLI binary itself and
//!     the child processes it spawns (node, ripgrep, git, …);
//!   - `HOME` — required because all three CLIs read their auth token
//!     and configuration from the user's home (`~/.claude`, `~/.codex`,
//!     `~/.config/opencode`). This is the *only* secret channel the
//!     reviewer legitimately needs; provider API keys, `LIBRA_STORAGE_*`,
//!     `LIBRA_D1_*` and the rest of the parent environment never reach
//!     the reviewer. The first line of defense against exfiltration is
//!     that the isolated workspace contains no gitignored secret files.
//! - stdin/stdout/stderr are all piped (stdin is dropped immediately —
//!   reviewers get EOF; stdout/stderr feed the bounded sink).
//! - bounded ETXTBSY retry on spawn (parallel-fork mitigation).
//! - `kill_on_drop(true)` is the reap guard: an unwinding runner can
//!   never leak a running reviewer.
//! - the child is placed in its own process group (unix) so cancel can
//!   kill the reviewer *and* its child processes (process-tree depth 1
//!   per `agent.md:519-525` is reviewer-enforced; the group kill is our
//!   backstop).
//! - `current_dir` is **always** the isolated workspace, never the repo
//!   root (plan.md:947). [`ReviewerCommand`] deliberately has NO working
//!   directory field: no caller — production or test — can point a
//!   reviewer at the repo root; [`spawn_reviewer`] pins the cwd itself.
//!
//! # Test seam
//!
//! [`ReviewerCommand`] is plain data: production code builds it with
//! [`build_reviewer_command`]; integration tests construct it directly
//! with arbitrary fake programs (args may carry a `{workspace}` token
//! the runner substitutes). There is deliberately no env-var backdoor —
//! and no cwd override — in the production path.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use crate::internal::ai::observed_agents::launchable_review_slugs;

/// Default per-reviewer wall-clock budget. A reviewer past its deadline
/// is killed and its outcome recorded as `timed_out`.
pub const DEFAULT_REVIEWER_TIMEOUT: Duration = Duration::from_secs(300);

/// Default `--max-budget-usd` for the claude-code reviewer ("small" per
/// §0.3.2 — one review turn currently costs ≈ $0.13).
pub const DEFAULT_CLAUDE_REVIEW_MAX_BUDGET_USD: &str = "0.50";

/// Environment variables copied from the parent into a reviewer child.
/// Every entry must be documented in the module docs above; adding one
/// is a security-surface change.
pub const REVIEWER_ENV_ALLOWLIST: &[&str] = &["PATH", "HOME"];

/// Flags that must NEVER appear in a produced reviewer argv (§0.3.2).
pub const FORBIDDEN_REVIEWER_FLAGS: &[&str] = &[
    "--ephemeral",                    // codex
    "--bare",                         // claude
    "--safe-mode",                    // claude
    "--no-session-persistence",       // claude
    "--dangerously-skip-permissions", // opencode
];

/// A fully resolved reviewer process invocation.
///
/// Production instances come from [`build_reviewer_command`]; tests
/// construct the struct directly to substitute arbitrary fake commands.
/// There is intentionally no working-directory field — reviewers always
/// run in the isolated workspace root ([`spawn_reviewer`] enforces it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerCommand {
    /// Reviewer identity — names the run's log files and state rows.
    pub slug: String,
    /// Program resolved via the child's allowlisted `PATH`.
    pub program: PathBuf,
    pub args: Vec<String>,
    /// The COMPLETE child environment (the spawn applies `env_clear()`
    /// first, then exactly these pairs).
    pub env: Vec<(String, String)>,
    /// Per-reviewer wall-clock budget.
    pub timeout: Duration,
}

/// Inputs the production builder needs beyond the slug.
#[derive(Debug, Clone)]
pub struct ReviewerLaunchPlan {
    /// Root of the materialized isolated workspace (the mandatory
    /// reviewer working directory).
    pub workspace_root: PathBuf,
    /// The review prompt, passed as the final positional argument.
    pub prompt: String,
    /// Directory for reviewer-CLI side outputs (codex `-o`). The runner
    /// points this at the run's `reviewers/` dir and deletes the raw
    /// file at finalize — it is written by the external CLI and bypasses
    /// redaction, so it must never survive the run.
    pub scratch_dir: PathBuf,
    /// opencode `--title`.
    pub run_title: String,
    /// claude-code `--max-budget-usd`.
    pub claude_max_budget_usd: String,
    pub timeout: Duration,
}

/// Structured launcher failure.
#[derive(Debug, thiserror::Error)]
pub enum ReviewerLaunchError {
    /// The slug's capability-matrix row does not carry
    /// `launchable_review` (or the slug is unknown). No process was (or
    /// ever will be) spawned for it; manual attach is the only future
    /// fallback (plan.md:945).
    #[error("agent '{slug}' is not launchable for review; first-batch launchable agents: {roster}")]
    UnsupportedSlug { slug: String, roster: String },
    /// The reviewer binary could not be spawned.
    #[error("failed to spawn reviewer '{slug}' ({program}): {source}")]
    Spawn {
        slug: String,
        program: String,
        #[source]
        source: std::io::Error,
    },
}

/// The raw last-message file name the codex `-o` flag targets inside the
/// plan's `scratch_dir` (deleted at finalize; see [`ReviewerLaunchPlan`]).
pub const CODEX_LAST_MESSAGE_FILE: &str = "codex.last-message.raw.tmp";

/// Build the production real-CLI invocation for a review-launchable
/// agent per §0.3.2. Any other slug — supported-but-not-launchable,
/// preview, quarantined, external, or unknown — returns
/// [`ReviewerLaunchError::UnsupportedSlug`] without side effects.
pub fn build_reviewer_command(
    slug: &str,
    plan: &ReviewerLaunchPlan,
) -> Result<ReviewerCommand, ReviewerLaunchError> {
    if !is_launchable_reviewer(slug) {
        return Err(unsupported_reviewer_error(slug));
    }
    let workspace = plan.workspace_root.display().to_string();
    let (program, args): (&str, Vec<String>) = match slug {
        "codex" => (
            "codex",
            vec![
                "exec".into(),
                "-C".into(),
                workspace,
                "--skip-git-repo-check".into(),
                "--sandbox".into(),
                "read-only".into(),
                "--json".into(),
                "-o".into(),
                plan.scratch_dir
                    .join(CODEX_LAST_MESSAGE_FILE)
                    .display()
                    .to_string(),
                plan.prompt.clone(),
            ],
        ),
        "claude-code" => (
            "claude",
            vec![
                "-p".into(),
                "--permission-mode".into(),
                "plan".into(),
                "--output-format".into(),
                "stream-json".into(),
                "--verbose".into(),
                "--include-hook-events".into(),
                "--max-budget-usd".into(),
                plan.claude_max_budget_usd.clone(),
                plan.prompt.clone(),
            ],
        ),
        "opencode" => (
            "opencode",
            vec![
                "run".into(),
                "--dir".into(),
                workspace,
                "--format".into(),
                "json".into(),
                "--title".into(),
                plan.run_title.clone(),
                plan.prompt.clone(),
            ],
        ),
        // A slug can only reach here if the registry marks it
        // launchable_review without a §0.3.2 argv row — a wiring bug
        // this fail-closed arm keeps loud but non-fatal.
        other => return Err(unsupported_reviewer_error(other)),
    };
    debug_assert!(
        args.iter()
            .all(|arg| !FORBIDDEN_REVIEWER_FLAGS.contains(&arg.as_str())),
        "forbidden reviewer flag leaked into §0.3.2 argv"
    );
    Ok(ReviewerCommand {
        slug: slug.to_string(),
        program: PathBuf::from(program),
        args,
        env: reviewer_env_allowlist(),
        timeout: plan.timeout,
    })
}

/// Whether a slug is launchable for review: gated on the capability
/// matrix's `launchable_review` flag (the same fact source
/// `agent list --json` renders), never on `supported` alone.
pub fn is_launchable_reviewer(slug: &str) -> bool {
    launchable_review_slugs().contains(&slug)
}

/// The structured rejection for a non-launchable slug (shared by the
/// builder and the runner's pre-validation).
pub fn unsupported_reviewer_error(slug: &str) -> ReviewerLaunchError {
    ReviewerLaunchError::UnsupportedSlug {
        slug: slug.to_string(),
        roster: launchable_review_slugs().join(", "),
    }
}

/// Snapshot the allowlisted environment ([`REVIEWER_ENV_ALLOWLIST`])
/// from the parent process. Unset variables are simply omitted.
pub fn reviewer_env_allowlist() -> Vec<(String, String)> {
    REVIEWER_ENV_ALLOWLIST
        .iter()
        .filter_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| (key.to_string(), value))
        })
        .collect()
}

/// A spawned reviewer child. `kill_on_drop(true)` is the RAII reap
/// guard; [`Self::kill_tree`] is the cancel/timeout path.
pub struct SpawnedReviewer {
    pub slug: String,
    pub child: tokio::process::Child,
    /// Process-group id (== the child's pid: the spawn puts the child in
    /// its own group). Captured at spawn so the group can still be
    /// killed after the direct child was reaped (drain-grace path) and
    /// so the run store can record it for orphaned-run cancel.
    pub pgid: Option<u32>,
    /// The child's kernel start time (`/proc/<pid>/stat` field 22, in
    /// clock ticks; Linux only, `None` elsewhere). Recorded alongside
    /// the pid/pgid as process *provenance*: pids are reused, so an
    /// orphaned-run cancel must never SIGKILL a recorded pgid unless
    /// the process at that pid still has this exact start time.
    pub start_ticks: Option<u64>,
}

impl SpawnedReviewer {
    /// Kill the reviewer and (on unix) its whole process group, then
    /// reap it. Used by both the cancel path and the per-reviewer
    /// timeout; safe to call after the child already exited.
    pub async fn kill_tree(&mut self) {
        if let Some(pgid) = self.pgid {
            kill_process_group(pgid);
        }
        // Tokio's kill() sends SIGKILL to the direct child and reaps it.
        let _ = self.child.kill().await;
    }
}

/// Send SIGKILL to a whole process group (unix; no-op elsewhere).
/// Guarded against pgid 0/1 so a corrupt value can never signal "every
/// process in our group" or init.
pub(crate) fn kill_process_group(pgid: u32) {
    #[cfg(unix)]
    if pgid > 1 {
        // SAFETY: plain libc syscall on a process group we created (or,
        // on the orphan path, one recorded by us at spawn). Failure
        // (e.g. the group is already gone) is benign and ignored.
        unsafe {
            libc::kill(-(pgid as libc::pid_t), libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pgid;
}

/// Whether any process in the group is still alive (unix; `false`
/// elsewhere — non-unix cannot probe groups).
pub(crate) fn process_group_alive(pgid: u32) -> bool {
    #[cfg(unix)]
    {
        if pgid <= 1 {
            return false;
        }
        // SAFETY: signal 0 only probes deliverability, sends nothing.
        unsafe { libc::kill(-(pgid as libc::pid_t), 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pgid;
        false
    }
}

/// The kernel start time of `pid` in clock ticks (`/proc/<pid>/stat`
/// field 22). `None` when the process does not exist, `/proc` is
/// unreadable, or the platform is not Linux.
///
/// This is the process-provenance anchor for the orphaned-run cancel:
/// a (pid, start_ticks) pair uniquely identifies a process incarnation,
/// so a recorded reviewer pgid whose current start time does not match
/// the recorded one is a *reused* pid — an unrelated process that must
/// never be killed.
pub fn process_start_ticks(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        parse_stat_start_ticks(&stat)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

/// Parse field 22 (`starttime`) out of a `/proc/<pid>/stat` line.
///
/// The line is `pid (comm) state ppid pgrp …`; `comm` is NOT escaped and
/// may itself contain spaces and parentheses (even `") ("`), so the only
/// safe anchor is the LAST `)` — everything after it is the
/// space-separated tail starting at field 3 (`state`). Field 22 is
/// therefore index 19 of that tail.
#[cfg(any(target_os = "linux", test))]
fn parse_stat_start_ticks(stat: &str) -> Option<u64> {
    let tail = &stat[stat.rfind(')')? + 1..];
    tail.split_whitespace().nth(19)?.parse().ok()
}

/// Spawn a reviewer with the module's security contract applied
/// (`env_clear` + allowlist, piped stdio, own process group, ETXTBSY
/// retry, `kill_on_drop` reap guard). The working directory is ALWAYS
/// `workspace_root` — the isolated workspace; there is no override.
pub async fn spawn_reviewer(
    command: &ReviewerCommand,
    workspace_root: &Path,
) -> Result<SpawnedReviewer, ReviewerLaunchError> {
    let mut cmd = tokio::process::Command::new(&command.program);
    cmd.args(&command.args)
        .env_clear()
        .envs(command.env.iter().cloned())
        .current_dir(workspace_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);

    // Bounded ETXTBSY (os error 26) retry: a concurrent fork can hold a
    // just-written executable's write fd open across exec; retrying
    // briefly is the standard mitigation (see rpc.rs spawn).
    let mut attempt = 0u8;
    let mut child = loop {
        match cmd.spawn() {
            Ok(child) => break child,
            Err(err) if err.raw_os_error() == Some(26 /* ETXTBSY */) && attempt < 5 => {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(err) => {
                return Err(ReviewerLaunchError::Spawn {
                    slug: command.slug.clone(),
                    program: command.program.display().to_string(),
                    source: err,
                });
            }
        }
    };
    // Reviewers get no input: drop the piped stdin so they see EOF
    // immediately instead of blocking on a read.
    drop(child.stdin.take());
    let pgid = child.id();
    // Provenance for the orphaned-run cancel: capture the incarnation's
    // start time while the child is definitely alive (we still hold it).
    let start_ticks = pgid.and_then(process_start_ticks);
    Ok(SpawnedReviewer {
        slug: command.slug.clone(),
        child,
        pgid,
        start_ticks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal::ai::observed_agents::supported_slugs;

    fn plan() -> ReviewerLaunchPlan {
        ReviewerLaunchPlan {
            workspace_root: PathBuf::from("/ws/root"),
            prompt: "review the diff".into(),
            scratch_dir: PathBuf::from("/runs/r1/reviewers"),
            run_title: "libra-review-r1".into(),
            claude_max_budget_usd: DEFAULT_CLAUDE_REVIEW_MAX_BUDGET_USD.into(),
            timeout: DEFAULT_REVIEWER_TIMEOUT,
        }
    }

    #[test]
    fn codex_argv_matches_section_0_3_2_exactly() {
        let cmd = build_reviewer_command("codex", &plan()).expect("codex is launchable");
        assert_eq!(cmd.program, PathBuf::from("codex"));
        assert_eq!(
            cmd.args,
            vec![
                "exec",
                "-C",
                "/ws/root",
                "--skip-git-repo-check",
                "--sandbox",
                "read-only",
                "--json",
                "-o",
                "/runs/r1/reviewers/codex.last-message.raw.tmp",
                "review the diff",
            ]
        );
    }

    #[test]
    fn claude_argv_matches_section_0_3_2_exactly() {
        let cmd = build_reviewer_command("claude-code", &plan()).expect("claude-code launchable");
        assert_eq!(cmd.program, PathBuf::from("claude"));
        assert_eq!(
            cmd.args,
            vec![
                "-p",
                "--permission-mode",
                "plan",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-hook-events",
                "--max-budget-usd",
                "0.50",
                "review the diff",
            ]
        );
    }

    #[test]
    fn opencode_argv_matches_section_0_3_2_exactly() {
        let cmd = build_reviewer_command("opencode", &plan()).expect("opencode launchable");
        assert_eq!(cmd.program, PathBuf::from("opencode"));
        assert_eq!(
            cmd.args,
            vec![
                "run",
                "--dir",
                "/ws/root",
                "--format",
                "json",
                "--title",
                "libra-review-r1",
                "review the diff",
            ]
        );
    }

    #[test]
    fn forbidden_flags_are_never_present_for_any_launchable_slug() {
        for slug in launchable_review_slugs() {
            let cmd = build_reviewer_command(slug, &plan()).expect("launchable");
            for flag in FORBIDDEN_REVIEWER_FLAGS {
                assert!(
                    !cmd.args.iter().any(|arg| arg == flag),
                    "{slug}: forbidden flag {flag} present in argv {:?}",
                    cmd.args
                );
            }
        }
    }

    /// The launchability gate is `launchable_review` (registry fact
    /// source), the builder covers exactly that roster, and — the AG-22
    /// invariant — every launchable slug is also supported (launchable
    /// implies supported, never the reverse).
    #[test]
    fn builder_covers_exactly_the_launchable_review_roster() {
        assert_eq!(
            launchable_review_slugs(),
            ["claude-code", "codex", "opencode"],
            "AG-22 launches exactly the first-batch trio"
        );
        for slug in launchable_review_slugs() {
            assert!(
                build_reviewer_command(slug, &plan()).is_ok(),
                "launchable slug '{slug}' must build a §0.3.2 argv"
            );
            assert!(is_launchable_reviewer(slug));
            assert!(
                supported_slugs().contains(&slug),
                "'{slug}': launchable_review implies supported"
            );
        }
    }

    #[test]
    fn non_first_batch_slugs_are_structured_unsupported_errors() {
        for slug in [
            "gemini",
            "cursor",
            "copilot-cli",
            "factory-ai",
            "pi",
            "vogon",
            "not-a-cli",
        ] {
            let err = build_reviewer_command(slug, &plan()).expect_err("must be rejected");
            match err {
                ReviewerLaunchError::UnsupportedSlug { slug: got, roster } => {
                    assert_eq!(got, slug);
                    assert_eq!(roster, "claude-code, codex, opencode");
                }
                other => panic!("expected UnsupportedSlug, got {other:?}"),
            }
            assert!(!is_launchable_reviewer(slug));
        }
    }

    /// Field-22 extraction must survive a hostile `comm`: the process
    /// name is unescaped in `/proc/<pid>/stat` and may contain spaces
    /// and parentheses — including `") ("` — so only the LAST `)` is a
    /// safe anchor.
    #[test]
    fn stat_start_ticks_parser_survives_parens_and_spaces_in_comm() {
        // Plain comm: fields 3..22 follow the ')'; starttime (field 22)
        // is the 20th tail field.
        let plain = "1234 (sleep) S 1 1234 1234 0 -1 4194560 100 0 0 0 0 0 0 0 20 0 1 0 \
                     987654321 2216 173 18446744073709551615";
        assert_eq!(parse_stat_start_ticks(plain), Some(987654321));

        // Hostile comm embedding ") (" — a naive first-')' or split
        // would misalign every field.
        let hostile = "4321 (evil) (comm r) R 1 4321 4321 0 -1 4194560 100 0 0 0 0 0 0 0 20 0 1 \
                       0 123456789 2216 173 18446744073709551615";
        assert_eq!(parse_stat_start_ticks(hostile), Some(123456789));

        // Malformed inputs yield None, never a panic or a bogus value.
        assert_eq!(parse_stat_start_ticks(""), None);
        assert_eq!(parse_stat_start_ticks("1234 (short) S 1"), None);
        assert_eq!(parse_stat_start_ticks("no parens at all"), None);
    }

    /// On Linux the live helper agrees with the parser for our own
    /// process (and a dead/invalid pid yields None).
    #[cfg(target_os = "linux")]
    #[test]
    fn process_start_ticks_reads_the_live_proc_entry() {
        let own = std::process::id();
        assert!(
            process_start_ticks(own).is_some(),
            "our own /proc/<pid>/stat must parse"
        );
        // pid 0 never has a /proc entry.
        assert_eq!(process_start_ticks(0), None);
    }

    #[test]
    fn reviewer_env_is_exactly_the_documented_allowlist() {
        let cmd = build_reviewer_command("codex", &plan()).expect("codex launchable");
        for (key, _) in &cmd.env {
            assert!(
                REVIEWER_ENV_ALLOWLIST.contains(&key.as_str()),
                "env var {key} is not in the reviewer allowlist"
            );
        }
        // PATH is effectively always set in a test environment; assert it
        // flows through so the child can resolve its binary.
        assert!(
            cmd.env.iter().any(|(key, _)| key == "PATH"),
            "PATH must be forwarded to the reviewer"
        );
        // No key may appear twice.
        let mut keys: Vec<&str> = cmd.env.iter().map(|(k, _)| k.as_str()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), cmd.env.len());
    }
}
