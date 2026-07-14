//! Phase 4.4 (entire.md §14.4 item 4): promote the five v1-preview
//! adapters (Cursor, Codex, OpenCode, GitHub Copilot CLI, Factory AI
//! Droid) from `AgentStability::Preview` to `AgentStability::Stable`.
//!
//! Each adapter ships a real `read_transcript` that loads bytes from
//! `AgentSessionCtx.transcript_path` (when the hook envelope captured
//! one), capped at the same 16 MB ceiling used by
//! [`super::claude_code::ClaudeCodeObservedAgent`]. Per-agent
//! transcript-format knowledge (line schema, message-uuid pairing,
//! tool_use semantics) is not yet implemented — that's why none of
//! these adapters carry the `TranscriptTruncator` capability. A v2
//! follow-up will add per-agent truncation. The adapter is still
//! useful in the meantime: hook ingestion + restore + `agent session
//! show --extract-transcript` (forthcoming) all rely on
//! `read_transcript`, which is now real.
//!
//! All five share the same shape, so they go through one
//! [`StablePromotedSpec`] table rather than five hand-written
//! near-duplicates.

use std::{
    fs, io,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};

use super::super::adapter::{AgentKind, AgentSessionCtx, AgentStability, ObservedAgent};

const MAX_TRANSCRIPT_BYTES: u64 = 16 * 1024 * 1024;

/// Static description of a Phase 4.4 stable-promoted adapter. Stays
/// `Copy + 'static` so the registry can hand out cheap references.
#[derive(Clone, Copy)]
pub struct StablePromotedSpec {
    pub kind: AgentKind,
    pub provider_name: &'static str,
    pub protected_dirs: &'static [&'static str],
    /// AG-19: hook provider exposed via `ObservedAgent::as_hooks()`.
    /// `None` for agents without an installable `HookProvider`
    /// (`declared_capabilities().hooks` derives from this).
    pub hooks: Option<&'static dyn crate::internal::ai::hooks::provider::HookProvider>,
}

impl std::fmt::Debug for StablePromotedSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StablePromotedSpec")
            .field("kind", &self.kind)
            .field("provider_name", &self.provider_name)
            .field("protected_dirs", &self.protected_dirs)
            .field("hooks", &self.hooks.map(|h| h.provider_name()))
            .finish()
    }
}

/// Concrete `ObservedAgent` over a [`StablePromotedSpec`]. Reports
/// `AgentStability::Stable` and reads transcript bytes from the
/// session ctx's `transcript_path` slot.
#[derive(Debug, Clone, Copy)]
pub struct StablePromotedAgent(pub &'static StablePromotedSpec);

impl ObservedAgent for StablePromotedAgent {
    fn provider_kind(&self) -> AgentKind {
        self.0.kind
    }
    fn provider_name(&self) -> &'static str {
        self.0.provider_name
    }
    fn stability(&self) -> AgentStability {
        AgentStability::Stable
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
        self.0.protected_dirs
    }
    fn as_hooks(&self) -> Option<&dyn crate::internal::ai::hooks::provider::HookProvider> {
        self.0.hooks
    }

    // AG-21: best-effort transcript intelligence for the first-batch
    // promoted agents only (codex/opencode rollout/export formats); the
    // non-first-batch kinds keep the default `None` so their registry
    // rows stay all-false (E9).
    fn as_prompt_extractor(
        &self,
    ) -> Option<&dyn crate::internal::ai::observed_agents::capability::PromptExtractor> {
        matches!(self.0.kind, AgentKind::Codex | AgentKind::OpenCode).then_some(self)
    }
    fn as_token_calculator(
        &self,
    ) -> Option<&dyn crate::internal::ai::observed_agents::capability::TokenCalculator> {
        matches!(self.0.kind, AgentKind::Codex | AgentKind::OpenCode).then_some(self)
    }
    fn as_model_extractor(
        &self,
    ) -> Option<&dyn crate::internal::ai::observed_agents::capability::ModelExtractor> {
        matches!(self.0.kind, AgentKind::Codex | AgentKind::OpenCode).then_some(self)
    }
    fn as_skill_event_extractor(
        &self,
    ) -> Option<&dyn crate::internal::ai::observed_agents::capability::SkillEventExtractor> {
        matches!(self.0.kind, AgentKind::Codex | AgentKind::OpenCode).then_some(self)
    }
}

fn promoted_extract(
    kind: AgentKind,
    data: &[u8],
) -> crate::internal::ai::observed_agents::extract::ExtractionSummary {
    use crate::internal::ai::observed_agents::extract;
    match kind {
        AgentKind::OpenCode => extract::extract_opencode(data),
        // Codex is the only other kind wired through the accessors above.
        _ => extract::extract_codex(data),
    }
}

fn promoted_tail(data: &[u8], from_offset: usize) -> &[u8] {
    &data[from_offset.min(data.len())..]
}

impl crate::internal::ai::observed_agents::capability::PromptExtractor for StablePromotedAgent {
    fn extract_prompts(&self, data: &[u8], from_offset: usize) -> Result<Vec<String>> {
        Ok(promoted_extract(self.0.kind, promoted_tail(data, from_offset)).prompts)
    }
}

impl crate::internal::ai::observed_agents::capability::TokenCalculator for StablePromotedAgent {
    fn calculate_token_usage(
        &self,
        data: &[u8],
        from_offset: usize,
    ) -> Result<crate::internal::ai::completion::CompletionUsageSummary> {
        Ok(
            promoted_extract(self.0.kind, promoted_tail(data, from_offset))
                .usage
                .unwrap_or_default(),
        )
    }
}

impl crate::internal::ai::observed_agents::capability::ModelExtractor for StablePromotedAgent {
    fn extract_model(&self, data: &[u8]) -> Result<Option<String>> {
        Ok(promoted_extract(self.0.kind, data).model)
    }
}

impl crate::internal::ai::observed_agents::capability::SkillEventExtractor for StablePromotedAgent {
    fn extract_skill_events(
        &self,
        data: &[u8],
        from_offset: usize,
    ) -> Result<Vec<crate::internal::ai::observed_agents::capability::SkillEvent>> {
        Ok(promoted_extract(self.0.kind, promoted_tail(data, from_offset)).skill_events)
    }
}

pub static CURSOR_STABLE_PROMOTED_SPEC: StablePromotedSpec = StablePromotedSpec {
    kind: AgentKind::Cursor,
    provider_name: "cursor",
    protected_dirs: &[".cursor"],
    hooks: None,
};

pub static CODEX_STABLE_PROMOTED_SPEC: StablePromotedSpec = StablePromotedSpec {
    kind: AgentKind::Codex,
    provider_name: "codex",
    protected_dirs: &[".codex"],
    // AG-19: Codex HookProvider (user-level hooks.json + [hooks.state]
    // trust entries; see providers/codex).
    hooks: Some(&crate::internal::ai::hooks::providers::codex::CODEX_PROVIDER),
};

pub static OPENCODE_STABLE_PROMOTED_SPEC: StablePromotedSpec = StablePromotedSpec {
    kind: AgentKind::OpenCode,
    provider_name: "opencode",
    protected_dirs: &[".opencode"],
    // AG-19: OpenCode HookProvider (Libra-managed .opencode/plugin file;
    // see providers/opencode).
    hooks: Some(&crate::internal::ai::hooks::providers::opencode::OPENCODE_PROVIDER),
};

pub static COPILOT_STABLE_PROMOTED_SPEC: StablePromotedSpec = StablePromotedSpec {
    kind: AgentKind::Copilot,
    provider_name: "copilot",
    protected_dirs: &[".copilot"],
    hooks: None,
};

pub static FACTORY_AI_STABLE_PROMOTED_SPEC: StablePromotedSpec = StablePromotedSpec {
    kind: AgentKind::FactoryAi,
    provider_name: "factory_ai",
    protected_dirs: &[".factory"],
    hooks: None,
};

/// Phase 4.4 stable-promoted adapter table. Mirrors the v1 adapter
/// matrix (entire.md §5.2) for the five agents that previously
/// returned `AgentNotYetImplemented`. The `protected_dirs` mirror each
/// agent's well-known config directory so `clean` / `rewind --apply`
/// won't trample them.
pub static STABLE_PROMOTED_SPECS: &[&StablePromotedSpec] = &[
    &CURSOR_STABLE_PROMOTED_SPEC,
    &CODEX_STABLE_PROMOTED_SPEC,
    &OPENCODE_STABLE_PROMOTED_SPEC,
    &COPILOT_STABLE_PROMOTED_SPEC,
    &FACTORY_AI_STABLE_PROMOTED_SPEC,
];

/// Lookup a stable-promoted spec by `AgentKind`. Returns `None` for
/// kinds that aren't in the Phase 4.4 promotion set (the two original
/// stable adapters — Claude Code, Gemini — have their own dedicated
/// types with extra capabilities like `TranscriptTruncator`).
pub fn stable_promoted_spec_for(kind: AgentKind) -> Option<&'static StablePromotedSpec> {
    STABLE_PROMOTED_SPECS
        .iter()
        .copied()
        .find(|spec| spec.kind == kind)
}

// ---------------------------------------------------------------------------
// DR-03 — Codex rollout discovery (plan-20260713; GC-DR-08 bounded traversal)
// ---------------------------------------------------------------------------

/// Directory-entry cap per level and total-file cap for the rollout walk —
/// hard bounds so a hostile/bloated `$CODEX_HOME` cannot stall a hook or
/// import (GC-DR-08).
const ROLLOUT_MAX_ENTRIES_PER_DIR: usize = 4_096;
const ROLLOUT_MAX_ENTRIES_SCANNED: usize = 20_000;
const ROLLOUT_MAX_FILES_SCANNED: usize = 10_000;
const ROLLOUT_SCAN_TIMEOUT: Duration = Duration::from_secs(2);

struct RolloutScanBudget {
    entries_scanned: usize,
    max_entries: usize,
    deadline: Instant,
}

impl RolloutScanBudget {
    fn new(max_entries: usize, timeout: Duration) -> Self {
        let now = Instant::now();
        Self {
            entries_scanned: 0,
            max_entries,
            deadline: now.checked_add(timeout).unwrap_or(now),
        }
    }

    fn check_time(&self, dir: &std::path::Path) -> Result<()> {
        if Instant::now() >= self.deadline {
            return Err(anyhow!(
                "rollout discovery exceeded its time budget while scanning {}; refusing a stale answer (GC-DR-08 bounded discovery)",
                dir.display()
            ));
        }
        Ok(())
    }

    fn charge_entry(&mut self, dir: &std::path::Path) -> Result<()> {
        self.check_time(dir)?;
        self.entries_scanned = self
            .entries_scanned
            .checked_add(1)
            .ok_or_else(|| anyhow!("rollout discovery entry counter overflow"))?;
        if self.entries_scanned > self.max_entries {
            return Err(anyhow!(
                "rollout discovery scanned more than {} total entries; refusing a stale answer (GC-DR-08 bounded discovery)",
                self.max_entries
            ));
        }
        Ok(())
    }
}

fn codex_sessions_root() -> Option<std::path::PathBuf> {
    // $CODEX_HOME (absolute) wins WITHOUT requiring a resolvable home dir;
    // only the fallback needs one.
    if let Some(path) = std::env::var_os("CODEX_HOME").map(std::path::PathBuf::from)
        && path.is_absolute()
    {
        return Some(path.join("sessions"));
    }
    let home = std::env::var_os("LIBRA_TEST_HOME")
        .map(std::path::PathBuf::from)
        .or_else(dirs::home_dir)?;
    Some(home.join(".codex").join("sessions"))
}

/// List a directory's entry names filtered by `keep`, sorted DESCENDING
/// (newest date partition first). Bounded and LOUD: scanning more than
/// [`ROLLOUT_MAX_ENTRIES_PER_DIR`] entries is an explicit error (a silent
/// prefix could skip a newer partition and return a stale match); a missing
/// directory lists empty; any other I/O failure propagates so callers can
/// diagnose it instead of reading "not found".
fn sorted_desc_entries(
    dir: &std::path::Path,
    keep: impl Fn(&str) -> bool,
    budget: &mut RolloutScanBudget,
) -> Result<Vec<std::ffi::OsString>> {
    budget.check_time(dir)?;
    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(err).with_context(|| format!("read rollout directory {}", dir.display()));
        }
    };
    let mut names: Vec<std::ffi::OsString> = Vec::new();
    for (scanned, entry) in read.enumerate() {
        if scanned >= ROLLOUT_MAX_ENTRIES_PER_DIR {
            return Err(anyhow!(
                "rollout directory {} exceeds the {} entry scan bound; refusing a possibly \
                 stale answer (GC-DR-08 bounded discovery)",
                dir.display(),
                ROLLOUT_MAX_ENTRIES_PER_DIR
            ));
        }
        budget.charge_entry(dir)?;
        let entry =
            entry.with_context(|| format!("read rollout directory entry in {}", dir.display()))?;
        let name = entry.file_name();
        if keep(&name.to_string_lossy()) {
            names.push(name);
        }
    }
    names.sort_unstable_by(|a, b| b.cmp(a));
    Ok(names)
}

fn fixed_ascii_u32(name: &str, len: usize) -> Option<u32> {
    (name.len() == len && name.bytes().all(|b| b.is_ascii_digit()))
        .then(|| name.parse().ok())
        .flatten()
}

fn valid_year(name: &str) -> Option<i32> {
    fixed_ascii_u32(name, 4)
        .filter(|year| (1..=9_999).contains(year))
        .and_then(|year| i32::try_from(year).ok())
}

fn valid_month(name: &str) -> Option<u32> {
    fixed_ascii_u32(name, 2).filter(|month| (1..=12).contains(month))
}

fn real_directory(path: &std::path::Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(anyhow!(
            "refusing symlinked Codex rollout directory {} (fail-closed)",
            path.display()
        )),
        Ok(meta) => Ok(meta.is_dir()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => {
            Err(err).with_context(|| format!("stat Codex rollout directory {}", path.display()))
        }
    }
}

/// Locate the newest Codex rollout JSONL for `session_id` under
/// `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*-<session_id>.jsonl`
/// (plan-20260713 DR-03, mirroring Entire's `findRolloutBySessionID`).
///
/// Bounded (GC-DR-08): fixed depth 3, digit-validated date components (a
/// lexically-high junk directory can never win), per-dir scan bound and a
/// total-file cap that FAIL rather than silently mis-answer, newest date
/// partitions first, first match wins. Fail-closed on an invalid id or a
/// symlinked match; `Ok(None)` only when nothing matches.
pub fn find_codex_rollout(session_id: &str) -> Result<Option<std::path::PathBuf>> {
    find_codex_rollout_bounded(session_id, RolloutDiscoveryLimits::default())
}

/// Injectable discovery bounds (GC-DR-07) so tests drive the FULL finder
/// against every cap deterministically; production uses [`Default`].
#[derive(Debug, Clone, Copy)]
pub struct RolloutDiscoveryLimits {
    /// Total entries (directories + files) the walk may charge.
    pub max_entries_scanned: usize,
    /// Monotonic wall-clock deadline for one discovery pass.
    pub deadline: Duration,
}

impl Default for RolloutDiscoveryLimits {
    fn default() -> Self {
        Self {
            max_entries_scanned: ROLLOUT_MAX_ENTRIES_SCANNED,
            deadline: ROLLOUT_SCAN_TIMEOUT,
        }
    }
}

/// [`find_codex_rollout`] with injectable bounds. Every bound fails LOUDLY —
/// a truncated walk must never silently claim "newest" or "not found".
pub fn find_codex_rollout_bounded(
    session_id: &str,
    limits: RolloutDiscoveryLimits,
) -> Result<Option<std::path::PathBuf>> {
    let valid = !session_id.is_empty()
        && session_id.len() <= 64
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        return Err(anyhow!(
            "invalid Codex session id (expected alnum/dash/underscore, ≤64 chars)"
        ));
    }
    let Some(root) = codex_sessions_root() else {
        return Ok(None);
    };
    let mut budget = RolloutScanBudget::new(limits.max_entries_scanned, limits.deadline);
    let suffix = format!("-{session_id}.jsonl");
    let mut files_seen = 0usize;
    for year in sorted_desc_entries(&root, |n| valid_year(n).is_some(), &mut budget)? {
        let year_dir = root.join(&year);
        if !real_directory(&year_dir)? {
            continue;
        }
        let Some(year_number) = valid_year(&year.to_string_lossy()) else {
            continue;
        };
        for month in sorted_desc_entries(&year_dir, |n| valid_month(n).is_some(), &mut budget)? {
            let month_dir = year_dir.join(&month);
            if !real_directory(&month_dir)? {
                continue;
            }
            let Some(month_number) = valid_month(&month.to_string_lossy()) else {
                continue;
            };
            for day in sorted_desc_entries(
                &month_dir,
                |name| {
                    fixed_ascii_u32(name, 2).is_some_and(|day| {
                        chrono::NaiveDate::from_ymd_opt(year_number, month_number, day).is_some()
                    })
                },
                &mut budget,
            )? {
                let day_dir = month_dir.join(&day);
                if !real_directory(&day_dir)? {
                    continue;
                }
                let names =
                    sorted_desc_entries(&day_dir, |n| n.starts_with("rollout-"), &mut budget)?;
                files_seen = files_seen
                    .checked_add(names.len())
                    .ok_or_else(|| anyhow!("rollout discovery file counter overflow"))?;
                if files_seen > ROLLOUT_MAX_FILES_SCANNED {
                    return Err(anyhow!(
                        "rollout discovery scanned more than {ROLLOUT_MAX_FILES_SCANNED} files; \
                         refusing a possibly stale answer (GC-DR-08 bounded discovery)"
                    ));
                }
                for name in names {
                    let name_str = name.to_string_lossy();
                    if name_str.ends_with(&suffix) {
                        let candidate = day_dir.join(&name);
                        let meta = fs::symlink_metadata(&candidate)
                            .context("stat candidate Codex rollout")?;
                        if meta.file_type().is_symlink() {
                            return Err(anyhow!("refusing symlinked Codex rollout (fail-closed)"));
                        }
                        if !meta.is_file() {
                            return Err(anyhow!(
                                "refusing non-file Codex rollout {} (fail-closed)",
                                candidate.display()
                            ));
                        }
                        return Ok(Some(candidate));
                    }
                }
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {

    // -- DR-03: codex_rollout_discovery ------------------------------------

    struct CodexHomeGuard {
        prior_home: Option<std::ffi::OsString>,
        prior_codex: Option<std::ffi::OsString>,
    }
    impl CodexHomeGuard {
        fn set(home: &std::path::Path, codex_home: &std::path::Path) -> Self {
            let prior_home = std::env::var_os("LIBRA_TEST_HOME");
            let prior_codex = std::env::var_os("CODEX_HOME");
            // SAFETY: test-only env mutation, restored on drop; #[serial].
            unsafe {
                std::env::set_var("LIBRA_TEST_HOME", home);
                std::env::set_var("CODEX_HOME", codex_home);
            }
            Self {
                prior_home,
                prior_codex,
            }
        }
    }
    impl Drop for CodexHomeGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prior_home {
                    Some(v) => std::env::set_var("LIBRA_TEST_HOME", v),
                    None => std::env::remove_var("LIBRA_TEST_HOME"),
                }
                match &self.prior_codex {
                    Some(v) => std::env::set_var("CODEX_HOME", v),
                    None => std::env::remove_var("CODEX_HOME"),
                }
            }
        }
    }

    /// R1 follow-ups: absolute $CODEX_HOME needs no home; junk lexically-high
    /// dirs never win; I/O errors surface instead of reading "not found".
    #[test]
    #[serial_test::serial]
    fn codex_rollout_discovery_hardening() {
        let tmp = tempfile::tempdir().unwrap();
        let codex_home = tmp.path().join("codex-abs");
        // Point LIBRA_TEST_HOME at a NONEXISTENT dir: with an absolute
        // CODEX_HOME the lookup must still work (home-independent).
        let _g = CodexHomeGuard::set(&tmp.path().join("no-such-home"), &codex_home);
        let sid = "0199a213-81a0-7800-8aa2-58a4a352b423";

        let day = codex_home.join("sessions/2026/07/13");
        std::fs::create_dir_all(&day).unwrap();
        let real = day.join(format!("rollout-2026-07-13T09-30-00-{sid}.jsonl"));
        std::fs::write(&real, "{}\n").unwrap();
        // Junk lexically-above-year directory with a decoy match: the digit
        // filter must keep it out of the walk entirely.
        let junk = codex_home.join("sessions/zzzz/07/13");
        std::fs::create_dir_all(&junk).unwrap();
        std::fs::write(
            junk.join(format!("rollout-9999-01-01T00-00-00-{sid}.jsonl")),
            "{}\n",
        )
        .unwrap();
        // Numeric but impossible dates must not outrank a real partition.
        let invalid_date = codex_home.join("sessions/9999/99/99");
        std::fs::create_dir_all(&invalid_date).unwrap();
        std::fs::write(
            invalid_date.join(format!("rollout-9999-99-99T00-00-00-{sid}.jsonl")),
            "{}\n",
        )
        .unwrap();
        let found = find_codex_rollout(sid).unwrap().expect("found");
        assert_eq!(found, real, "calendar-valid date partitions only");

        // I/O error (unreadable year dir) surfaces as Err, not Ok(None).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let locked = codex_home.join("sessions/2025");
            std::fs::create_dir_all(&locked).unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
            let missing = "ffffffff-1111-2222-3333-444444444444";
            let result = find_codex_rollout(missing);
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(
                result.is_err(),
                "permission failure must be diagnosable, not \"not found\""
            );
        }
    }

    #[test]
    fn codex_rollout_global_entry_budget_is_loud() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["one", "two", "three"] {
            std::fs::write(dir.path().join(name), b"").unwrap();
        }
        let mut budget = RolloutScanBudget::new(2, Duration::from_secs(1));
        let err = sorted_desc_entries(dir.path(), |_| true, &mut budget).unwrap_err();
        assert!(
            err.to_string().contains("more than 2 total entries"),
            "global traversal budget must fail loudly: {err:#}"
        );
    }

    #[test]
    fn codex_rollout_time_budget_is_loud() {
        let dir = tempfile::tempdir().unwrap();
        let mut budget = RolloutScanBudget::new(usize::MAX, Duration::ZERO);
        let err = sorted_desc_entries(dir.path(), |_| true, &mut budget).unwrap_err();
        assert!(
            err.to_string().contains("exceeded its time budget"),
            "traversal deadline must fail loudly even for an empty directory: {err:#}"
        );
    }

    /// M2 R3: FULL-finder loud-bounds regression — nested empty date trees,
    /// oversized file fan-out, and an expired deadline all error through
    /// `find_codex_rollout_bounded`, never returning a silent Ok(None).
    #[test]
    #[serial_test::serial]
    fn codex_rollout_discovery_bounds_fail_loudly() {
        let tmp = tempfile::tempdir().unwrap();
        let codex_home = tmp.path().join("codex-abs");
        let _g = CodexHomeGuard::set(&tmp.path().join("no-such-home"), &codex_home);
        let sid = "0199a213-81a0-7800-8aa2-58a4a352b423";

        // Nested EMPTY numeric date tree: dirs alone exhaust a small entry
        // budget → loud error (the fan-out case).
        for y in 0..6 {
            std::fs::create_dir_all(codex_home.join(format!("sessions/20{:02}/01/01", 10 + y)))
                .unwrap();
        }
        let tight = RolloutDiscoveryLimits {
            max_entries_scanned: 4,
            ..Default::default()
        };
        let err = find_codex_rollout_bounded(sid, tight)
            .expect_err("empty-tree fan-out must fail loudly");
        assert!(format!("{err:#}").contains("total entries"), "got {err:#}");

        // File fan-out: many rollout files exceed the budget → loud error.
        let day = codex_home.join("sessions/2015/01/01");
        std::fs::create_dir_all(&day).unwrap();
        for i in 0..8 {
            std::fs::write(
                day.join(format!(
                    "rollout-2015-01-01T00-00-0{i}-ffffffff-0000-0000-0000-00000000000{i}.jsonl"
                )),
                "{}\n",
            )
            .unwrap();
        }
        let tight_files = RolloutDiscoveryLimits {
            max_entries_scanned: 10,
            ..Default::default()
        };
        let err = find_codex_rollout_bounded(sid, tight_files)
            .expect_err("file fan-out must fail loudly");
        assert!(format!("{err:#}").contains("total entries"), "got {err:#}");

        // Expired deadline errors on the first charge.
        let expired = RolloutDiscoveryLimits {
            deadline: Duration::ZERO,
            ..Default::default()
        };
        let err = find_codex_rollout_bounded(sid, expired)
            .expect_err("expired deadline must fail loudly");
        assert!(format!("{err:#}").contains("time budget"), "got {err:#}");

        // A generous budget still finds nothing here without erroring.
        assert!(
            find_codex_rollout_bounded(
                "eeeeeeee-0000-0000-0000-000000000000",
                RolloutDiscoveryLimits::default()
            )
            .unwrap()
            .is_none()
        );
    }

    /// DR-03: date-partitioned rollout lookup by session id — newest match
    /// wins, invalid ids and symlinks fail closed, absence is Ok(None), and
    /// the walk stays bounded.
    #[test]
    #[serial_test::serial]
    fn codex_rollout_discovery() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let codex_home = tmp.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        let _g = CodexHomeGuard::set(&home, &codex_home);
        let sid = "0199a213-81a0-7800-8aa2-58a4a352b423";

        // Absent store → Ok(None).
        assert!(find_codex_rollout(sid).unwrap().is_none());

        // Two date partitions carry the same session id; the NEWEST wins.
        let old_day = codex_home.join("sessions/2026/06/30");
        let new_day = codex_home.join("sessions/2026/07/13");
        std::fs::create_dir_all(&old_day).unwrap();
        std::fs::create_dir_all(&new_day).unwrap();
        let old_file = old_day.join(format!("rollout-2026-06-30T10-00-00-{sid}.jsonl"));
        let new_file = new_day.join(format!("rollout-2026-07-13T09-30-00-{sid}.jsonl"));
        std::fs::write(&old_file, "{}\n").unwrap();
        std::fs::write(&new_file, "{}\n").unwrap();
        // Unrelated session in the newest partition must not match.
        std::fs::write(
            new_day.join("rollout-2026-07-13T08-00-00-ffffffff-0000-0000-0000-000000000000.jsonl"),
            "{}\n",
        )
        .unwrap();

        let found = find_codex_rollout(sid).unwrap().expect("found");
        assert_eq!(found, new_file, "newest date partition must win");

        // Invalid ids fail closed (no traversal).
        assert!(find_codex_rollout("").is_err());
        assert!(find_codex_rollout("../escape").is_err());
        assert!(find_codex_rollout("id with spaces").is_err());

        // Symlinked rollout fails closed.
        #[cfg(unix)]
        {
            let sid_link = "abcdef00-1111-2222-3333-444444444444";
            let target = tmp.path().join("outside.jsonl");
            std::fs::write(&target, "{}\n").unwrap();
            std::os::unix::fs::symlink(
                &target,
                new_day.join(format!("rollout-2026-07-13T12-00-00-{sid_link}.jsonl")),
            )
            .unwrap();
            assert!(
                find_codex_rollout(sid_link).is_err(),
                "symlinked rollout must be rejected"
            );

            // A symlink in any date component must be rejected before the
            // walk can escape the configured sessions root.
            let sid_dir_link = "abcdef00-5555-6666-7777-888888888888";
            let outside_year = tmp.path().join("outside-year");
            let outside_day = outside_year.join("07/14");
            std::fs::create_dir_all(&outside_day).unwrap();
            std::fs::write(
                outside_day.join(format!("rollout-2027-07-14T12-00-00-{sid_dir_link}.jsonl")),
                "{}\n",
            )
            .unwrap();
            std::os::unix::fs::symlink(&outside_year, codex_home.join("sessions/2027")).unwrap();
            assert!(
                find_codex_rollout(sid_dir_link).is_err(),
                "symlinked date partition must be rejected"
            );
        }
    }

    use std::path::PathBuf;

    use super::*;

    #[test]
    fn promoted_specs_cover_every_v1_preview_kind() {
        // The five agents that were `Preview` in Phase 1 must all be
        // present here. The two original stable kinds (ClaudeCode,
        // Gemini) must NOT — they have dedicated structs.
        for kind in AgentKind::all() {
            let is_dedicated_stable = matches!(kind, AgentKind::ClaudeCode | AgentKind::Gemini);
            assert_eq!(
                stable_promoted_spec_for(*kind).is_some(),
                !is_dedicated_stable,
                "promotion coverage mismatch for {kind:?}"
            );
        }
    }

    /// Companion to `promoted_specs_cover_every_v1_preview_kind`. The
    /// prior test asserts ClaudeCode / Gemini are absent from
    /// `STABLE_PROMOTED_SPECS`, implicitly assuming they have dedicated
    /// adapter structs elsewhere. Removing `ClaudeCodeObservedAgent`
    /// or `GeminiObservedAgent` would not fail that test by itself,
    /// so an entire `AgentKind` could end up with no adapter at all.
    ///
    /// Pin the partition directly: instantiate each dedicated struct
    /// and verify it reports the expected `AgentKind`. A future refactor
    /// that drops either dedicated type fails this test rather than
    /// silently leaving the kind unserviced.
    #[test]
    fn dedicated_stable_adapters_exist_and_report_their_kind() {
        use super::super::{ClaudeCodeObservedAgent, GeminiObservedAgent};

        let claude = ClaudeCodeObservedAgent::new();
        assert_eq!(claude.provider_kind(), AgentKind::ClaudeCode);
        assert_eq!(claude.stability(), AgentStability::Stable);

        let gemini = GeminiObservedAgent::new();
        assert_eq!(gemini.provider_kind(), AgentKind::Gemini);
        assert_eq!(gemini.stability(), AgentStability::Stable);
    }

    #[test]
    fn promoted_agent_reports_stable_tier() {
        let spec = stable_promoted_spec_for(AgentKind::Cursor).unwrap();
        let agent = StablePromotedAgent(spec);
        assert_eq!(agent.stability(), AgentStability::Stable);
        assert_eq!(agent.provider_kind(), AgentKind::Cursor);
        assert_eq!(agent.provider_name(), "cursor");
        assert_eq!(agent.protected_dirs(), &[".cursor"]);
    }

    #[test]
    fn read_transcript_returns_none_when_path_unset() {
        let spec = stable_promoted_spec_for(AgentKind::Codex).unwrap();
        let agent = StablePromotedAgent(spec);
        let ctx = AgentSessionCtx {
            session_id: "s".to_string(),
            provider_session_id: "p".to_string(),
            working_dir: PathBuf::from("/tmp"),
            transcript_path: None,
        };
        assert!(agent.read_transcript(&ctx).unwrap().is_none());
    }

    #[test]
    fn read_transcript_returns_bytes_when_path_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, b"{\"hello\":1}\n").unwrap();
        let spec = stable_promoted_spec_for(AgentKind::OpenCode).unwrap();
        let agent = StablePromotedAgent(spec);
        let ctx = AgentSessionCtx {
            session_id: "s".to_string(),
            provider_session_id: "p".to_string(),
            working_dir: dir.path().to_path_buf(),
            transcript_path: Some(path),
        };
        let bytes = agent.read_transcript(&ctx).unwrap().expect("Some(bytes)");
        assert_eq!(bytes, b"{\"hello\":1}\n");
    }

    #[test]
    fn read_transcript_returns_none_when_path_missing() {
        let spec = stable_promoted_spec_for(AgentKind::Copilot).unwrap();
        let agent = StablePromotedAgent(spec);
        let ctx = AgentSessionCtx {
            session_id: "s".to_string(),
            provider_session_id: "p".to_string(),
            working_dir: PathBuf::from("/tmp"),
            transcript_path: Some(PathBuf::from("/no/such/file.jsonl")),
        };
        assert!(agent.read_transcript(&ctx).unwrap().is_none());
    }
}
