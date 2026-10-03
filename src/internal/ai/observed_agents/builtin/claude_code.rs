//! Claude Code [`ObservedAgent`] adapter, paired with a
//! [`TranscriptTruncator`] capability so `libra agent checkpoint rewind
//! --apply` can rewrite the on-disk transcript JSONL — phase-4 item 1
//! per `docs/development/commands/_general.md` §14.4.
//!
//! Claude Code stores its session as line-delimited JSON. Each line has
//! a top-level `timestamp` field (ISO-8601 / RFC-3339). The truncator
//! drops every line whose timestamp is strictly greater than the
//! checkpoint boundary; lines with non-parseable or missing timestamps
//! are kept (they're typically session-meta records that pre-date the
//! first message, so dropping them would lose context).

use std::{
    fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};

use super::super::{
    adapter::{AgentKind, AgentSessionCtx, ObservedAgent, TranscriptTruncator},
    capability::{
        ModelExtractor, PromptExtractor, SkillEvent, SkillEventExtractor, SubagentAwareExtractor,
        SubagentExtraction, TokenCalculator, TranscriptAnalyzer, TranscriptPreparer,
    },
    extract,
};
use crate::internal::ai::{
    completion::CompletionUsageSummary,
    hooks::{provider::HookProvider, providers::claude::ClaudeProvider},
};

/// Hard cap on how many bytes the transcript reader will pull off disk.
/// Claude Code transcripts grow with conversation length; in practice
/// they sit under a few MB even for long-running sessions, so 16 MB is a
/// generous ceiling that still protects against a runaway file.
const MAX_TRANSCRIPT_BYTES: u64 = 16 * 1024 * 1024;

/// Stable adapter for Claude Code (`AgentKind::ClaudeCode`).
///
/// Held as a unit struct so the `ObservedAgent` registry can return a
/// `&'static dyn ObservedAgent` without lifetime gymnastics.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeCodeObservedAgent;

impl ClaudeCodeObservedAgent {
    pub const fn new() -> Self {
        Self
    }
}

impl ObservedAgent for ClaudeCodeObservedAgent {
    fn provider_kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    fn provider_name(&self) -> &'static str {
        "claude-code"
    }

    fn read_transcript(&self, session: &AgentSessionCtx) -> Result<Option<Vec<u8>>> {
        let Some(path) = session.transcript_path.as_ref() else {
            return Ok(None);
        };
        match fs::metadata(path) {
            Ok(meta) if meta.len() == 0 => Ok(Some(Vec::new())),
            Ok(meta) if meta.len() > MAX_TRANSCRIPT_BYTES => Err(anyhow!(
                "transcript at {} exceeds {} byte cap; refusing to load",
                path.display(),
                MAX_TRANSCRIPT_BYTES
            )),
            Ok(_) => {
                let bytes = fs::read(path)
                    .with_context(|| format!("read transcript {}", path.display()))?;
                Ok(Some(bytes))
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => {
                Err(anyhow!(err)).with_context(|| format!("stat transcript {}", path.display()))
            }
        }
    }

    fn protected_dirs(&self) -> &'static [&'static str] {
        &[".claude"]
    }

    /// Claude Code is the one first-batch agent whose `HookProvider`
    /// already exists; expose it so `declared_capabilities()` matches the
    /// static registry row (AG-16 coherence). Runtime hook dispatch still
    /// goes through the provider registry until AG-19 removes the string
    /// bridge.
    fn as_hooks(&self) -> Option<&dyn HookProvider> {
        static CLAUDE_HOOKS: ClaudeProvider = ClaudeProvider;
        Some(&CLAUDE_HOOKS)
    }

    // AG-21 transcript intelligence: Claude Code session JSONL supports
    // the full extraction surface (see `observed_agents::extract`).
    fn as_transcript_preparer(&self) -> Option<&dyn TranscriptPreparer> {
        Some(self)
    }
    fn as_transcript_analyzer(&self) -> Option<&dyn TranscriptAnalyzer> {
        Some(self)
    }
    fn as_prompt_extractor(&self) -> Option<&dyn PromptExtractor> {
        Some(self)
    }
    fn as_token_calculator(&self) -> Option<&dyn TokenCalculator> {
        Some(self)
    }
    fn as_model_extractor(&self) -> Option<&dyn ModelExtractor> {
        Some(self)
    }
    fn as_subagent_aware_extractor(&self) -> Option<&dyn SubagentAwareExtractor> {
        Some(self)
    }
    fn as_skill_event_extractor(&self) -> Option<&dyn SkillEventExtractor> {
        Some(self)
    }
}

fn tail(data: &[u8], from_offset: usize) -> &[u8] {
    &data[from_offset.min(data.len())..]
}

impl TranscriptAnalyzer for ClaudeCodeObservedAgent {
    fn transcript_position(&self, data: &[u8]) -> Result<usize> {
        Ok(data.len())
    }
    fn extract_modified_files_from_offset(
        &self,
        data: &[u8],
        from_offset: usize,
    ) -> Result<Vec<PathBuf>> {
        Ok(extract::extract_claude_code(tail(data, from_offset))
            .modified_files
            .into_iter()
            .map(PathBuf::from)
            .collect())
    }
}

impl PromptExtractor for ClaudeCodeObservedAgent {
    fn extract_prompts(&self, data: &[u8], from_offset: usize) -> Result<Vec<String>> {
        Ok(extract::extract_claude_code(tail(data, from_offset)).prompts)
    }
}

impl TokenCalculator for ClaudeCodeObservedAgent {
    fn calculate_token_usage(
        &self,
        data: &[u8],
        from_offset: usize,
    ) -> Result<CompletionUsageSummary> {
        Ok(extract::extract_claude_code(tail(data, from_offset))
            .usage
            .unwrap_or_default())
    }
}

impl ModelExtractor for ClaudeCodeObservedAgent {
    fn extract_model(&self, data: &[u8]) -> Result<Option<String>> {
        Ok(extract::extract_claude_code(data).model)
    }
}

impl SubagentAwareExtractor for ClaudeCodeObservedAgent {
    fn extract_all_modified_files(&self, data: &[u8]) -> Result<Vec<PathBuf>> {
        Ok(extract::extract_claude_code(data)
            .modified_files
            .into_iter()
            .map(PathBuf::from)
            .collect())
    }

    fn total_token_usage_including_subagents(&self, data: &[u8]) -> Result<CompletionUsageSummary> {
        Ok(extract::extract_claude_code(data).usage.unwrap_or_default())
    }

    fn extract_parent_and_subagents(
        &self,
        parent: &[u8],
        subagents: &[&[u8]],
    ) -> Result<SubagentExtraction> {
        let parent_summary = extract::extract_claude_code(parent);
        let mut modified_files = parent_summary.modified_files.clone();
        let mut aggregate_usage = parent_summary.usage.clone().unwrap_or_default();
        let mut subagent_usage: Option<CompletionUsageSummary> = None;
        let mut partial = parent_summary.partial;
        let mut warnings = parent_summary.warnings;

        for data in subagents {
            let summary = extract::extract_claude_code(data);
            for path in summary.modified_files {
                if !modified_files.iter().any(|existing| existing == &path) {
                    modified_files.push(path);
                }
            }
            if let Some(usage) = summary.usage {
                aggregate_usage.merge(&usage);
                match &mut subagent_usage {
                    Some(total) => total.merge(&usage),
                    None => subagent_usage = Some(usage),
                }
            }
            partial |= summary.partial;
            warnings.extend(
                summary
                    .warnings
                    .into_iter()
                    .map(|warning| format!("subagent transcript: {warning}")),
            );
        }

        Ok(SubagentExtraction {
            modified_files: modified_files.into_iter().map(PathBuf::from).collect(),
            aggregate_usage,
            subagent_usage,
            partial,
            warnings,
        })
    }
}

impl SkillEventExtractor for ClaudeCodeObservedAgent {
    fn extract_skill_events(&self, data: &[u8], from_offset: usize) -> Result<Vec<SkillEvent>> {
        Ok(extract::extract_claude_code(tail(data, from_offset)).skill_events)
    }
}

impl TranscriptTruncator for ClaudeCodeObservedAgent {
    /// Drop every JSONL line whose `timestamp` field is strictly greater
    /// than the checkpoint's RFC-3339 boundary. The `checkpoint_id`
    /// argument carries the boundary as a serialised RFC-3339 timestamp
    /// (e.g. `"2026-05-05T12:34:56Z"`) — callers in
    /// `libra agent checkpoint rewind --apply` resolve
    /// `agent_checkpoint.created_at` (Unix seconds) to that string before
    /// invoking us, keeping the trait surface free of timestamp types.
    fn truncate_transcript(&self, transcript_data: &[u8], checkpoint_id: &str) -> Result<Vec<u8>> {
        let boundary: DateTime<Utc> = checkpoint_id.parse().with_context(|| {
            format!(
                "checkpoint boundary '{checkpoint_id}' must be an RFC-3339 timestamp \
                 (caller is responsible for resolving agent_checkpoint.created_at)"
            )
        })?;
        truncate_jsonl_after(transcript_data, boundary)
    }
}

/// Walk `transcript_data` line-by-line (LF separated), keeping every
/// line whose parsed `timestamp` field is `<= boundary` (or that lacks a
/// parseable timestamp altogether). Non-JSONL bytes — e.g. trailing
/// partial writes — are kept verbatim so the truncator never silently
/// erases a record we don't understand.
fn truncate_jsonl_after(transcript_data: &[u8], boundary: DateTime<Utc>) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(transcript_data.len());
    for line in transcript_data.split_inclusive(|&b| b == b'\n') {
        let trimmed = trim_trailing_newline(line);
        if trimmed.is_empty() {
            // Preserve empty separator lines verbatim — Claude Code
            // doesn't emit them but a hand-edited file might.
            out.extend_from_slice(line);
            continue;
        }
        match serde_json::from_slice::<serde_json::Value>(trimmed) {
            Ok(value) => {
                let ts = value
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<DateTime<Utc>>().ok());
                match ts {
                    Some(parsed) if parsed > boundary => {
                        // Strictly after the checkpoint — drop.
                    }
                    _ => out.extend_from_slice(line),
                }
            }
            Err(_) => {
                // Non-JSON line (e.g. partial write at tail). Keep it
                // — physical truncation is the user's job once they've
                // inspected what survived.
                out.extend_from_slice(line);
            }
        }
    }
    Ok(out)
}

fn trim_trailing_newline(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && (line[end - 1] == b'\n' || line[end - 1] == b'\r') {
        end -= 1;
    }
    &line[..end]
}

/// Convert the Unix-second `agent_checkpoint.created_at` into the
/// RFC-3339 boundary string the truncator expects. Public so the
/// `libra agent checkpoint rewind --apply` path can resolve a boundary
/// without re-importing `chrono` glue at the call site.
///
/// Codex round-2 follow-up: a corrupt or out-of-range `created_at`
/// (e.g. negative or beyond the chrono representable range) used to
/// silently fall back to Unix epoch, which would set the truncation
/// boundary to 1970-01-01 and effectively erase the entire transcript
/// the next time `rewind --apply` ran. Now returns `Err` so the
/// caller (`truncate_agent_transcript_for_checkpoint_with_conn`)
/// surfaces a `Failed { reason: ... }` outcome and leaves the
/// transcript alone.
pub fn rfc3339_boundary_for_unix_seconds(created_at: i64) -> Result<String> {
    let parsed = DateTime::<Utc>::from_timestamp(created_at, 0).ok_or_else(|| {
        anyhow!(
            "agent_checkpoint.created_at {created_at} is outside the chrono \
             representable range; cannot derive an RFC-3339 boundary"
        )
    })?;
    Ok(parsed.to_rfc3339())
}

/// Persist `truncated_bytes` back to the original transcript path,
/// preserving file permissions where possible. Writes through a
/// temporary sibling and renames atomically so a partial write cannot
/// corrupt the live transcript that Claude Code may be appending to
/// concurrently.
///
/// Codex round-1 follow-up: closes the read→rename TOCTOU window. The
/// caller passes `expected_size_at_read` — the file size observed when
/// the bytes were first loaded. We re-stat the original immediately
/// before the rename and refuse to swap if the size has changed,
/// signalling to the caller that Claude Code (or another writer)
/// appended after our read. The rewind path then surfaces this as a
/// `Failed` outcome rather than silently dropping the appended bytes.
/// Pass `None` to skip the check (used by tests that don't model a
/// concurrent writer).
pub fn write_truncated_transcript(
    path: &PathBuf,
    truncated_bytes: &[u8],
    expected_size_at_read: Option<u64>,
) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("transcript path has no parent: {}", path.display()))?;
    fs::create_dir_all(dir)
        .with_context(|| format!("create transcript parent dir {}", dir.display()))?;
    let tmp_path = dir.join(format!(
        ".libra-truncate-{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("transcript")
    ));
    // Codex round-3 + round-4 follow-up: open the tmp file with
    // restricted Unix permissions BEFORE writing any bytes, AND use
    // `create_new(true)` so a pre-existing file at that path never
    // gets reused with whatever permissions it had. A reused
    // permissive tmp file (left over from a prior crashed run, or
    // planted by an attacker who can write into the parent dir) would
    // expose the transcript contents during the brief window between
    // open and chmod.
    //
    // If a leftover tmp file blocks creation, remove it once and
    // retry — this is the same idempotency the rewind path expects.
    let mut open_opts = fs::OpenOptions::new();
    open_opts.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open_opts.mode(0o600);
    }
    let open_result = match open_opts.open(&tmp_path) {
        Ok(f) => Ok(f),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            // Stale tmp file from a previously-crashed truncate.
            // Remove and retry; we still create_new on the retry so
            // an attacker racing to plant a new file in the gap will
            // simply lose to our atomic create.
            let _ = fs::remove_file(&tmp_path);
            open_opts.open(&tmp_path)
        }
        Err(err) => Err(err),
    };
    {
        let mut file =
            open_result.with_context(|| format!("create truncate temp {}", tmp_path.display()))?;
        file.write_all(truncated_bytes)
            .with_context(|| format!("write truncated bytes to {}", tmp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("fsync truncate temp {}", tmp_path.display()))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Promote to the original transcript's mode if it was more
        // permissive (group-readable, etc.). Errors propagate — silent
        // swallowing here would leave the tmp file at 0600 and confuse
        // operators who explicitly chmod'd the transcript.
        if let Ok(meta) = fs::metadata(path) {
            let mode = meta.permissions().mode();
            // mode bits are platform-dependent; mask to the file-mode
            // bits to avoid copying inheritance bits.
            let file_mode = mode & 0o7777;
            if file_mode != 0o600 {
                fs::set_permissions(&tmp_path, fs::Permissions::from_mode(file_mode))
                    .with_context(|| {
                        format!(
                            "preserve original transcript mode 0o{file_mode:o} on {}",
                            tmp_path.display()
                        )
                    })?;
            }
        }
    }
    // Concurrent-writer guard: re-stat the original transcript *just*
    // before the atomic rename. If the file grew (or vanished), abort
    // and clean up the tmp file rather than overwrite — a silent
    // overwrite would lose the bytes the agent appended after our
    // read.
    if let Some(expected) = expected_size_at_read {
        match fs::metadata(path) {
            Ok(meta) if meta.len() != expected => {
                let actual = meta.len();
                let _ = fs::remove_file(&tmp_path);
                return Err(anyhow!(
                    "transcript {} grew from {} to {} bytes during truncation \
                     (concurrent writer). Aborted to avoid silently dropping \
                     appended bytes; rerun once the agent is idle.",
                    path.display(),
                    expected,
                    actual
                ));
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let _ = fs::remove_file(&tmp_path);
                return Err(anyhow!(
                    "transcript {} disappeared during truncation",
                    path.display()
                ));
            }
            Err(err) => {
                let _ = fs::remove_file(&tmp_path);
                return Err(anyhow!(err))
                    .with_context(|| format!("re-stat transcript {}", path.display()));
            }
        }
    }
    fs::rename(&tmp_path, path)
        .with_context(|| format!("atomic rename {} -> {}", tmp_path.display(), path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// DR-01 — transcript flush-wait gate (plan-20260713 ADR-DR-07 / GC-DR-04)
// ---------------------------------------------------------------------------

/// Default bounded flush-wait budget (GC-DR-04: ≤ 2s, leaving read/redact/
/// write headroom inside the 10s hook ceiling).
pub const FLUSH_WAIT_BUDGET: Duration = Duration::from_millis(2_000);
/// Poll cadence while waiting for the tail to settle.
const FLUSH_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// A transcript whose mtime is at least this old is already settled — skip
/// the wait entirely (the "stale mtime 跳过" rule).
const FLUSH_STALE_THRESHOLD: Duration = Duration::from_secs(3);
/// The flush probe never reads more than the same 16 MiB hard cap enforced by
/// the actual transcript reader. This prevents a hook-supplied or runaway
/// file from causing an unbounded allocation before the authorized read.
const FLUSH_TAIL_PROBE_BYTES: u64 = MAX_TRANSCRIPT_BYTES;

/// Outcome of one flush-wait pass (diagnostic; callers proceed to read
/// either way — a budget-exhausted tail simply parses `incomplete`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushOutcome {
    /// Tail settled (complete last line + quiescent mtime) or the file was
    /// already stale/absent — nothing to wait for.
    Settled,
    /// Budget ran out while the tail still looked in-flight.
    BudgetExhausted,
}

/// Does the file currently end in a complete JSONL record (trailing
/// newline + parseable last line)?
#[cfg(test)]
fn tail_is_complete_with_limit(path: &Path, max_probe_bytes: u64) -> bool {
    if max_probe_bytes == 0 {
        return false;
    }
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    tail_is_complete_reader_with_limit(&mut file, max_probe_bytes)
}

fn tail_is_complete_reader(file: &mut fs::File) -> bool {
    tail_is_complete_reader_with_limit(file, FLUSH_TAIL_PROBE_BYTES)
}

fn tail_is_complete_reader_with_limit(file: &mut fs::File, max_probe_bytes: u64) -> bool {
    let Ok(len) = file.metadata().map(|meta| meta.len()) else {
        return false;
    };
    if len == 0 {
        return true; // empty file: nothing in flight
    }
    let probe_len = len.min(max_probe_bytes);
    let Ok(offset) = i64::try_from(probe_len) else {
        return false;
    };
    if file.seek(SeekFrom::End(-offset)).is_err() {
        return false;
    }
    let Ok(capacity) = usize::try_from(probe_len) else {
        return false;
    };
    let mut bytes = Vec::with_capacity(capacity);
    if file.take(probe_len).read_to_end(&mut bytes).is_err() || bytes.len() as u64 != probe_len {
        return false;
    }
    if bytes.last() != Some(&b'\n') {
        return false;
    }
    let before_terminator = &bytes[..bytes.len() - 1];
    let line_start = match before_terminator.iter().rposition(|byte| *byte == b'\n') {
        Some(index) => index + 1,
        None if probe_len == len => 0,
        // The final record starts before the bounded probe, so we cannot
        // prove it is valid without exceeding the memory ceiling.
        None => return false,
    };
    let last_line = &before_terminator[line_start..];
    serde_json::from_slice::<serde_json::Value>(last_line).is_ok()
}

/// Path-based compatibility helper retained for focused DR-01 tests. The
/// production resolver calls [`flush_wait_file`] with its pinned descriptor.
pub fn flush_wait(path: &Path, budget: Duration, poll: Duration) -> FlushOutcome {
    let Ok(file) = fs::File::open(path) else {
        return FlushOutcome::Settled;
    };
    flush_wait_file(&file, budget, poll)
}

/// Bounded synchronous wait for Claude's async JSONL flush to settle
/// (plan-20260713 DR-01). Never fails the caller: the outcome is
/// diagnostic-only, and reads proceed regardless.
pub fn flush_wait_file(file: &fs::File, budget: Duration, poll: Duration) -> FlushOutcome {
    let Ok(meta) = file.metadata() else {
        return FlushOutcome::Settled; // absent: nothing to wait for
    };
    // Stale-mtime short-circuit: a transcript untouched for a while is not
    // mid-flush; waiting would only burn hook budget.
    if let Ok(modified) = meta.modified()
        && modified
            .elapsed()
            .map(|age| age >= FLUSH_STALE_THRESHOLD)
            .unwrap_or(false)
    {
        return FlushOutcome::Settled;
    }
    let deadline = Instant::now() + budget;
    let mut last_snapshot = None;
    loop {
        let Ok(current) = file.metadata() else {
            return FlushOutcome::Settled;
        };
        if current.len() > MAX_TRANSCRIPT_BYTES {
            // The authorized reader will return its actionable size error;
            // waiting cannot make an already-oversized transcript readable.
            return FlushOutcome::Settled;
        }
        let snapshot = (current.len(), current.modified().ok());
        let tail_complete = file
            .try_clone()
            .ok()
            .is_some_and(|mut probe| tail_is_complete_reader(&mut probe));
        if last_snapshot.as_ref() != Some(&snapshot) && tail_complete {
            return FlushOutcome::Settled;
        }
        last_snapshot = Some(snapshot);
        if Instant::now() >= deadline {
            return FlushOutcome::BudgetExhausted;
        }
        std::thread::sleep(poll.min(deadline.saturating_duration_since(Instant::now())));
    }
}

/// Run Claude's bounded flush probe against an already-open source held by a
/// killable capture helper. The helper, not the hook parent, owns every
/// metadata/clone/seek/read operation in this probe; it rewinds the same held
/// descriptor before its bounded reader consumes it.
pub(crate) fn prepare_held_file_for_capture(file: &fs::File) {
    let _ = flush_wait_file(file, FLUSH_WAIT_BUDGET, FLUSH_POLL_INTERVAL);
}

impl TranscriptPreparer for ClaudeCodeObservedAgent {
    /// DR-01: bounded flush-wait before the seam opens the transcript.
    /// Always `Ok` — a budget-exhausted tail is read anyway and its final
    /// turn parses `incomplete` (upgradeable later; ADR-DR-07).
    fn prepare_transcript(
        &self,
        _session: &AgentSessionCtx,
        file: &fs::File,
        deadline: Option<Instant>,
    ) -> Result<()> {
        let budget = deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(FLUSH_WAIT_BUDGET)
            .min(FLUSH_WAIT_BUDGET);
        let outcome = flush_wait_file(file, budget, FLUSH_POLL_INTERVAL);
        if outcome == FlushOutcome::BudgetExhausted {
            tracing::warn!(
                reason = "flush_wait_budget_exhausted",
                "transcript flush-wait budget exhausted; reading a possibly \
                 in-flight tail (final turn will parse incomplete)"
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DR-02 — independent session discovery (plan-20260713)
// ---------------------------------------------------------------------------

/// Claude Code's project-directory slug for a working directory: every
/// character outside `[A-Za-z0-9]` becomes `-` (best-effort pinned to the
/// current probe version — e.g. `/run/media/eli/data/gitmono/libra` →
/// `-run-media-eli-data-gitmono-libra`). Upstream changes to this rule are
/// caught by the pinned vectors in `claude_session_dir_resolve`.
pub fn claude_project_slug(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn claude_home() -> Option<PathBuf> {
    std::env::var_os("LIBRA_TEST_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
}

/// `~/.claude/projects/<slug>` for `cwd` (DR-02 `session_dir`).
pub fn claude_session_dir(cwd: &Path) -> Option<PathBuf> {
    Some(
        claude_home()?
            .join(".claude")
            .join("projects")
            .join(claude_project_slug(cwd)),
    )
}

/// Path-component contract shared by explicit historical imports and Claude
/// transcript/subagent discovery. Older Claude releases emitted identifiers
/// containing dots or underscores, so the resolver must accept those safe
/// legacy forms as well as today's UUID-like ids.
pub fn claude_session_id_is_safe_path_component(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 128
        && !matches!(session_id, "." | "..")
        && session_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
}

/// Derive Claude's expected session filename without touching the filesystem.
/// Deadline-bound live capture must hand this lexical candidate to its
/// killable helper, which performs the provider-root no-follow open itself;
/// calling `resolve_session_file` here would add a parent-side directory scan
/// outside the capture deadline boundary.
pub fn claude_session_file_candidate(cwd: &Path, session_id: &str) -> Result<Option<PathBuf>> {
    if !claude_session_id_is_safe_path_component(session_id) {
        return Err(anyhow!(
            "invalid Claude session id (expected alphanumeric/dot/dash/underscore, at most 128 characters)"
        ));
    }
    Ok(claude_session_dir(cwd).map(|dir| dir.join(format!("{session_id}.jsonl"))))
}

/// Locate the on-disk session JSONL for `(cwd, session_id)` without a hook
/// pointer (DR-02 `resolve_session_file`). Fail-closed: an invalid id, a
/// symlink, or a path escaping the projects root is an error; an absent
/// file is `Ok(None)`.
pub fn resolve_session_file(cwd: &Path, session_id: &str) -> Result<Option<PathBuf>> {
    let Some(candidate) = claude_session_file_candidate(cwd, session_id)? else {
        return Ok(None);
    };
    let Some(dir) = candidate.parent() else {
        return Ok(None);
    };
    let adapter = ClaudeCodeObservedAgent::new();
    let Some(directory) =
        super::super::transcript_source::open_provider_directory_for_discovery(&adapter, dir)?
    else {
        return Ok(None);
    };
    let name = candidate
        .file_name()
        .ok_or_else(|| anyhow!("derived Claude session candidate lacks a filename"))?;
    match super::super::transcript_source::open_file_beneath_pinned_provider_directory(
        &directory,
        Path::new(name),
    ) {
        Ok(_) => Ok(Some(candidate)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error).context("open pinned Claude session file (no-follow)"),
    }
}

#[cfg(test)]
mod tests {

    // -- DR-02: claude_session_dir_resolve ---------------------------------

    struct HomeGuard {
        prior: Option<std::ffi::OsString>,
    }
    impl HomeGuard {
        fn set(path: &Path) -> Self {
            let prior = std::env::var_os("LIBRA_TEST_HOME");
            // SAFETY: test-only env mutation, restored on drop; #[serial(env)].
            unsafe { std::env::set_var("LIBRA_TEST_HOME", path) };
            Self { prior }
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prior {
                    Some(v) => std::env::set_var("LIBRA_TEST_HOME", v),
                    None => std::env::remove_var("LIBRA_TEST_HOME"),
                }
            }
        }
    }

    /// DR-02 pinned slug vectors + session-file resolution semantics.
    #[test]
    #[serial_test::serial(env)]
    fn claude_session_dir_resolve() {
        // Pinned slug vectors (probe: Claude Code 2.1.207 layout).
        assert_eq!(
            claude_project_slug(Path::new("/run/media/eli/data/gitmono/libra")),
            "-run-media-eli-data-gitmono-libra"
        );
        assert_eq!(
            claude_project_slug(Path::new("/home/user/my.project_x")),
            "-home-user-my-project-x"
        );
        assert_eq!(claude_project_slug(Path::new("/")), "-");
        assert_eq!(claude_project_slug(Path::new("/a b/中文")), "-a-b---"); // non-ASCII → '-'
        assert_eq!(claude_project_slug(Path::new("/p/🚀x")), "-p--x"); // non-BMP → '-' (per char)

        let home = tempfile::tempdir().unwrap();
        let _g = HomeGuard::set(home.path());
        let cwd = Path::new("/work/proj");
        let dir = claude_session_dir(cwd).expect("home resolves");
        assert!(dir.ends_with(".claude/projects/-work-proj"));

        // Absent file → Ok(None).
        assert!(
            resolve_session_file(cwd, "0a12b043-5f5d-40d2-8f46-47b1f4370564")
                .unwrap()
                .is_none()
        );

        // Present file → Ok(Some(path)).
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("0a12b043-5f5d-40d2-8f46-47b1f4370564.jsonl");
        std::fs::write(&file, "{}\n").unwrap();
        let found = resolve_session_file(cwd, "0a12b043-5f5d-40d2-8f46-47b1f4370564")
            .unwrap()
            .expect("file found");
        assert_eq!(found, file);

        // Invalid ids fail closed (traversal attempts included).
        assert!(resolve_session_file(cwd, "").is_err());
        assert!(resolve_session_file(cwd, "../escape").is_err());
        assert!(resolve_session_file(cwd, "id with spaces").is_err());

        // Symlinked session file fails closed.
        #[cfg(unix)]
        {
            let target = home.path().join("outside.jsonl");
            std::fs::write(&target, "{}\n").unwrap();
            let link = dir.join("abcdef00-0000-0000-0000-000000000001.jsonl");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(
                resolve_session_file(cwd, "abcdef00-0000-0000-0000-000000000001").is_err(),
                "symlink must be rejected"
            );

            // A symlinked project directory is rejected before even probing
            // the selected filename outside the provider root.
            std::fs::remove_file(&link).unwrap();
            std::fs::remove_file(&file).unwrap();
            std::fs::remove_dir(&dir).unwrap();
            let outside_dir = home.path().join("outside-project");
            std::fs::create_dir(&outside_dir).unwrap();
            std::fs::write(
                outside_dir.join("abcdef00-0000-0000-0000-000000000002.jsonl"),
                "{}\n",
            )
            .unwrap();
            std::os::unix::fs::symlink(&outside_dir, &dir).unwrap();
            assert!(
                resolve_session_file(cwd, "abcdef00-0000-0000-0000-000000000002").is_err(),
                "symlinked project directory must be rejected"
            );
        }
    }

    // -- DR-01: transcript_flush_gate --------------------------------------

    /// DR-01 flush-wait states: absent file, settled tail, stale mtime,
    /// budget exhaustion on an in-flight tail, and mid-wait completion.
    /// No real Claude binary involved (GC-DR-07).
    #[test]
    fn transcript_flush_gate() {
        let dir = tempfile::tempdir().unwrap();
        let budget = Duration::from_millis(300);
        let poll = Duration::from_millis(20);

        // Absent file: settled immediately.
        assert_eq!(
            flush_wait(&dir.path().join("nope.jsonl"), budget, poll),
            FlushOutcome::Settled
        );

        // Empty file: nothing in flight — settled immediately.
        let empty = dir.path().join("empty.jsonl");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(flush_wait(&empty, budget, poll), FlushOutcome::Settled);

        // Complete tail: settled immediately.
        let complete = dir.path().join("complete.jsonl");
        std::fs::write(&complete, "{\"type\":\"user\"}\n").unwrap();
        assert_eq!(flush_wait(&complete, budget, poll), FlushOutcome::Settled);

        // A complete record larger than the configured probe is not loaded
        // in full or guessed complete; the production probe uses the reader's
        // 16 MiB hard cap.
        let long_line = dir.path().join("long-line.jsonl");
        std::fs::write(
            &long_line,
            format!("{{\"text\":\"{}\"}}\n", "x".repeat(128)),
        )
        .unwrap();
        assert!(!tail_is_complete_with_limit(&long_line, 64));

        // In-flight tail (no trailing newline): budget exhausts, bounded.
        let inflight = dir.path().join("inflight.jsonl");
        std::fs::write(&inflight, "{\"type\":\"assistant\",\"partial").unwrap();
        let started = Instant::now();
        assert_eq!(
            flush_wait(&inflight, budget, poll),
            FlushOutcome::BudgetExhausted
        );
        assert!(
            started.elapsed() < budget + Duration::from_millis(500),
            "wait must stay bounded"
        );

        // Tail completing mid-wait: settles before the budget.
        let settling = dir.path().join("settling.jsonl");
        std::fs::write(&settling, "{\"type\":\"assistant\",\"partial").unwrap();
        let writer = {
            let settling = settling.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(60));
                let mut f = fs::OpenOptions::new().append(true).open(&settling).unwrap();
                f.write_all(b"\":true}\n").unwrap();
            })
        };
        assert_eq!(
            flush_wait(&settling, Duration::from_millis(2_000), poll),
            FlushOutcome::Settled
        );
        writer.join().unwrap();

        // Stale mtime short-circuits even with an incomplete tail.
        let stale = dir.path().join("stale.jsonl");
        std::fs::write(&stale, "{\"type\":\"assistant\",\"partial").unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(30);
        let file = fs::OpenOptions::new().write(true).open(&stale).unwrap();
        file.set_modified(old).unwrap();
        drop(file);
        let started = Instant::now();
        assert_eq!(flush_wait(&stale, budget, poll), FlushOutcome::Settled);
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "stale file must not wait"
        );
    }

    #[test]
    fn transcript_preparer_respects_expired_import_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inflight.jsonl");
        std::fs::write(&path, "{\"type\":\"assistant\",\"partial\"").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let session = AgentSessionCtx {
            session_id: "claude_code__deadline".to_string(),
            provider_session_id: "deadline".to_string(),
            working_dir: dir.path().to_path_buf(),
            transcript_path: Some(path),
        };
        let started = Instant::now();
        ClaudeCodeObservedAgent::new()
            .prepare_transcript(&session, &file, Some(Instant::now()))
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "an expired historical-import deadline must skip the 2s flush wait"
        );
    }

    use super::*;

    #[test]
    fn truncator_drops_lines_after_checkpoint_boundary() {
        let transcript = b"\
{\"timestamp\":\"2026-05-05T10:00:00Z\",\"type\":\"user\",\"text\":\"a\"}\n\
{\"timestamp\":\"2026-05-05T10:30:00Z\",\"type\":\"assistant\",\"text\":\"b\"}\n\
{\"timestamp\":\"2026-05-05T11:00:00Z\",\"type\":\"user\",\"text\":\"c\"}\n\
{\"timestamp\":\"2026-05-05T11:30:00Z\",\"type\":\"assistant\",\"text\":\"d\"}\n";
        let agent = ClaudeCodeObservedAgent::new();
        let truncated = agent
            .truncate_transcript(transcript, "2026-05-05T10:30:00Z")
            .unwrap();
        let s = String::from_utf8(truncated).unwrap();
        assert!(s.contains("\"a\""), "first kept");
        assert!(s.contains("\"b\""), "boundary-equal kept");
        assert!(!s.contains("\"c\""), "post-boundary dropped");
        assert!(!s.contains("\"d\""), "post-boundary dropped");
    }

    #[test]
    fn truncator_keeps_lines_with_unparseable_timestamps() {
        // Session-meta line at the very top of a Claude transcript has
        // no `timestamp` field. We must not drop it — it carries
        // session config that downstream tooling needs.
        let transcript = b"\
{\"type\":\"session_meta\",\"version\":\"1.0\"}\n\
{\"timestamp\":\"2026-05-05T11:00:00Z\",\"type\":\"user\"}\n";
        let agent = ClaudeCodeObservedAgent::new();
        let truncated = agent
            .truncate_transcript(transcript, "2026-05-05T10:00:00Z")
            .unwrap();
        let s = String::from_utf8(truncated).unwrap();
        assert!(s.contains("session_meta"), "untimestamped line preserved");
        assert!(!s.contains("2026-05-05T11:00:00Z"), "post-boundary dropped");
    }

    #[test]
    fn truncator_rejects_non_rfc3339_boundary() {
        let agent = ClaudeCodeObservedAgent::new();
        let err = agent
            .truncate_transcript(b"", "not-a-timestamp")
            .unwrap_err();
        assert!(
            err.to_string().contains("RFC-3339 timestamp"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn read_transcript_returns_none_when_path_missing() {
        let agent = ClaudeCodeObservedAgent::new();
        let ctx = AgentSessionCtx {
            session_id: "s".to_string(),
            provider_session_id: "p".to_string(),
            working_dir: PathBuf::from("/tmp"),
            transcript_path: Some(PathBuf::from("/no/such/file.jsonl")),
        };
        let bytes = agent.read_transcript(&ctx).unwrap();
        assert!(bytes.is_none());
    }

    #[test]
    fn read_transcript_returns_none_when_path_unset() {
        let agent = ClaudeCodeObservedAgent::new();
        let ctx = AgentSessionCtx {
            session_id: "s".to_string(),
            provider_session_id: "p".to_string(),
            working_dir: PathBuf::from("/tmp"),
            transcript_path: None,
        };
        let bytes = agent.read_transcript(&ctx).unwrap();
        assert!(bytes.is_none());
    }

    #[test]
    fn read_transcript_returns_bytes_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, b"{\"x\":1}\n").unwrap();
        let agent = ClaudeCodeObservedAgent::new();
        let ctx = AgentSessionCtx {
            session_id: "s".to_string(),
            provider_session_id: "p".to_string(),
            working_dir: dir.path().to_path_buf(),
            transcript_path: Some(path),
        };
        let bytes = agent.read_transcript(&ctx).unwrap();
        assert_eq!(bytes.unwrap(), b"{\"x\":1}\n");
    }

    #[test]
    fn write_truncated_transcript_atomic_rename() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, b"old contents\n").unwrap();
        // Pass the original size (matches what the caller would have
        // observed during read) — guard should not trip on a stable
        // file.
        let expected = fs::metadata(&path).unwrap().len();
        write_truncated_transcript(&path, b"new\n", Some(expected)).unwrap();
        let read = fs::read(&path).unwrap();
        assert_eq!(read, b"new\n");
    }

    /// Codex round-1 follow-up: when the original transcript grew
    /// between read and rename (a concurrent Claude Code writer
    /// appending to the file), we must NOT overwrite. The function
    /// returns an error and leaves the file as the writer left it.
    #[test]
    fn write_truncated_transcript_aborts_on_concurrent_growth() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, b"old contents\n").unwrap();
        let expected = fs::metadata(&path).unwrap().len();
        // Simulate a concurrent appender by extending the file before
        // the rename guard fires.
        fs::write(&path, b"old contents\nappended after read\n").unwrap();
        let err = write_truncated_transcript(&path, b"new\n", Some(expected)).unwrap_err();
        assert!(
            err.to_string().contains("concurrent writer"),
            "expected concurrent-writer error, got: {err}"
        );
        // File preserves the appended bytes — we did NOT overwrite.
        let preserved = fs::read(&path).unwrap();
        assert_eq!(preserved, b"old contents\nappended after read\n");
        // Tmp file was cleaned up.
        let leftover = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with(".libra-truncate-"))
            .count();
        assert_eq!(leftover, 0, "tmp file removed on abort");
    }

    #[test]
    fn rfc3339_boundary_round_trips_unix_seconds() {
        let boundary = rfc3339_boundary_for_unix_seconds(1_778_020_200).unwrap();
        let parsed: DateTime<Utc> = boundary.parse().unwrap();
        assert_eq!(parsed.timestamp(), 1_778_020_200);
    }

    /// Codex round-2 follow-up: out-of-range `created_at` must surface
    /// as `Err` rather than silently falling back to Unix epoch (which
    /// would erase the entire transcript at the next `rewind --apply`).
    #[test]
    fn rfc3339_boundary_rejects_out_of_range_unix_seconds() {
        // i64::MIN is well outside chrono's representable range.
        let err = rfc3339_boundary_for_unix_seconds(i64::MIN).unwrap_err();
        assert!(
            err.to_string()
                .contains("outside the chrono representable range"),
            "unexpected: {err}"
        );
    }
}
