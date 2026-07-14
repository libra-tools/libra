//! Sandbox subsystem for AI tool calls.
//!
//! Boundary: exposes policy parsing, command-safety checks, and runtime enforcement;
//! it does not decide workflow phase state. AI hardening contract tests exercise the
//! public guarantees of this module.

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use chrono::{DateTime, Utc};
use ring::digest::{SHA256, digest};
use serde::Deserialize;
const DEFAULT_APPROVAL_SCOPE: &str = "interactive";
pub const DEFAULT_APPROVAL_TTL: Duration = Duration::from_secs(300);

use tokio::{
    io::AsyncReadExt,
    sync::{Mutex, mpsc::UnboundedSender, oneshot},
};
use uuid::Uuid;

use self::evidence::SandboxEvidenceSink;
use super::runtime::hardening::{SafetyDecision, SafetyDisposition};

mod command_safety;
pub mod evidence;
pub mod policy;
pub mod proxy;
mod proxy_runtime;
pub mod runtime;
#[cfg(target_os = "linux")]
pub mod seccomp_compile;

pub use policy::{
    NetworkAccess, NetworkProtocol, NetworkService, NetworkServiceValidationError,
    SandboxEnforcement, SandboxPermissions, SandboxPolicy, SandboxPolicyError, WritableRoot,
    sensitive_read_paths,
};
pub use proxy::{
    LoopbackOnlyProxy, NetworkAccessMode, NetworkDecision, NetworkProxy, NetworkProxySelection,
    NetworkRequest, NoopProxy, ProxyEnforcement, allowlist_proxy_from_policy, is_loopback_host,
    select_network_proxy,
};
pub use runtime::{
    CommandSpec, ExecEnv, SandboxManager, SandboxTransformError, SandboxTransformRequest,
    SandboxType,
};

/// Runtime sandbox configuration attached to a tool invocation.
#[derive(Clone, Debug)]
pub struct ToolSandboxContext {
    pub policy: SandboxPolicy,
    pub permissions: SandboxPermissions,
}

#[derive(Clone, Debug, Default)]
pub struct ToolRuntimeContext {
    pub sandbox: Option<ToolSandboxContext>,
    pub sandbox_runtime: Option<SandboxRuntimeConfig>,
    pub approval: Option<ToolApprovalContext>,
    pub file_history: Option<FileHistoryRuntimeContext>,
    pub max_output_bytes: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct SandboxRuntimeConfig {
    pub linux_sandbox_exe: Option<PathBuf>,
    pub use_linux_sandbox_bwrap: bool,
    pub enforcement: SandboxEnforcement,
    pub deny_read_paths: Vec<PathBuf>,
    /// Optional structured-event sink that receives sandbox-level
    /// notifications (tmp cleanup failures, writable-root rejections,
    /// future enforcement / network denials). Defaults to `None` —
    /// the sandbox falls back to [`evidence::TracingSandboxEvidenceSink`]
    /// so existing log scrapers see no change. See
    /// [`evidence`](self::evidence) for the full event vocabulary and
    /// `docs/development/commands/sandbox.md` lines 142-144 / 162 / 373 for
    /// the doc contract this hook satisfies.
    pub evidence_sink: Option<std::sync::Arc<dyn evidence::SandboxEvidenceSink>>,
    /// Optional seccomp BPF policy file path. When set on Linux
    /// and the built-in bwrap path is selected,
    /// [`runtime::create_bwrap_command_args_with_seccomp`] appends
    /// `--seccomp <fd>` to the bwrap args and
    /// [`runtime::install_seccomp_policy_pre_exec`] opens the
    /// file in the child to populate that FD. Default `None`
    /// keeps Linux as opt-in unless `~/.libra/seccomp.bpf` exists and
    /// no explicit `LIBRA_SECCOMP_POLICY` override is set. See
    /// `docs/development/commands/sandbox.md` line 19 ("seccomp 注入") for
    /// the doc contract.
    pub seccomp_policy_path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHistoryRuntimeContext {
    pub session_root: PathBuf,
    pub batch_id: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AskForApproval {
    Never,
    OnFailure,
    #[default]
    OnRequest,
    UnlessTrusted,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReviewDecision {
    Approved,
    ApprovedForSession,
    ApprovedForTtl,
    ApprovedForDirectoryTtl,
    ApprovedForPatternTtl,
    ApprovedForAllCommands,
    #[default]
    Denied,
    Abort,
}

impl ReviewDecision {
    fn is_approved(self) -> bool {
        matches!(
            self,
            Self::Approved
                | Self::ApprovedForSession
                | Self::ApprovedForTtl
                | Self::ApprovedForDirectoryTtl
                | Self::ApprovedForPatternTtl
                | Self::ApprovedForAllCommands
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalSensitivityTier {
    Strict,
    Directory,
    Pattern,
}

impl ApprovalSensitivityTier {
    fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Directory => "directory",
            Self::Pattern => "pattern",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalScope {
    Session,
    Project,
    User,
}

impl ApprovalScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Project => "project",
            Self::User => "user",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalCachePolicy {
    pub protected_branches: Vec<String>,
    pub allowed_network_domains: Vec<String>,
    pub no_cache_unknown_network: bool,
    /// OC-Phase 2 P2.5 projection: persistent `Always`-reply approvals
    /// loaded from the `approved_permission` SQLite table for the active
    /// project. `None` when the runtime has not yet populated it (the
    /// default for `Default::default()` and any in-memory test that does
    /// not reach the database). When populated, the rules merge into the
    /// in-memory permission ruleset before per-session rules so a cached
    /// approval survives a process restart but a session-level deny can
    /// still escalate.
    pub approved_ruleset: Option<crate::internal::ai::permission::ApprovedRuleset>,
}

impl Default for ApprovalCachePolicy {
    fn default() -> Self {
        Self {
            protected_branches: vec![
                "main".to_string(),
                "master".to_string(),
                "trunk".to_string(),
                "develop".to_string(),
                "release/*".to_string(),
            ],
            allowed_network_domains: Vec::new(),
            no_cache_unknown_network: false,
            approved_ruleset: None,
        }
    }
}

impl ApprovalCachePolicy {
    fn disabled_reason_for_command(&self, command: &str) -> Option<String> {
        if let Some(branch) = protected_branch_in_command(command, &self.protected_branches) {
            return Some(format!(
                "approval cache disabled because command references protected branch `{branch}`"
            ));
        }

        if let Some(domain) = non_allowlisted_network_domain(
            command,
            &self.allowed_network_domains,
            self.no_cache_unknown_network,
        ) {
            return Some(format!(
                "approval cache disabled because command references non-allowlisted domain `{domain}`"
            ));
        }

        None
    }
}

fn protected_branch_in_command(command: &str, protected_branches: &[String]) -> Option<String> {
    if protected_branches.is_empty() {
        return None;
    }
    let parts = shell_words(command);
    protected_branches
        .iter()
        .map(|branch| branch.trim())
        .filter(|branch| !branch.is_empty())
        .find(|branch| {
            parts.iter().any(|part| {
                protected_branch_pattern_matches(part, branch)
                    || part
                        .strip_prefix("origin/")
                        .is_some_and(|short| protected_branch_pattern_matches(short, branch))
                    || part
                        .strip_prefix("refs/heads/")
                        .is_some_and(|short| protected_branch_pattern_matches(short, branch))
            })
        })
        .map(ToString::to_string)
}

fn protected_branch_pattern_matches(part: &str, branch: &str) -> bool {
    if let Some(prefix) = branch.strip_suffix("/*") {
        return part
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'));
    }
    part == branch
}

fn non_allowlisted_network_domain(
    command: &str,
    allowed_network_domains: &[String],
    no_cache_unknown_network: bool,
) -> Option<String> {
    let domains = extract_network_domains(command);
    if domains.is_empty() {
        return None;
    }
    if allowed_network_domains.is_empty() && !no_cache_unknown_network {
        return None;
    }
    domains.into_iter().find(|domain| {
        !allowed_network_domains
            .iter()
            .any(|allowed| domain_matches_allowed(domain, allowed))
    })
}

fn extract_network_domains(command: &str) -> Vec<String> {
    shell_words(command)
        .into_iter()
        .filter_map(|part| network_domain_from_token(&part))
        .collect()
}

fn network_domain_from_token(token: &str) -> Option<String> {
    let has_scheme = token.contains("://");
    let host_port_path = if let Some((_, rest)) = token.split_once("://") {
        rest.split('@').next_back().unwrap_or(rest)
    } else {
        token
    };
    let host = host_port_path
        .trim_start_matches('[')
        .split(['/', ':', '?', '#', ']'])
        .next()
        .unwrap_or_default()
        .trim()
        .trim_end_matches('.');
    if host.is_empty() {
        return None;
    }
    let is_ascii_domain = host.contains('.')
        && host
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.'))
        && !host.starts_with('-')
        && !host.ends_with('-');
    if is_ascii_domain {
        return Some(host.to_ascii_lowercase());
    }
    // A scheme-qualified URL with a non-ASCII / IDN host shouldn't silently
    // bypass network policy — return a sentinel that never matches an
    // allowlist entry so `no_cache_unknown_network` and the allowlist gate
    // both treat the request as untrusted. Bare tokens without a scheme
    // (paths, args) still fall through to None.
    if has_scheme && host.contains('.') {
        Some(format!("__non_ascii__:{host}"))
    } else {
        None
    }
}

fn domain_matches_allowed(domain: &str, allowed: &str) -> bool {
    let allowed = allowed.trim().trim_end_matches('.').to_ascii_lowercase();
    !allowed.is_empty() && (domain == allowed || domain.ends_with(&format!(".{allowed}")))
}

fn shell_words(command: &str) -> Vec<String> {
    shlex::split(command)
        .filter(|parts| !parts.is_empty())
        .unwrap_or_else(|| {
            command
                .split_whitespace()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalMemo {
    pub key: String,
    pub decision: ReviewDecision,
    pub expires_at: Option<DateTime<Utc>>,
    pub scope: ApprovalScope,
    pub sensitivity_tier: ApprovalSensitivityTier,
}

impl ApprovalMemo {
    fn session(
        key: String,
        decision: ReviewDecision,
        scope: ApprovalScope,
        sensitivity_tier: ApprovalSensitivityTier,
    ) -> Self {
        Self {
            key,
            decision,
            expires_at: None,
            scope,
            sensitivity_tier,
        }
    }

    fn ttl(
        key: String,
        decision: ReviewDecision,
        scope: ApprovalScope,
        sensitivity_tier: ApprovalSensitivityTier,
        now: DateTime<Utc>,
        ttl: Duration,
    ) -> Self {
        // A pathological caller could pass a TTL that overflows
        // `chrono::Duration` or `now + ttl`. Without a fallback, `expires_at`
        // would be `None`, which `is_active_at` treats as "never expires" —
        // silently turning a TTL memo into a session-permanent one. Substitute
        // a 7-day cap on the overflow paths so the memo still expires; honest
        // in-range TTLs flow through unchanged.
        const OVERFLOW_FALLBACK_HOURS: i64 = 24 * 7;
        let fallback = chrono::Duration::hours(OVERFLOW_FALLBACK_HOURS);
        let bounded = chrono::Duration::from_std(ttl).unwrap_or(fallback);
        let expires_at = now
            .checked_add_signed(bounded)
            .or_else(|| now.checked_add_signed(fallback));
        Self {
            key,
            decision,
            expires_at,
            scope,
            sensitivity_tier,
        }
    }

    fn is_active_at(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_none_or(|expires_at| expires_at > now)
    }
}

#[derive(Debug, Default)]
pub struct ApprovalStore {
    map: HashMap<String, ApprovalMemo>,
    allow_all_commands_scopes: HashSet<String>,
}

impl ApprovalStore {
    pub fn get(&self, key: &str) -> Option<ReviewDecision> {
        self.get_at(key, Utc::now())
    }

    pub fn get_at(&self, key: &str, now: DateTime<Utc>) -> Option<ReviewDecision> {
        self.map
            .get(key)
            .filter(|memo| memo.is_active_at(now))
            .map(|memo| memo.decision)
    }

    pub fn put(&mut self, key: String, value: ReviewDecision) {
        if !matches!(value, ReviewDecision::ApprovedForSession) {
            return;
        }
        self.map.insert(
            key.clone(),
            ApprovalMemo::session(
                key,
                value,
                ApprovalScope::Session,
                ApprovalSensitivityTier::Strict,
            ),
        );
    }

    pub fn put_ttl(
        &mut self,
        key: String,
        value: ReviewDecision,
        scope: ApprovalScope,
        sensitivity_tier: ApprovalSensitivityTier,
        now: DateTime<Utc>,
        ttl: Duration,
    ) {
        if !matches!(value, ReviewDecision::ApprovedForTtl) {
            return;
        }
        self.map.insert(
            key.clone(),
            ApprovalMemo::ttl(key, value, scope, sensitivity_tier, now, ttl),
        );
    }

    pub fn revoke(&mut self, key: &str) -> bool {
        self.map.remove(key).is_some()
    }

    /// Drop the broad "approve every command in this scope" decision so the
    /// next matching command falls back to the regular prompt path. Returns
    /// `true` if a record was removed. Without this, an `ApprovedForAllCommands`
    /// answer would persist for the rest of the session with no revoke surface.
    pub fn revoke_allow_all_for_scope(&mut self, scope: &str) -> bool {
        self.allow_all_commands_scopes
            .remove(normalized_approval_scope(scope).as_str())
    }

    /// Snapshot of every active scope that currently has the allow-all
    /// decision recorded. Sorted for deterministic UI output.
    pub fn active_allow_all_scopes(&self) -> Vec<String> {
        let mut scopes: Vec<String> = self.allow_all_commands_scopes.iter().cloned().collect();
        scopes.sort();
        scopes
    }

    pub fn active_memos_at(&self, now: DateTime<Utc>) -> Vec<ApprovalMemo> {
        let mut memos = self
            .map
            .values()
            .filter(|memo| memo.is_active_at(now))
            .cloned()
            .collect::<Vec<_>>();
        memos.sort_by(|a, b| a.key.cmp(&b.key));
        memos
    }

    pub fn allow_all_commands(&self) -> bool {
        self.allow_all_commands_for_scope(DEFAULT_APPROVAL_SCOPE)
    }

    pub fn approve_all_commands(&mut self) {
        self.approve_all_commands_for_scope(DEFAULT_APPROVAL_SCOPE);
    }

    pub fn allow_all_commands_for_scope(&self, scope: &str) -> bool {
        self.allow_all_commands_scopes
            .contains(normalized_approval_scope(scope).as_str())
    }

    pub fn approve_all_commands_for_scope(&mut self, scope: &str) {
        self.allow_all_commands_scopes
            .insert(normalized_approval_scope(scope));
    }
}

pub async fn request_cached_approval_with_keys<F>(
    ctx: &ToolApprovalContext,
    keys: &[String],
    build_request: F,
) -> ReviewDecision
where
    F: FnOnce(oneshot::Sender<ReviewDecision>) -> ExecApprovalRequest,
{
    request_cached_approval_with_cache_keys(
        ctx,
        ApprovalCacheKeys::strict(keys.to_vec()),
        None,
        |response_tx, _cache_disabled_reason| build_request(response_tx),
    )
    .await
}

async fn request_cached_approval_with_cache_keys<F>(
    ctx: &ToolApprovalContext,
    keys: ApprovalCacheKeys,
    cache_disabled_reason: Option<String>,
    build_request: F,
) -> ReviewDecision
where
    F: FnOnce(oneshot::Sender<ReviewDecision>, Option<String>) -> ExecApprovalRequest,
{
    let scope = ctx
        .scope_key_prefix
        .as_deref()
        .unwrap_or(DEFAULT_APPROVAL_SCOPE);
    let scoped_keys = keys.scoped(scope);
    if cache_disabled_reason.is_none() {
        let store = ctx.store.lock().await;
        if store.allow_all_commands_for_scope(scope) {
            tracing::debug!(
                target: "libra::internal::ai::sandbox",
                key_count = scoped_keys.lookup.len(),
                approval_scope = scope,
                "approval request skipped by allow-all-commands session decision"
            );
            return ReviewDecision::ApprovedForAllCommands;
        }
    }

    let cached_decision = if cache_disabled_reason.is_some() || scoped_keys.lookup.is_empty() {
        None
    } else {
        let store = ctx.store.lock().await;
        if scoped_keys.require_all_lookup {
            cached_approval_decision(&store, &scoped_keys.lookup)
        } else {
            cached_any_approval_decision(&store, &scoped_keys.lookup)
        }
    };
    if let Some(decision) = cached_decision {
        tracing::debug!(
            target: "libra::internal::ai::sandbox",
            key_count = scoped_keys.lookup.len(),
            approval_scope = scope,
            decision = ?decision,
            "approval request skipped by matching cached approval"
        );
        return decision;
    }

    let (response_tx, response_rx) = oneshot::channel();
    let request = build_request(response_tx, cache_disabled_reason.clone());
    if ctx.request_tx.send(request).is_err() {
        return ReviewDecision::Denied;
    }

    let decision = response_rx.await.unwrap_or_default();
    if cache_disabled_reason.is_some() {
        return if decision.is_approved() {
            ReviewDecision::Approved
        } else {
            decision
        };
    }

    if matches!(decision, ReviewDecision::ApprovedForAllCommands) {
        let mut store = ctx.store.lock().await;
        store.approve_all_commands_for_scope(scope);
        tracing::debug!(
            target: "libra::internal::ai::sandbox",
            approval_scope = scope,
            "approval decision cached as allow-all-commands for this session"
        );
    } else if matches!(decision, ReviewDecision::ApprovedForSession)
        && !scoped_keys.strict.is_empty()
    {
        let mut store = ctx.store.lock().await;
        for key in &scoped_keys.strict {
            store.put(key.clone(), ReviewDecision::ApprovedForSession);
        }
        tracing::debug!(
            target: "libra::internal::ai::sandbox",
            key_count = scoped_keys.strict.len(),
            approval_scope = scope,
            "approval decision cached for matching commands"
        );
    } else if matches!(decision, ReviewDecision::ApprovedForTtl) && !scoped_keys.strict.is_empty() {
        let mut store = ctx.store.lock().await;
        let now = Utc::now();
        for key in &scoped_keys.strict {
            store.put_ttl(
                key.clone(),
                ReviewDecision::ApprovedForTtl,
                ApprovalScope::Session,
                ApprovalSensitivityTier::Strict,
                now,
                ctx.approval_ttl,
            );
        }
        tracing::debug!(
            target: "libra::internal::ai::sandbox",
            key_count = scoped_keys.strict.len(),
            approval_scope = scope,
            ttl_secs = ctx.approval_ttl.as_secs(),
            "approval decision cached with ttl for matching commands"
        );
    } else if matches!(decision, ReviewDecision::ApprovedForDirectoryTtl)
        && !scoped_keys.directory_ttl.is_empty()
    {
        let mut store = ctx.store.lock().await;
        let now = Utc::now();
        for key in &scoped_keys.directory_ttl {
            store.put_ttl(
                key.clone(),
                ReviewDecision::ApprovedForTtl,
                ApprovalScope::Session,
                ApprovalSensitivityTier::Directory,
                now,
                ctx.approval_ttl,
            );
        }
        tracing::debug!(
            target: "libra::internal::ai::sandbox",
            key_count = scoped_keys.directory_ttl.len(),
            approval_scope = scope,
            ttl_secs = ctx.approval_ttl.as_secs(),
            "approval decision cached with directory ttl for matching commands"
        );
    } else if matches!(decision, ReviewDecision::ApprovedForPatternTtl)
        && !scoped_keys.pattern_ttl.is_empty()
    {
        let mut store = ctx.store.lock().await;
        let now = Utc::now();
        for key in &scoped_keys.pattern_ttl {
            store.put_ttl(
                key.clone(),
                ReviewDecision::ApprovedForTtl,
                ApprovalScope::Session,
                ApprovalSensitivityTier::Pattern,
                now,
                ctx.approval_ttl,
            );
        }
        tracing::debug!(
            target: "libra::internal::ai::sandbox",
            key_count = scoped_keys.pattern_ttl.len(),
            approval_scope = scope,
            ttl_secs = ctx.approval_ttl.as_secs(),
            "approval decision cached with pattern ttl for matching commands"
        );
    }
    decision
}

fn cached_approval_decision(
    store: &ApprovalStore,
    scoped_keys: &[String],
) -> Option<ReviewDecision> {
    let mut saw_ttl = false;
    for key in scoped_keys {
        match store.get(key) {
            Some(ReviewDecision::ApprovedForSession) => {}
            Some(ReviewDecision::ApprovedForTtl) => saw_ttl = true,
            _ => return None,
        }
    }

    if saw_ttl {
        Some(ReviewDecision::ApprovedForTtl)
    } else {
        Some(ReviewDecision::ApprovedForSession)
    }
}

fn cached_any_approval_decision(
    store: &ApprovalStore,
    scoped_keys: &[String],
) -> Option<ReviewDecision> {
    let mut saw_ttl = false;
    for key in scoped_keys {
        match store.get(key) {
            Some(ReviewDecision::ApprovedForSession) => {
                return Some(ReviewDecision::ApprovedForSession);
            }
            Some(ReviewDecision::ApprovedForTtl) => saw_ttl = true,
            _ => {}
        }
    }
    saw_ttl.then_some(ReviewDecision::ApprovedForTtl)
}

fn normalized_approval_scope(scope: &str) -> String {
    let trimmed = scope.trim();
    if trimmed.is_empty() {
        DEFAULT_APPROVAL_SCOPE.to_string()
    } else {
        trimmed.to_string()
    }
}

fn scoped_approval_keys(scope: &str, keys: &[String]) -> Vec<String> {
    let scope = normalized_approval_scope(scope);
    if scope == DEFAULT_APPROVAL_SCOPE {
        return keys.to_vec();
    }
    keys.iter()
        .map(|key| format!("{scope}:{key}"))
        .collect::<Vec<_>>()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ApprovalCacheKeys {
    lookup: Vec<String>,
    strict: Vec<String>,
    directory_ttl: Vec<String>,
    pattern_ttl: Vec<String>,
    require_all_lookup: bool,
}

impl ApprovalCacheKeys {
    fn strict(keys: Vec<String>) -> Self {
        Self {
            lookup: keys.clone(),
            strict: keys,
            directory_ttl: Vec::new(),
            pattern_ttl: Vec::new(),
            require_all_lookup: true,
        }
    }

    fn shell(command: &str, cwd: &Path, sandbox_permissions: SandboxPermissions) -> Self {
        let strict = shell_approval_key_with_scope(
            command,
            cwd,
            sandbox_permissions,
            ApprovalScope::Session,
            ApprovalSensitivityTier::Strict,
        );
        let directory = shell_approval_key_with_scope(
            command,
            cwd,
            sandbox_permissions,
            ApprovalScope::Session,
            ApprovalSensitivityTier::Directory,
        );
        let pattern = shell_approval_key_with_scope(
            command,
            cwd,
            sandbox_permissions,
            ApprovalScope::Session,
            ApprovalSensitivityTier::Pattern,
        );
        Self {
            lookup: vec![strict.clone(), directory.clone(), pattern.clone()],
            strict: vec![strict],
            directory_ttl: vec![directory],
            pattern_ttl: vec![pattern],
            require_all_lookup: false,
        }
    }

    fn scoped(&self, scope: &str) -> Self {
        Self {
            lookup: scoped_approval_keys(scope, &self.lookup),
            strict: scoped_approval_keys(scope, &self.strict),
            directory_ttl: scoped_approval_keys(scope, &self.directory_ttl),
            pattern_ttl: scoped_approval_keys(scope, &self.pattern_ttl),
            require_all_lookup: self.require_all_lookup,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToolApprovalContext {
    pub policy: AskForApproval,
    pub request_tx: UnboundedSender<ExecApprovalRequest>,
    pub store: Arc<Mutex<ApprovalStore>>,
    pub scope_key_prefix: Option<String>,
    pub approval_ttl: Duration,
    pub cache_policy: ApprovalCachePolicy,
}

pub struct ExecApprovalRequest {
    pub call_id: String,
    pub command: String,
    pub cwd: PathBuf,
    pub reason: Option<String>,
    pub is_retry: bool,
    pub sandbox_label: String,
    pub network_access: NetworkAccess,
    pub writable_roots: Vec<PathBuf>,
    pub cache_disabled_reason: Option<String>,
    pub response_tx: oneshot::Sender<ReviewDecision>,
}

impl std::fmt::Debug for ExecApprovalRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecApprovalRequest")
            .field("call_id", &self.call_id)
            .field("command", &self.command)
            .field("cwd", &self.cwd)
            .field("reason", &self.reason)
            .field("is_retry", &self.is_retry)
            .field("sandbox_label", &self.sandbox_label)
            .field("network_access", &self.network_access)
            .field("writable_roots", &self.writable_roots)
            .field("cache_disabled_reason", &self.cache_disabled_reason)
            .field("response_tx", &"<oneshot::Sender>")
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct SandboxExecOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

#[derive(Clone, Debug)]
pub struct ShellCommandRequest {
    pub call_id: String,
    pub command: String,
    pub cwd: PathBuf,
    pub timeout_ms: Option<u64>,
    pub max_output_bytes: usize,
    pub sandbox: Option<ToolSandboxContext>,
    pub sandbox_runtime: Option<SandboxRuntimeConfig>,
    pub evidence_sink: Option<std::sync::Arc<dyn evidence::SandboxEvidenceSink>>,
    pub approval: Option<ToolApprovalContext>,
    pub justification: Option<String>,
    pub safety_decision: Option<SafetyDecision>,
}

#[derive(Default, Clone)]
struct StreamState {
    bytes: Vec<u8>,
    truncated: bool,
}

const DEFAULT_TIMEOUT_MS: u64 = 60_000;
const TIMEOUT_EXIT_CODE: i32 = 124;
const STREAM_DRAIN_TIMEOUT: Duration = Duration::from_millis(250);
const COMMAND_TMPDIR_PREFIX: &str = "libra-sandbox-";
const COMMAND_TMPDIR_CREATE_ATTEMPTS: usize = 8;
const SANDBOX_ENFORCEMENT_ENV: &str = "LIBRA_SANDBOX_ENFORCEMENT";
/// Environment variable users export to opt into seccomp BPF
/// policy injection without editing `SandboxRuntimeConfig`
/// directly. Value is an absolute path to a precompiled BPF
/// binary (output of `seccompiler --output bpf-bin` or
/// equivalent). See `docs/sandbox-seccomp.md` for the recommended
/// policy and compilation steps.
const DEFAULT_SECCOMP_POLICY_PATH: &str = ".libra/seccomp.bpf";
const SANDBOX_SECCOMP_POLICY_ENV: &str = "LIBRA_SECCOMP_POLICY";
const SANDBOX_CONFIG_FILE: &str = "sandbox.toml";
#[cfg(unix)]
const COMMAND_TMPDIR_MODE: u32 = 0o700;
const SANDBOX_DENIED_KEYWORDS: [&str; 7] = [
    "operation not permitted",
    "permission denied",
    "read-only file system",
    "seccomp",
    "sandbox",
    "landlock",
    "failed to write file",
];
const QUICK_REJECT_EXIT_CODES: [i32; 3] = [129, 126, 127];

pub async fn run_shell_command(
    command: &str,
    cwd: &Path,
    timeout_ms: Option<u64>,
    max_output_bytes: usize,
    sandbox: Option<ToolSandboxContext>,
    sandbox_runtime: Option<&SandboxRuntimeConfig>,
) -> Result<SandboxExecOutput, String> {
    let spec = CommandSpec::shell(
        command,
        cwd.to_path_buf(),
        timeout_ms,
        sandbox
            .as_ref()
            .map(|context| context.permissions)
            .unwrap_or(SandboxPermissions::UseDefault),
        None,
    );
    run_command_spec(spec, max_output_bytes, sandbox, sandbox_runtime, None, None).await
}

pub async fn run_shell_command_with_approval(
    request: ShellCommandRequest,
) -> Result<SandboxExecOutput, String> {
    let ShellCommandRequest {
        call_id,
        command,
        cwd,
        timeout_ms,
        max_output_bytes,
        sandbox,
        sandbox_runtime,
        evidence_sink,
        approval,
        justification,
        safety_decision,
    } = request;

    let spec = CommandSpec::shell(
        &command,
        cwd.clone(),
        timeout_ms,
        sandbox
            .as_ref()
            .map(|context| context.permissions)
            .unwrap_or(SandboxPermissions::UseDefault),
        justification.clone(),
    );

    let allow_all_commands = if let Some(ctx) = approval.as_ref() {
        let scope = ctx
            .scope_key_prefix
            .as_deref()
            .unwrap_or(DEFAULT_APPROVAL_SCOPE);
        ctx.store.lock().await.allow_all_commands_for_scope(scope)
    } else {
        false
    };
    let allow_all_bypasses_prompt = allow_all_commands
        && !matches!(
            safety_decision
                .as_ref()
                .map(|decision| &decision.disposition),
            Some(SafetyDisposition::Deny)
        );
    let requirement = if allow_all_bypasses_prompt {
        ExecApprovalRequirement::Skip {
            bypass_sandbox: false,
        }
    } else {
        approval
            .as_ref()
            .map(|ctx| {
                shell_exec_approval_requirement(
                    ctx.policy,
                    sandbox.as_ref().map(|s| &s.policy),
                    &command,
                    spec.sandbox_permissions,
                    safety_decision.as_ref(),
                )
            })
            .unwrap_or(ExecApprovalRequirement::Skip {
                bypass_sandbox: false,
            })
    };

    let mut already_approved = allow_all_bypasses_prompt;

    if let ExecApprovalRequirement::Forbidden { ref reason } = requirement {
        return Err(reason.clone());
    }

    let network_access_upgrade = if approval.is_some() {
        requested_network_access_upgrade(
            sandbox.as_ref().map(|context| &context.policy),
            spec.sandbox_permissions,
            &cwd,
            false,
        )?
    } else {
        None
    };

    let mut approved_network_access_upgrade = None;

    if let Some(approval_ctx) = approval.as_ref() {
        if let Some(upgrade_access) = network_access_upgrade.as_ref() {
            if matches!(approval_ctx.policy, AskForApproval::Never) {
                return Err(
                    "network access escalation requires approval, but approval policy is never"
                        .to_string(),
                );
            }
            let network_reason = format!(
                "requested network access escalation from {current:?} to {requested:?}",
                current = shell_policy_network_access(
                    sandbox.as_ref().map(|context| &context.policy),
                    spec.sandbox_permissions,
                    false,
                ),
                requested = upgrade_access,
            );
            let reason = match &requirement {
                ExecApprovalRequirement::NeedsApproval { reason } => reason
                    .clone()
                    .or_else(|| {
                        justification
                            .as_deref()
                            .map(str::trim)
                            .filter(|text| !text.is_empty())
                            .map(ToString::to_string)
                    })
                    .map(|reason| format!("{reason}; {network_reason}"))
                    .or(Some(network_reason)),
                _ => Some(network_reason),
            };
            let decision = request_uncached_exec_approval(
                approval_ctx,
                ExecApprovalPrompt {
                    call_id: &call_id,
                    command: &command,
                    cwd: &cwd,
                    reason,
                    sandbox_policy: sandbox.as_ref().map(|s| &s.policy),
                    sandbox_permissions: spec.sandbox_permissions,
                    is_retry: false,
                    requested_network_access: Some(upgrade_access.clone()),
                },
                Some("network access escalation approvals are not cached".to_string()),
            )
            .await;

            if decision.is_approved() {
                if matches!(requirement, ExecApprovalRequirement::NeedsApproval { .. }) {
                    already_approved = true;
                }
                approved_network_access_upgrade = Some(upgrade_access.clone());
            } else {
                match decision {
                    ReviewDecision::Denied => return Err("rejected by user".to_string()),
                    ReviewDecision::Abort => return Err("aborted by user".to_string()),
                    _ => {}
                }
            }
        } else {
            match requirement {
                ExecApprovalRequirement::Skip { .. } => {}
                ExecApprovalRequirement::NeedsApproval { ref reason } => {
                    let decision = request_exec_approval(
                        approval_ctx,
                        ExecApprovalPrompt {
                            call_id: &call_id,
                            command: &command,
                            cwd: &cwd,
                            reason: reason.clone().or_else(|| {
                                justification
                                    .as_deref()
                                    .map(str::trim)
                                    .filter(|text| !text.is_empty())
                                    .map(ToString::to_string)
                            }),
                            sandbox_policy: sandbox.as_ref().map(|s| &s.policy),
                            sandbox_permissions: spec.sandbox_permissions,
                            is_retry: false,
                            requested_network_access: None,
                        },
                    )
                    .await;

                    if decision.is_approved() {
                        already_approved = true;
                    } else {
                        match decision {
                            ReviewDecision::Denied => return Err("rejected by user".to_string()),
                            ReviewDecision::Abort => return Err("aborted by user".to_string()),
                            _ => {}
                        }
                    }
                }
                ExecApprovalRequirement::Forbidden { ref reason } => {
                    return Err(reason.clone());
                }
            }
        }
    }

    let first_attempt_is_sandboxed = sandbox.is_some()
        && !spec.sandbox_permissions.requires_escalated_permissions()
        && !matches!(
            requirement,
            ExecApprovalRequirement::Skip {
                bypass_sandbox: true
            }
        );
    if first_attempt_is_sandboxed
        && let Some(reason) =
            prefer_strict_sandbox_fallback_reason(sandbox.as_ref(), sandbox_runtime.as_ref())?
    {
        let Some(approval_ctx) = approval.as_ref() else {
            return Err(format!(
                "{reason}; no approval channel is available to confirm the sandbox downgrade"
            ));
        };
        if matches!(approval_ctx.policy, AskForApproval::Never) {
            return Err(format!(
                "{reason}; approval policy is never, so Libra cannot confirm the sandbox downgrade"
            ));
        }
        let decision = request_uncached_exec_approval(
            approval_ctx,
            ExecApprovalPrompt {
                call_id: &call_id,
                command: &command,
                cwd: &cwd,
                reason: Some(reason),
                sandbox_policy: sandbox.as_ref().map(|s| &s.policy),
                sandbox_permissions: SandboxPermissions::RequireEscalated,
                is_retry: true,
                requested_network_access: None,
            },
            Some("sandbox fallback approvals are not cached".to_string()),
        )
        .await;

        if !decision.is_approved() {
            match decision {
                ReviewDecision::Denied => return Err("rejected by user".to_string()),
                ReviewDecision::Abort => return Err("aborted by user".to_string()),
                _ => {}
            }
        }
    }

    let first_attempt_sandbox = if first_attempt_is_sandboxed {
        sandbox.clone()
    } else {
        None
    };

    let first_output = run_command_spec(
        spec.clone(),
        max_output_bytes,
        first_attempt_sandbox,
        sandbox_runtime.as_ref(),
        approved_network_access_upgrade,
        evidence_sink.clone(),
    )
    .await?;

    if !first_attempt_is_sandboxed || !is_likely_sandbox_denied(&first_output) {
        return Ok(first_output);
    }

    let Some(approval_ctx) = approval.as_ref() else {
        return Ok(first_output);
    };
    if !wants_no_sandbox_approval(approval_ctx.policy) {
        return Ok(first_output);
    }

    if !should_bypass_approval(approval_ctx.policy, already_approved) {
        let decision = request_exec_approval(
            approval_ctx,
            ExecApprovalPrompt {
                call_id: &call_id,
                command: &command,
                cwd: &cwd,
                reason: Some(build_denial_reason_from_output(&first_output)),
                sandbox_policy: sandbox.as_ref().map(|s| &s.policy),
                sandbox_permissions: spec.sandbox_permissions,
                is_retry: true,
                requested_network_access: None,
            },
        )
        .await;

        if !decision.is_approved() {
            match decision {
                ReviewDecision::Denied => return Err("rejected by user".to_string()),
                ReviewDecision::Abort => return Err("aborted by user".to_string()),
                _ => {}
            }
        }
    }

    run_command_spec(
        spec,
        max_output_bytes,
        None,
        sandbox_runtime.as_ref(),
        None,
        evidence_sink,
    )
    .await
}

pub async fn run_command_spec(
    mut spec: CommandSpec,
    max_output_bytes: usize,
    mut sandbox: Option<ToolSandboxContext>,
    sandbox_runtime: Option<&SandboxRuntimeConfig>,
    network_access_override: Option<NetworkAccess>,
    evidence_sink: Option<std::sync::Arc<dyn evidence::SandboxEvidenceSink>>,
) -> Result<SandboxExecOutput, String> {
    let command_tmpdir = create_command_tmpdir()?;
    inject_command_tmp_env(&mut spec, &command_tmpdir);
    // The built-in bubblewrap profile replaces `/tmp` with a private tmpfs.
    // Add only this command's 0700 temp directory as an explicit writable
    // root so TMPDIR remains usable without granting the whole host `/tmp`.
    if let Some(ToolSandboxContext {
        policy: SandboxPolicy::WorkspaceWrite { writable_roots, .. },
        ..
    }) = sandbox.as_mut()
        && !writable_roots.contains(&command_tmpdir)
    {
        writable_roots.push(command_tmpdir.clone());
    }

    let output = run_command_spec_inner(
        spec,
        max_output_bytes,
        sandbox,
        sandbox_runtime,
        network_access_override,
        evidence_sink.clone(),
    )
    .await;
    let runtime_evidence_sink = sandbox_runtime.and_then(|cfg| cfg.evidence_sink.clone());
    let cleanup_sink = evidence_sink
        .as_deref()
        .or(runtime_evidence_sink.as_deref());
    cleanup_command_tmpdir(&command_tmpdir, cleanup_sink).await;
    output
}

async fn run_command_spec_inner(
    spec: CommandSpec,
    max_output_bytes: usize,
    sandbox: Option<ToolSandboxContext>,
    sandbox_runtime: Option<&SandboxRuntimeConfig>,
    network_access_override: Option<NetworkAccess>,
    evidence_sink: Option<std::sync::Arc<dyn evidence::SandboxEvidenceSink>>,
) -> Result<SandboxExecOutput, String> {
    let built = build_command_from_spec(
        spec,
        sandbox.as_ref(),
        sandbox_runtime,
        network_access_override,
        evidence_sink.as_deref(),
    )?;
    let mut protected_mountpoints =
        PreparedProtectedMountpoints::prepare(&built.protected_mount_cleanup_paths)?;
    let output = run_built_command(built, max_output_bytes, sandbox_runtime, evidence_sink).await;
    let cleanup = protected_mountpoints.cleanup();
    match (output, cleanup) {
        (Ok(output), Ok(())) => Ok(output),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup_error)) => Err(format!("{error}; additionally, {cleanup_error}")),
    }
}

async fn run_built_command(
    mut built: BuiltCommand,
    max_output_bytes: usize,
    sandbox_runtime: Option<&SandboxRuntimeConfig>,
    evidence_sink: Option<std::sync::Arc<dyn evidence::SandboxEvidenceSink>>,
) -> Result<SandboxExecOutput, String> {
    let proxy_evidence_sink = evidence_sink
        .clone()
        .or_else(|| sandbox_runtime.and_then(|cfg| cfg.evidence_sink.clone()));
    let mut allowlist_proxy = start_allowlist_proxy_if_needed(
        &mut built.command,
        built.allowlist_proxy_services,
        proxy_evidence_sink,
    )
    .await?;
    let timeout_override = built.timeout_ms;
    repair_missing_process_cwd(&built.process_cwd)?;
    let mut cmd = built.command;
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match spawn_shell_command(&mut cmd, &built.process_cwd) {
        Ok(child) => child,
        Err(error) => {
            #[cfg(test)]
            let fallback_error = if error.kind() == ErrorKind::NotFound {
                match run_std_command_fallback(
                    built.exec_env.clone(),
                    max_output_bytes,
                    timeout_override,
                )
                .await
                {
                    Ok(output) => {
                        if let Some(proxy) = allowlist_proxy.take() {
                            proxy.shutdown().await;
                        }
                        return Ok(output);
                    }
                    Err(fallback_error) => Some(fallback_error),
                }
            } else {
                None
            };
            #[cfg(not(test))]
            let fallback_error: Option<String> = None;

            if let Some(proxy) = allowlist_proxy.take() {
                proxy.shutdown().await;
            }
            let process_cwd = std::env::current_dir()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|err| format!("<unavailable: {err}>"));
            let fallback_detail = fallback_error
                .map(|detail| format!("; std fallback also failed: {detail}"))
                .unwrap_or_default();
            return Err(format!(
                "failed to spawn shell program `{}` in `{}` \
                 (spawn cwd exists: {}, process cwd: {}): {error}{fallback_detail}",
                built.spawn_program,
                built.spawn_cwd.display(),
                built.spawn_cwd.exists(),
                process_cwd
            ));
        }
    };

    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture stdout".to_string())?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| "failed to capture stderr".to_string())?;

    let stdout_state = Arc::new(Mutex::new(StreamState::default()));
    let stderr_state = Arc::new(Mutex::new(StreamState::default()));
    let stdout_task = tokio::spawn(drain_reader(
        stdout_pipe,
        max_output_bytes,
        Arc::clone(&stdout_state),
    ));
    let stderr_task = tokio::spawn(drain_reader(
        stderr_pipe,
        max_output_bytes,
        Arc::clone(&stderr_state),
    ));

    let timeout_dur = Duration::from_millis(timeout_override.unwrap_or(DEFAULT_TIMEOUT_MS));
    let (exit_code, timed_out) = tokio::select! {
        status = child.wait() => {
            let code = status
                .map_err(|e| format!("wait failed: {e}"))?
                .code()
                .unwrap_or(-1);
            (code, false)
        }
        _ = tokio::time::sleep(timeout_dur) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            (TIMEOUT_EXIT_CODE, true)
        }
    };

    let (mut stdout, stdout_truncated, stdout_incomplete) =
        collect_stream(stdout_task, stdout_state).await;
    let (mut stderr, stderr_truncated, stderr_incomplete) =
        collect_stream(stderr_task, stderr_state).await;

    if stdout_truncated {
        stdout.push_str("\n[stdout truncated]");
    }
    if stderr_truncated {
        stderr.push_str("\n[stderr truncated]");
    }
    if stdout_incomplete {
        stdout.push_str("\n[stdout stream incomplete]");
    }
    if stderr_incomplete {
        stderr.push_str("\n[stderr stream incomplete]");
    }

    if let Some(proxy) = allowlist_proxy.take() {
        proxy.shutdown().await;
    }

    Ok(SandboxExecOutput {
        exit_code,
        stdout,
        stderr,
        timed_out,
    })
}

fn create_command_tmpdir() -> Result<PathBuf, String> {
    let system_tmp = std::env::temp_dir();
    for _ in 0..COMMAND_TMPDIR_CREATE_ATTEMPTS {
        let path = system_tmp.join(format!("{COMMAND_TMPDIR_PREFIX}{}", Uuid::new_v4()));
        match create_private_command_tmpdir(&path) {
            Ok(()) => return Ok(path),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(format!(
                    "failed to create private command tmp dir `{}`: {err}",
                    path.display()
                ));
            }
        }
    }

    Err(format!(
        "failed to allocate a unique private command tmp dir under `{}`",
        system_tmp.display()
    ))
}

#[cfg(unix)]
fn create_private_command_tmpdir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    let mut builder = std::fs::DirBuilder::new();
    builder.mode(COMMAND_TMPDIR_MODE).create(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(COMMAND_TMPDIR_MODE))
}

#[cfg(not(unix))]
fn create_private_command_tmpdir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir(path)
}

fn inject_command_tmp_env(spec: &mut CommandSpec, command_tmpdir: &Path) {
    let command_tmpdir = command_tmpdir.to_string_lossy().into_owned();
    spec.env
        .insert("TMPDIR".to_string(), command_tmpdir.clone());
    spec.env.insert("TEMP".to_string(), command_tmpdir.clone());
    spec.env.insert("TMP".to_string(), command_tmpdir);
}

async fn cleanup_command_tmpdir(
    path: &Path,
    evidence_sink: Option<&dyn evidence::SandboxEvidenceSink>,
) {
    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => {
            // Doc contract: `docs/development/commands/sandbox.md:142` —
            // surface tmp cleanup failure as a structured Evidence
            // event in addition to the legacy tracing line. The
            // default `TracingSandboxEvidenceSink` keeps the
            // existing log shape; an opt-in agent-runtime sink can
            // route to `AgentEvidence` rows downstream.
            let event = evidence::SandboxEvidenceEvent::TmpdirCleanupFailed {
                path: path.to_path_buf(),
                error: err.to_string(),
            };
            if let Some(sink) = evidence_sink {
                sink.record(event);
            } else {
                evidence::TracingSandboxEvidenceSink.record(event);
            }
        }
    }
}

fn build_command_from_spec(
    spec: CommandSpec,
    sandbox: Option<&ToolSandboxContext>,
    sandbox_runtime: Option<&SandboxRuntimeConfig>,
    network_access_override: Option<NetworkAccess>,
    evidence_sink: Option<&dyn evidence::SandboxEvidenceSink>,
) -> Result<BuiltCommand, String> {
    let sandbox_policy_cwd = spec.cwd.clone();
    let linux_sandbox_exe = resolve_linux_sandbox_exe(sandbox_runtime);
    let use_linux_sandbox_bwrap = sandbox_runtime
        .map(|config| config.use_linux_sandbox_bwrap)
        .unwrap_or_else(|| env_flag_enabled("LIBRA_USE_LINUX_SANDBOX_BWRAP"));
    let enforcement = resolve_sandbox_enforcement(sandbox_runtime)?;
    // Read `.libra/sandbox.toml` once and derive both deny-read paths
    // and the network restriction from the same parsed struct, so the
    // two never observe different file versions under a concurrent edit.
    let sandbox_config = load_sandbox_config_file(&sandbox_policy_cwd)?;
    let deny_read_paths =
        resolve_deny_read_paths_from(&sandbox_config, &sandbox_policy_cwd, sandbox_runtime);
    let fallback_seccomp_policy_path = resolve_seccomp_policy_path();
    let seccomp_policy_path = sandbox_runtime
        .and_then(|cfg| cfg.seccomp_policy_path.as_deref())
        .or(fallback_seccomp_policy_path.as_deref());
    // Apply any `.libra/sandbox.toml [sandbox.network]` restriction to
    // the policy BEFORE the transform reads it. This is tightening-only
    // (see `SandboxPolicy::with_network_restriction`): the config can
    // lock the workspace down but never widen the policy's reach, so the
    // security-critical transform derivation is left untouched.
    let config_network_access = sandbox_config.network_access()?;
    let policy = sandbox.map(|context| {
        network_access_override
            .as_ref()
            .map_or(context.policy.clone(), |access| {
                context.policy.with_network_access(access)
            })
    });
    let restricted_policy = match (&policy, &config_network_access) {
        (Some(context), Some(config_access)) => {
            Some(context.with_network_restriction(config_access))
        }
        _ => None,
    };
    let effective_policy = restricted_policy.as_ref().or(policy.as_ref());
    let manager = SandboxManager::new();
    let exec_env = manager
        .transform(SandboxTransformRequest {
            spec,
            policy: effective_policy,
            sandbox_policy_cwd: &sandbox_policy_cwd,
            linux_sandbox_exe: linux_sandbox_exe.as_ref(),
            use_linux_sandbox_bwrap,
            enforcement,
            deny_read_paths: &deny_read_paths,
            seccomp_policy_path,
        })
        .map_err(|err| {
            // Doc contract: `docs/development/commands/sandbox.md:143`, L162,
            // L373 — writable_root rejections AND enforcement
            // failures must surface as structured Evidence in
            // addition to the propagated error string. The default
            // `TracingSandboxEvidenceSink` keeps the existing log
            // shape; opt-in agent-runtime sinks fan out to
            // `AgentEvidence` rows.
            let event = match &err {
                runtime::SandboxTransformError::InvalidPolicy(
                    policy::SandboxPolicyError::DangerousWritableRoot { root, reason },
                ) => Some(evidence::SandboxEvidenceEvent::WritableRootRejected {
                    root: root.clone(),
                    reason: (*reason).to_string(),
                }),
                runtime::SandboxTransformError::EnforcementFailed { reason } => {
                    Some(evidence::SandboxEvidenceEvent::EnforcementFailed {
                        reason: reason.clone(),
                    })
                }
                runtime::SandboxTransformError::NetworkEnforcementFailed { reason } => {
                    Some(evidence::SandboxEvidenceEvent::NetworkEnforcementFailed {
                        reason: reason.clone(),
                    })
                }
                _ => None,
            };
            if let Some(event) = event {
                if let Some(sink) = evidence_sink
                    .or_else(|| sandbox_runtime.and_then(|c| c.evidence_sink.as_deref()))
                {
                    sink.record(event);
                } else {
                    evidence::TracingSandboxEvidenceSink.record(event);
                }
            }
            err.to_string()
        })?;
    let spawn_program = exec_env
        .command
        .first()
        .cloned()
        .unwrap_or_else(|| "<missing>".to_string());
    let allowlist_proxy_services = exec_env.allowlist_proxy_services.clone();
    let protected_mount_cleanup_paths = exec_env.protected_mount_cleanup_paths.clone();
    let spawn_cwd = exec_env.cwd.clone();
    let process_cwd = exec_env.spawn_cwd.clone();
    #[cfg(test)]
    let fallback_exec_env = exec_env.clone();
    let (command, timeout_ms) = exec_env.into_command()?;
    Ok(BuiltCommand {
        command,
        timeout_ms,
        allowlist_proxy_services,
        protected_mount_cleanup_paths,
        spawn_cwd,
        process_cwd,
        #[cfg(test)]
        exec_env: fallback_exec_env,
        spawn_program,
    })
}

fn spawn_shell_command(
    cmd: &mut tokio::process::Command,
    fallback_cwd: &Path,
) -> std::io::Result<tokio::process::Child> {
    let mut last_not_found = None;
    #[cfg(test)]
    let max_attempts = 20;
    #[cfg(not(test))]
    let max_attempts = 3;

    for attempt in 0..max_attempts {
        match cmd.spawn() {
            Ok(child) => return Ok(child),
            Err(error) if error.kind() == ErrorKind::NotFound && attempt + 1 < max_attempts => {
                last_not_found = Some(error);
                #[cfg(test)]
                {
                    let Some(_cwd_lock) = crate::utils::test::try_cwd_lock_guard() else {
                        break;
                    };
                    restore_process_cwd(fallback_cwd)?;
                }
                #[cfg(not(test))]
                restore_process_cwd(fallback_cwd)?;
                #[cfg(test)]
                std::thread::sleep(Duration::from_millis(10));
                std::thread::yield_now();
            }
            Err(error) => return Err(error),
        }
    }

    if let Some(error) = last_not_found {
        Err(error)
    } else {
        Err(io::Error::new(
            ErrorKind::NotFound,
            "failed to spawn command after cwd repair",
        ))
    }
}

#[cfg(test)]
async fn run_std_command_fallback(
    exec_env: ExecEnv,
    max_output_bytes: usize,
    timeout_override: Option<u64>,
) -> Result<SandboxExecOutput, String> {
    tokio::task::spawn_blocking(move || {
        run_std_command_fallback_blocking(exec_env, max_output_bytes, timeout_override)
    })
    .await
    .map_err(|error| format!("std fallback task join failed: {error}"))?
}

#[cfg(test)]
fn run_std_command_fallback_blocking(
    exec_env: ExecEnv,
    max_output_bytes: usize,
    timeout_override: Option<u64>,
) -> Result<SandboxExecOutput, String> {
    use std::io::{Seek, SeekFrom, Write};

    let (program, args) = exec_env
        .command
        .split_first()
        .ok_or_else(|| "missing command program".to_string())?;
    let canonical_cwd = exec_env
        .spawn_cwd
        .canonicalize()
        .unwrap_or_else(|_| exec_env.spawn_cwd.clone());
    let stdout_file = tempfile::tempfile()
        .map_err(|error| format!("create stdout tempfile for fallback: {error}"))?;
    let stderr_file = tempfile::tempfile()
        .map_err(|error| format!("create stderr tempfile for fallback: {error}"))?;
    let mut stdout_reader = stdout_file
        .try_clone()
        .map_err(|error| format!("clone stdout tempfile for fallback: {error}"))?;
    let mut stderr_reader = stderr_file
        .try_clone()
        .map_err(|error| format!("clone stderr tempfile for fallback: {error}"))?;

    let mut command = std::process::Command::new(program);
    command.args(args).current_dir(canonical_cwd);
    if exec_env.clear_env {
        command.env_clear();
    }
    command.envs(exec_env.env);
    if let Some(stdin) = exec_env.stdin {
        let mut file = tempfile::tempfile()
            .map_err(|error| format!("create stdin tempfile for fallback: {error}"))?;
        file.write_all(&stdin)
            .map_err(|error| format!("write stdin tempfile for fallback: {error}"))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|error| format!("rewind stdin tempfile for fallback: {error}"))?;
        command.stdin(Stdio::from(file));
    }
    command
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));

    let mut child = command
        .spawn()
        .map_err(|error| format!("spawn std fallback command `{program}`: {error}"))?;
    let timeout_dur = Duration::from_millis(timeout_override.unwrap_or(DEFAULT_TIMEOUT_MS));
    let start = std::time::Instant::now();
    let (exit_code, timed_out) = loop {
        match child
            .try_wait()
            .map_err(|error| format!("wait std fallback command `{program}`: {error}"))?
        {
            Some(status) => break (status.code().unwrap_or(-1), false),
            None if start.elapsed() >= timeout_dur => {
                let _ = child.kill();
                let _ = child.wait();
                break (TIMEOUT_EXIT_CODE, true);
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };

    let (mut stdout, stdout_truncated, stdout_incomplete) =
        read_limited_tempfile(&mut stdout_reader, max_output_bytes);
    let (mut stderr, stderr_truncated, stderr_incomplete) =
        read_limited_tempfile(&mut stderr_reader, max_output_bytes);
    if stdout_truncated {
        stdout.push_str("\n[stdout truncated]");
    }
    if stderr_truncated {
        stderr.push_str("\n[stderr truncated]");
    }
    if stdout_incomplete {
        stdout.push_str("\n[stdout stream incomplete]");
    }
    if stderr_incomplete {
        stderr.push_str("\n[stderr stream incomplete]");
    }

    Ok(SandboxExecOutput {
        exit_code,
        stdout,
        stderr,
        timed_out,
    })
}

#[cfg(test)]
fn read_limited_tempfile(
    file: &mut std::fs::File,
    max_output_bytes: usize,
) -> (String, bool, bool) {
    use std::io::{Read, Seek, SeekFrom};

    if file.seek(SeekFrom::Start(0)).is_err() {
        return (String::new(), false, true);
    }

    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut incomplete = false;
    let mut buf = [0_u8; 8192];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => {
                if bytes.len() < max_output_bytes {
                    let remaining = max_output_bytes - bytes.len();
                    let keep = read.min(remaining);
                    bytes.extend_from_slice(&buf[..keep]);
                    truncated |= keep < read;
                } else {
                    truncated = true;
                }
            }
            Err(_) => {
                incomplete = true;
                break;
            }
        }
    }

    (
        String::from_utf8_lossy(&bytes).into_owned(),
        truncated,
        incomplete,
    )
}

#[cfg(target_os = "linux")]
struct PreparedProtectedMountpoint {
    path: PathBuf,
    device: u64,
    inode: u64,
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct PreparedProtectedMountpoints {
    entries: Vec<PreparedProtectedMountpoint>,
}

#[cfg(target_os = "linux")]
impl PreparedProtectedMountpoints {
    fn prepare(paths: &[PathBuf]) -> Result<Self, String> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};

        let mut prepared = Self::default();
        for path in paths {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            if let Err(error) = builder.create(path) {
                let cleanup = prepared.cleanup();
                let mut message = if error.kind() == ErrorKind::AlreadyExists {
                    format!(
                        "protected sandbox mountpoint `{}` appeared while the command was being prepared; refusing to race with a concurrent filesystem change",
                        path.display()
                    )
                } else {
                    format!(
                        "failed to prepare protected sandbox mountpoint `{}`: {error}",
                        path.display()
                    )
                };
                if let Err(cleanup_error) = cleanup {
                    message.push_str(&format!("; additionally, {cleanup_error}"));
                }
                return Err(message);
            }

            let metadata = match fs::symlink_metadata(path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    let remove_error = fs::remove_dir(path).err();
                    let cleanup = prepared.cleanup();
                    let mut message = format!(
                        "failed to inspect protected sandbox mountpoint `{}` after creating it: {error}",
                        path.display()
                    );
                    if let Some(remove_error) = remove_error {
                        message.push_str(&format!(
                            "; failed to remove that mountpoint: {remove_error}"
                        ));
                    }
                    if let Err(cleanup_error) = cleanup {
                        message.push_str(&format!("; additionally, {cleanup_error}"));
                    }
                    return Err(message);
                }
            };
            prepared.entries.push(PreparedProtectedMountpoint {
                path: path.clone(),
                device: metadata.dev(),
                inode: metadata.ino(),
            });
        }
        Ok(prepared)
    }

    fn cleanup(&mut self) -> Result<(), String> {
        use std::os::unix::fs::MetadataExt;

        let mut failures = Vec::new();
        for mountpoint in self.entries.drain(..).rev() {
            let metadata = match fs::symlink_metadata(&mountpoint.path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => {
                    failures.push(format!("`{}`: {error}", mountpoint.path.display()));
                    continue;
                }
            };
            if !metadata.is_dir()
                || metadata.dev() != mountpoint.device
                || metadata.ino() != mountpoint.inode
            {
                failures.push(format!(
                    "`{}` changed identity while the sandboxed command ran",
                    mountpoint.path.display()
                ));
                continue;
            }
            match fs::remove_dir(&mountpoint.path) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => failures.push(format!("`{}`: {error}", mountpoint.path.display())),
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "failed to clean protected sandbox mountpoints: {}",
                failures.join("; ")
            ))
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for PreparedProtectedMountpoints {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            tracing::error!(%error, "protected sandbox mountpoint cleanup failed");
        }
    }
}

#[cfg(not(target_os = "linux"))]
struct PreparedProtectedMountpoints;

#[cfg(not(target_os = "linux"))]
impl PreparedProtectedMountpoints {
    fn prepare(_paths: &[PathBuf]) -> Result<Self, String> {
        Ok(Self)
    }

    fn cleanup(&mut self) -> Result<(), String> {
        Ok(())
    }
}

struct BuiltCommand {
    command: tokio::process::Command,
    timeout_ms: Option<u64>,
    allowlist_proxy_services: Option<Vec<NetworkService>>,
    protected_mount_cleanup_paths: Vec<PathBuf>,
    spawn_cwd: PathBuf,
    process_cwd: PathBuf,
    #[cfg(test)]
    exec_env: ExecEnv,
    spawn_program: String,
}

fn repair_missing_process_cwd(fallback: &Path) -> Result<(), String> {
    if std::env::current_dir().is_ok() {
        return Ok(());
    }

    restore_process_cwd(fallback).map_err(|error| error.to_string())
}

fn restore_process_cwd(fallback: &Path) -> std::io::Result<()> {
    let fallback = fallback
        .canonicalize()
        .unwrap_or_else(|_| fallback.to_path_buf());
    std::env::set_current_dir(&fallback).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
            "failed to restore process current directory to `{}` before spawning shell: {error}",
            fallback.display()
            ),
        )
    })
}

async fn start_allowlist_proxy_if_needed(
    command: &mut tokio::process::Command,
    services: Option<Vec<NetworkService>>,
    evidence_sink: Option<std::sync::Arc<dyn evidence::SandboxEvidenceSink>>,
) -> Result<Option<proxy_runtime::RunningAllowlistProxy>, String> {
    let Some(services) = services else {
        return Ok(None);
    };
    let proxy = if evidence_sink.is_some() {
        proxy_runtime::spawn_allowlist_http_proxy_with_evidence(services, evidence_sink).await?
    } else {
        proxy_runtime::spawn_allowlist_http_proxy(services).await?
    };
    inject_allowlist_proxy_env(command, &proxy);
    Ok(Some(proxy))
}

fn inject_allowlist_proxy_env(
    command: &mut tokio::process::Command,
    proxy: &proxy_runtime::RunningAllowlistProxy,
) {
    let proxy_url = proxy.local_http_proxy_url();
    for name in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        command.env(name, &proxy_url);
    }
    // Force proxy-aware tools through Libra's allowlist proxy even when the
    // parent shell has broad NO_PROXY defaults for loopback or local domains.
    command.env("NO_PROXY", "");
    command.env("no_proxy", "");
    command.env("LIBRA_SANDBOX_ALLOWLIST_PROXY", proxy_url);
}

#[derive(Debug, Default, Deserialize)]
struct SandboxConfigFile {
    #[serde(default)]
    deny_read: Vec<PathBuf>,
    /// Optional `[sandbox]` table. Network access lives under
    /// `[sandbox.network]` per `docs/development/commands/sandbox.md` §7.3,
    /// while `deny_read` stays at the file root for backward
    /// compatibility with existing configs.
    #[serde(default)]
    sandbox: Option<SandboxConfigSection>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SandboxConfigSection {
    #[serde(default)]
    network: Option<SandboxNetworkConfig>,
}

/// The `[sandbox.network]` section of `.libra/sandbox.toml`.
///
/// ```toml
/// [sandbox.network]
/// mode = "allowlist"  # denied | allowlist | full; default denied
///
/// [[sandbox.network.services]]
/// host = "registry.npmjs.org"
/// ports = [443]
/// ```
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SandboxNetworkConfig {
    #[serde(default)]
    mode: SandboxNetworkMode,
    /// Allowlist entries; only consulted when `mode = "allowlist"`.
    #[serde(default)]
    services: Vec<NetworkService>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SandboxNetworkMode {
    #[default]
    Denied,
    Allowlist,
    Full,
}

impl SandboxConfigFile {
    /// Translate the `[sandbox.network]` section into a
    /// [`NetworkAccess`], validating allowlist services per
    /// `docs/development/commands/sandbox.md` §7.3 (no bare `*` / empty host,
    /// no implicit high-sensitivity-port grants). Returns `Ok(None)`
    /// when no `[sandbox.network]` section is present so callers leave
    /// the policy-derived access untouched.
    ///
    /// The returned access is only ever applied as a *tightening*
    /// constraint via [`SandboxPolicy::with_network_restriction`], so a
    /// `mode = "full"` here can never loosen a more-restrictive policy.
    fn network_access(&self) -> Result<Option<NetworkAccess>, String> {
        let Some(network) = self
            .sandbox
            .as_ref()
            .and_then(|section| section.network.as_ref())
        else {
            return Ok(None);
        };

        let access = match network.mode {
            SandboxNetworkMode::Denied => NetworkAccess::Denied,
            SandboxNetworkMode::Full => NetworkAccess::Full,
            SandboxNetworkMode::Allowlist => {
                for service in &network.services {
                    service.validate().map_err(|error| {
                        format!(
                            "invalid `[sandbox.network]` service '{}' in .libra/sandbox.toml: {error}",
                            service.host
                        )
                    })?;
                }
                NetworkAccess::Allowlist {
                    services: network.services.clone(),
                }
            }
        };
        Ok(Some(access))
    }
}

/// Load `.libra/sandbox.toml` and resolve its deny-read paths in one
/// call. Production code (`build_command_from_spec`) loads the file
/// once and calls [`resolve_deny_read_paths_from`] directly so the
/// network section is derived from the same parsed struct; this
/// convenience wrapper is retained for the focused deny-read tests,
/// which also exercise the load/parse-error path.
#[cfg(test)]
fn resolve_deny_read_paths(
    cwd: &Path,
    sandbox_runtime: Option<&SandboxRuntimeConfig>,
) -> Result<Vec<PathBuf>, String> {
    let config = load_sandbox_config_file(cwd)?;
    Ok(resolve_deny_read_paths_from(&config, cwd, sandbox_runtime))
}

/// Resolve deny-read paths from an already-loaded config so the
/// `.libra/sandbox.toml` file is read once per command setup (the
/// network access is derived from the same parsed struct). Reading it
/// twice could otherwise observe two different file versions if the
/// file is edited concurrently.
fn resolve_deny_read_paths_from(
    config: &SandboxConfigFile,
    cwd: &Path,
    sandbox_runtime: Option<&SandboxRuntimeConfig>,
) -> Vec<PathBuf> {
    let mut paths = config.deny_read.clone();
    if let Some(runtime) = sandbox_runtime {
        paths.extend(runtime.deny_read_paths.iter().cloned());
    }

    let mut resolved = Vec::with_capacity(paths.len());
    for path in paths {
        push_unique_path(&mut resolved, resolve_deny_read_path(cwd, path));
    }
    resolved
}

fn load_sandbox_config_file(cwd: &Path) -> Result<SandboxConfigFile, String> {
    let path = sandbox_config_path(cwd);
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(SandboxConfigFile::default());
        }
        Err(error) => {
            return Err(format!(
                "failed to read sandbox config `{}`: {error}",
                path.display()
            ));
        }
    };

    toml::from_str::<SandboxConfigFile>(&contents).map_err(|error| {
        format!(
            "failed to parse sandbox config `{}`: {error}",
            path.display()
        )
    })
}

fn sandbox_config_path(cwd: &Path) -> PathBuf {
    crate::utils::util::try_get_storage_path(Some(cwd.to_path_buf()))
        .unwrap_or_else(|_| cwd.join(crate::utils::util::ROOT_DIR))
        .join(SANDBOX_CONFIG_FILE)
}

fn resolve_deny_read_path(cwd: &Path, path: PathBuf) -> PathBuf {
    let path_text = path.to_string_lossy();
    if path_text == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    } else if let Some(rest) = path_text.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }

    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn prefer_strict_sandbox_fallback_reason(
    sandbox: Option<&ToolSandboxContext>,
    sandbox_runtime: Option<&SandboxRuntimeConfig>,
) -> Result<Option<String>, String> {
    let Some(context) = sandbox else {
        return Ok(None);
    };
    if context.permissions.requires_escalated_permissions()
        || !sandbox_policy_needs_internal_backend(&context.policy)
    {
        return Ok(None);
    }

    let enforcement = resolve_sandbox_enforcement(sandbox_runtime)?;
    if enforcement != SandboxEnforcement::PreferStrict {
        return Ok(None);
    }

    #[cfg(target_os = "linux")]
    {
        let linux_sandbox_exe = resolve_linux_sandbox_exe(sandbox_runtime);
        if linux_sandbox_exe.is_none() && locate_bwrap_binary_for_prefer_strict().is_none() {
            return Ok(Some(
                "sandbox enforcement is prefer_strict, but Linux sandbox helper is not configured and the built-in bwrap sandbox is not available; approve to run outside Libra's internal sandbox".to_string(),
            ));
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        return Ok(Some(
            "sandbox enforcement is prefer_strict, but this platform has no supported internal sandbox backend for the selected policy; approve to run outside Libra's internal sandbox".to_string(),
        ));
    }

    Ok(None)
}

fn resolve_linux_sandbox_exe(sandbox_runtime: Option<&SandboxRuntimeConfig>) -> Option<PathBuf> {
    let candidate = sandbox_runtime
        .and_then(|config| config.linux_sandbox_exe.clone())
        .or_else(|| std::env::var_os("LIBRA_LINUX_SANDBOX_EXE").map(PathBuf::from));

    #[cfg(target_os = "linux")]
    {
        candidate.filter(|path| is_executable_file(path))
    }

    #[cfg(not(target_os = "linux"))]
    {
        candidate.filter(|path| path.is_file())
    }
}

#[cfg(target_os = "linux")]
fn locate_bwrap_binary_for_prefer_strict() -> Option<PathBuf> {
    if let Some(override_path) = std::env::var_os("LIBRA_BWRAP_BINARY") {
        let path = PathBuf::from(override_path);
        if path.is_absolute() && is_executable_file(&path) {
            return Some(path);
        }
        return None;
    }

    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join("bwrap");
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(target_os = "linux")]
#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return false,
    };
    if !metadata.is_file() {
        return false;
    }
    metadata.permissions().mode() & 0o111 != 0
}

fn sandbox_policy_needs_internal_backend(policy: &SandboxPolicy) -> bool {
    matches!(
        policy,
        SandboxPolicy::ReadOnly | SandboxPolicy::WorkspaceWrite { .. }
    )
}

fn resolve_sandbox_enforcement(
    sandbox_runtime: Option<&SandboxRuntimeConfig>,
) -> Result<SandboxEnforcement, String> {
    if let Some(config) = sandbox_runtime {
        return Ok(config.enforcement);
    }

    parse_sandbox_enforcement_env(std::env::var(SANDBOX_ENFORCEMENT_ENV).ok().as_deref())
}

fn parse_sandbox_enforcement_env(value: Option<&str>) -> Result<SandboxEnforcement, String> {
    value
        .map(|value| {
            value
                .parse::<SandboxEnforcement>()
                .map_err(|error| error.to_string())
        })
        .transpose()
        .map(|value| value.unwrap_or_default())
}

fn env_flag_enabled(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| {
        let value = value.to_string_lossy().to_ascii_lowercase();
        matches!(value.as_str(), "1" | "true" | "yes" | "on")
    })
}

/// Read `LIBRA_SECCOMP_POLICY` and return it as a `PathBuf` when
/// set to a non-empty value. Used by `build_command_from_spec` as
/// a fallback when `SandboxRuntimeConfig::seccomp_policy_path`
/// is `None`, so users can opt into seccomp without editing
/// in-process config. An empty / whitespace-only value is
/// treated as unset.
fn resolve_seccomp_policy_env() -> Option<PathBuf> {
    let raw = std::env::var(SANDBOX_SECCOMP_POLICY_ENV).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

/// Resolve seccomp policy using explicit env override first,
/// then defaulting to `~/.libra/seccomp.bpf` when the env
/// var is not set at all. Empty / whitespace-only env values
/// explicitly disable seccomp and therefore block defaulting.
///
/// On Linux, if the env var is unset and the default file is
/// missing, the v0.17.770
/// [`seccomp_compile::ensure_compiled_seccomp_policy_at`] helper
/// materialises it from the bundled JSON template before the
/// `is_file()` filter runs. This means a fresh Linux install
/// gets the default seccomp policy with zero operator action —
/// the file is created on first dispatch and reused thereafter.
/// Compile failures (unknown host arch, malformed bundled JSON)
/// downgrade to None so the legacy "no fallback" behaviour
/// kicks in; we never block a sandbox dispatch on a seccomp
/// compile error.
fn resolve_seccomp_policy_path() -> Option<PathBuf> {
    if std::env::var_os(SANDBOX_SECCOMP_POLICY_ENV).is_some() {
        return resolve_seccomp_policy_env();
    }

    let default_path = dirs::home_dir().map(|home| home.join(DEFAULT_SECCOMP_POLICY_PATH))?;

    #[cfg(target_os = "linux")]
    {
        if !default_path.is_file()
            && let Err(err) = seccomp_compile::ensure_compiled_seccomp_policy_at(&default_path)
        {
            tracing::warn!(
                error = %err,
                path = %default_path.display(),
                "failed to materialise default seccomp policy at first launch; \
                 sandbox dispatch will run without seccomp enforcement",
            );
            return None;
        }
    }

    Some(default_path).filter(|path| path.is_file())
}

async fn request_exec_approval(
    ctx: &ToolApprovalContext,
    request: ExecApprovalPrompt<'_>,
) -> ReviewDecision {
    let ExecApprovalPrompt {
        call_id,
        command,
        cwd,
        reason,
        sandbox_policy,
        sandbox_permissions,
        is_retry,
        requested_network_access,
    } = request;
    let (sandbox_label, network_access, writable_roots) = approval_request_context(
        sandbox_policy,
        cwd,
        sandbox_permissions,
        is_retry,
        requested_network_access.as_ref(),
    );
    let keys = ApprovalCacheKeys::shell(command, cwd, sandbox_permissions);
    let cache_disabled_reason = ctx.cache_policy.disabled_reason_for_command(command);
    request_cached_approval_with_cache_keys(
        ctx,
        keys,
        cache_disabled_reason,
        |response_tx, cache_disabled_reason| ExecApprovalRequest {
            call_id: call_id.to_string(),
            command: command.to_string(),
            cwd: cwd.to_path_buf(),
            reason,
            is_retry,
            sandbox_label,
            network_access,
            writable_roots,
            cache_disabled_reason,
            response_tx,
        },
    )
    .await
}

async fn request_uncached_exec_approval(
    ctx: &ToolApprovalContext,
    request: ExecApprovalPrompt<'_>,
    cache_disabled_reason: Option<String>,
) -> ReviewDecision {
    let ExecApprovalPrompt {
        call_id,
        command,
        cwd,
        reason,
        sandbox_policy,
        sandbox_permissions,
        is_retry,
        requested_network_access,
    } = request;
    let (sandbox_label, network_access, writable_roots) = approval_request_context(
        sandbox_policy,
        cwd,
        sandbox_permissions,
        is_retry,
        requested_network_access.as_ref(),
    );
    let (response_tx, response_rx) = oneshot::channel();
    if ctx
        .request_tx
        .send(ExecApprovalRequest {
            call_id: call_id.to_string(),
            command: command.to_string(),
            cwd: cwd.to_path_buf(),
            reason,
            is_retry,
            sandbox_label,
            network_access,
            writable_roots,
            cache_disabled_reason,
            response_tx,
        })
        .is_err()
    {
        return ReviewDecision::Denied;
    }

    response_rx.await.unwrap_or(ReviewDecision::Denied)
}

struct ExecApprovalPrompt<'a> {
    call_id: &'a str,
    command: &'a str,
    cwd: &'a Path,
    reason: Option<String>,
    sandbox_policy: Option<&'a SandboxPolicy>,
    sandbox_permissions: SandboxPermissions,
    is_retry: bool,
    requested_network_access: Option<NetworkAccess>,
}

fn approval_request_context(
    sandbox_policy: Option<&SandboxPolicy>,
    cwd: &Path,
    sandbox_permissions: SandboxPermissions,
    is_retry: bool,
    requested_network_access: Option<&NetworkAccess>,
) -> (String, NetworkAccess, Vec<PathBuf>) {
    let resolved_network_access = requested_network_access.cloned().unwrap_or_else(|| {
        shell_policy_network_access(sandbox_policy, sandbox_permissions, is_retry)
    });

    if sandbox_permissions.requires_escalated_permissions() || is_retry {
        return (
            "outside sandbox".to_string(),
            NetworkAccess::Full,
            Vec::new(),
        );
    }

    match sandbox_policy {
        Some(SandboxPolicy::DangerFullAccess) => (
            "danger-full-access".to_string(),
            resolved_network_access,
            Vec::new(),
        ),
        Some(SandboxPolicy::ExternalSandbox { .. }) => (
            "external-sandbox".to_string(),
            resolved_network_access,
            Vec::new(),
        ),
        Some(SandboxPolicy::ReadOnly) => {
            ("read-only".to_string(), resolved_network_access, Vec::new())
        }
        Some(policy @ SandboxPolicy::WorkspaceWrite { .. }) => (
            "workspace-write".to_string(),
            resolved_network_access,
            policy
                .get_writable_roots_with_cwd(cwd)
                .into_iter()
                .map(|root| root.root)
                .collect(),
        ),
        None => (
            "no sandbox".to_string(),
            resolved_network_access,
            Vec::new(),
        ),
    }
}

fn shell_policy_network_access(
    sandbox_policy: Option<&SandboxPolicy>,
    sandbox_permissions: SandboxPermissions,
    is_retry: bool,
) -> NetworkAccess {
    if sandbox_permissions.requires_escalated_permissions() || is_retry {
        return NetworkAccess::Full;
    }

    match sandbox_policy {
        Some(SandboxPolicy::DangerFullAccess) => NetworkAccess::Full,
        Some(SandboxPolicy::ExternalSandbox { network_access }) => network_access.clone(),
        Some(SandboxPolicy::ReadOnly) => NetworkAccess::Denied,
        Some(SandboxPolicy::WorkspaceWrite { network_access, .. }) => network_access.clone(),
        None => NetworkAccess::Full,
    }
}

fn requested_network_access_upgrade(
    sandbox_policy: Option<&SandboxPolicy>,
    sandbox_permissions: SandboxPermissions,
    cwd: &Path,
    is_retry: bool,
) -> Result<Option<NetworkAccess>, String> {
    if !matches!(
        sandbox_policy,
        Some(SandboxPolicy::ExternalSandbox { .. } | SandboxPolicy::WorkspaceWrite { .. })
    ) {
        return Ok(None);
    }

    let current_network_access =
        shell_policy_network_access(sandbox_policy, sandbox_permissions, is_retry);
    let config_network_access = load_sandbox_config_file(cwd)?.network_access()?;

    let Some(config_network_access) = config_network_access else {
        return Ok(None);
    };

    if config_network_access.restrictiveness_rank() > current_network_access.restrictiveness_rank()
    {
        return Ok(Some(config_network_access));
    }

    Ok(None)
}

pub(crate) fn load_sandbox_config_network_access(
    cwd: &Path,
) -> Result<Option<NetworkAccess>, String> {
    load_sandbox_config_file(cwd)?.network_access()
}

pub fn shell_approval_key(
    command: &str,
    cwd: &Path,
    sandbox_permissions: SandboxPermissions,
) -> String {
    shell_approval_key_with_scope(
        command,
        cwd,
        sandbox_permissions,
        ApprovalScope::Session,
        ApprovalSensitivityTier::Strict,
    )
}

pub fn shell_approval_key_with_scope(
    command: &str,
    cwd: &Path,
    sandbox_permissions: SandboxPermissions,
    scope: ApprovalScope,
    sensitivity_tier: ApprovalSensitivityTier,
) -> String {
    let material = [
        format!("sensitivity_tier={}", sensitivity_tier.as_str()),
        format!("scope={}", scope.as_str()),
        "tool_name=shell".to_string(),
        format!(
            "canonical_args={}",
            canonical_shell_args_for_tier(command, sensitivity_tier)
        ),
        format!("cwd={}", cwd.display()),
        format!(
            "sandbox_scope={}",
            match sandbox_permissions {
                SandboxPermissions::UseDefault => "use_default",
                SandboxPermissions::RequireEscalated => "require_escalated",
            }
        ),
        "target_path=".to_string(),
        "protected_branch=".to_string(),
        "source_slug=".to_string(),
        "network_domain=".to_string(),
        "workspace_id=".to_string(),
    ]
    .join("\n");

    hex::encode(digest(&SHA256, material.as_bytes()).as_ref())
}

fn canonical_shell_args_for_tier(
    command: &str,
    sensitivity_tier: ApprovalSensitivityTier,
) -> String {
    let mut parts = shlex::split(command)
        .filter(|parts| !parts.is_empty())
        .unwrap_or_else(|| {
            command
                .split_whitespace()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        });
    if parts.is_empty() {
        return String::new();
    }

    let argv0 = parts.remove(0);
    let mut flags = Vec::new();
    let mut args = Vec::new();
    for part in parts {
        if part.starts_with('-') {
            flags.push(part);
        } else {
            args.push(part);
        }
    }
    flags.sort();

    match sensitivity_tier {
        ApprovalSensitivityTier::Strict => format!(
            "argv0={};flags={};args={}",
            length_prefixed_list(&[argv0]),
            length_prefixed_list(&flags),
            length_prefixed_list(&args)
        ),
        ApprovalSensitivityTier::Directory => format!(
            "argv0={};flags={};args=<same-cwd>",
            length_prefixed_list(&[argv0]),
            length_prefixed_list(&flags)
        ),
        ApprovalSensitivityTier::Pattern => {
            let arg_patterns = args
                .iter()
                .map(|arg| shell_arg_pattern(arg))
                .collect::<Vec<_>>();
            format!(
                "argv0={};flags={};arg_patterns={}",
                length_prefixed_list(&[argv0]),
                length_prefixed_list(&flags),
                length_prefixed_list(&arg_patterns)
            )
        }
    }
}

fn length_prefixed_list(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("{}:{value}", value.len()))
        .collect::<Vec<_>>()
        .join(",")
}

fn shell_arg_pattern(value: &str) -> String {
    if value.parse::<f64>().is_ok() {
        return "number".to_string();
    }
    if value.contains('/') || value.starts_with('.') || value.starts_with('~') {
        return "path".to_string();
    }
    if value.contains('*') || value.contains('?') || value.contains('[') {
        return "glob".to_string();
    }
    "value".to_string()
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ExecApprovalRequirement {
    Skip { bypass_sandbox: bool },
    NeedsApproval { reason: Option<String> },
    Forbidden { reason: String },
}

fn shell_exec_approval_requirement(
    policy: AskForApproval,
    sandbox_policy: Option<&SandboxPolicy>,
    command: &str,
    sandbox_permissions: SandboxPermissions,
    safety_decision: Option<&SafetyDecision>,
) -> ExecApprovalRequirement {
    if let Some(decision) = safety_decision {
        match decision.disposition {
            SafetyDisposition::Deny => {
                return ExecApprovalRequirement::Forbidden {
                    reason: shell_safety_decision_reason("rejected", decision),
                };
            }
            SafetyDisposition::NeedsHuman => {
                return if matches!(policy, AskForApproval::Never) {
                    ExecApprovalRequirement::Forbidden {
                        reason: shell_safety_decision_reason(
                            "requires human approval but approval policy is never",
                            decision,
                        ),
                    }
                } else {
                    ExecApprovalRequirement::NeedsApproval {
                        reason: Some(shell_safety_decision_reason("needs review", decision)),
                    }
                };
            }
            SafetyDisposition::Allow if !sandbox_permissions.requires_escalated_permissions() => {
                return ExecApprovalRequirement::Skip {
                    bypass_sandbox: false,
                };
            }
            SafetyDisposition::Allow => {}
        }
    }

    if !sandbox_permissions.requires_escalated_permissions()
        && command_safety::is_known_safe_shell_command(command)
    {
        return ExecApprovalRequirement::Skip {
            bypass_sandbox: false,
        };
    }

    let runtime_sandbox_is_weak = cfg!(windows)
        && sandbox_policy.is_some_and(|policy| matches!(policy, SandboxPolicy::ReadOnly));
    if command_safety::shell_command_might_be_dangerous(command) || runtime_sandbox_is_weak {
        return if matches!(policy, AskForApproval::Never) {
            ExecApprovalRequirement::Forbidden {
                reason: "dangerous command rejected by approval policy".to_string(),
            }
        } else {
            ExecApprovalRequirement::NeedsApproval { reason: None }
        };
    }

    match policy {
        AskForApproval::Never | AskForApproval::OnFailure => ExecApprovalRequirement::Skip {
            bypass_sandbox: false,
        },
        AskForApproval::UnlessTrusted => ExecApprovalRequirement::NeedsApproval { reason: None },
        AskForApproval::OnRequest => match sandbox_policy {
            Some(SandboxPolicy::DangerFullAccess | SandboxPolicy::ExternalSandbox { .. })
            | None => ExecApprovalRequirement::Skip {
                bypass_sandbox: false,
            },
            Some(SandboxPolicy::ReadOnly | SandboxPolicy::WorkspaceWrite { .. }) => {
                if sandbox_permissions.requires_escalated_permissions() {
                    ExecApprovalRequirement::NeedsApproval { reason: None }
                } else {
                    ExecApprovalRequirement::Skip {
                        bypass_sandbox: false,
                    }
                }
            }
        },
    }
}

fn shell_safety_decision_reason(prefix: &str, decision: &SafetyDecision) -> String {
    format!(
        "shell safety {prefix}: rule={} blast_radius={} reason={}",
        decision.rule_name, decision.blast_radius, decision.reason
    )
}

pub fn approval_required(policy: AskForApproval, sandbox_policy: Option<&SandboxPolicy>) -> bool {
    match policy {
        AskForApproval::Never | AskForApproval::OnFailure => false,
        AskForApproval::OnRequest => sandbox_policy.is_some_and(|policy| {
            !matches!(
                policy,
                SandboxPolicy::DangerFullAccess | SandboxPolicy::ExternalSandbox { .. }
            )
        }),
        AskForApproval::UnlessTrusted => true,
    }
}

fn should_bypass_approval(policy: AskForApproval, already_approved: bool) -> bool {
    if already_approved {
        return true;
    }
    matches!(policy, AskForApproval::Never)
}

fn wants_no_sandbox_approval(policy: AskForApproval) -> bool {
    !matches!(policy, AskForApproval::Never | AskForApproval::OnRequest)
}

fn build_denial_reason_from_output(_output: &SandboxExecOutput) -> String {
    "command failed; retry without sandbox?".to_string()
}

fn is_likely_sandbox_denied(output: &SandboxExecOutput) -> bool {
    if output.exit_code == 0 || output.timed_out {
        return false;
    }

    let has_sandbox_keyword = [&output.stderr, &output.stdout].into_iter().any(|section| {
        let lower = section.to_ascii_lowercase();
        SANDBOX_DENIED_KEYWORDS
            .iter()
            .any(|needle| lower.contains(needle))
    });
    if has_sandbox_keyword {
        return true;
    }

    !QUICK_REJECT_EXIT_CODES.contains(&output.exit_code)
}

async fn drain_reader(
    mut reader: impl AsyncReadExt + Unpin,
    max_bytes: usize,
    state: Arc<Mutex<StreamState>>,
) {
    let mut tmp = [0u8; 8192];
    loop {
        match reader.read(&mut tmp).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut guard = state.lock().await;
                append_chunk(&mut guard, &tmp[..n], max_bytes);
            }
        }
    }
}

fn append_chunk(state: &mut StreamState, chunk: &[u8], max_bytes: usize) {
    let remaining = max_bytes.saturating_sub(state.bytes.len());
    let to_take = remaining.min(chunk.len());
    if to_take > 0 {
        state.bytes.extend_from_slice(&chunk[..to_take]);
    }
    if to_take < chunk.len() {
        state.truncated = true;
    }
}

async fn collect_stream(
    mut task: tokio::task::JoinHandle<()>,
    state: Arc<Mutex<StreamState>>,
) -> (String, bool, bool) {
    let completed = tokio::time::timeout(STREAM_DRAIN_TIMEOUT, &mut task)
        .await
        .is_ok();
    if !completed {
        task.abort();
        let _ = task.await;
    }

    let snapshot = state.lock().await.clone();
    (
        String::from_utf8_lossy(&snapshot.bytes).into_owned(),
        snapshot.truncated,
        !completed,
    )
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use serial_test::serial;
    use tokio::sync::mpsc::error::TryRecvError;

    use super::*;
    use crate::utils::test::ScopedEnvVar;

    #[test]
    fn sandbox_enforcement_env_defaults_to_best_effort() {
        let enforcement =
            parse_sandbox_enforcement_env(None).expect("missing env should use the default");

        assert_eq!(enforcement, SandboxEnforcement::BestEffort);
    }

    #[test]
    fn sandbox_enforcement_env_accepts_required() {
        let enforcement =
            parse_sandbox_enforcement_env(Some("required")).expect("required should parse");

        assert_eq!(enforcement, SandboxEnforcement::Required);
    }

    #[test]
    fn sandbox_enforcement_env_rejects_unknown_values() {
        let error = parse_sandbox_enforcement_env(Some("strict"))
            .expect_err("unknown enforcement values must fail command construction");

        assert_eq!(
            error,
            "invalid sandbox enforcement 'strict'; expected one of: required, prefer_strict, best_effort"
        );
    }

    /// `LIBRA_SECCOMP_POLICY` env var resolves to a `PathBuf`
    /// when set to a non-empty value; whitespace-only and unset
    /// both yield `None` so users can clear the policy by
    /// `unset LIBRA_SECCOMP_POLICY` rather than needing to set
    /// an empty string sentinel. Pins the contract that env-var
    /// seccomp opt-in is the lowest-friction path for users who
    /// don't customise `SandboxRuntimeConfig` directly.
    #[cfg_attr(target_os = "linux", serial)]
    #[test]
    fn seccomp_policy_env_resolves_path_only_when_non_empty() {
        // SAFETY: test-only env mutation.
        let prior = std::env::var_os(SANDBOX_SECCOMP_POLICY_ENV);
        let _policy = match prior {
            Some(value) => Some(ScopedEnvVar::set(SANDBOX_SECCOMP_POLICY_ENV, value)),
            None => {
                // SAFETY: test-only env cleanup before running the assertion.
                unsafe {
                    std::env::remove_var(SANDBOX_SECCOMP_POLICY_ENV);
                }
                None
            }
        };
        unsafe {
            std::env::remove_var(SANDBOX_SECCOMP_POLICY_ENV);
        }
        assert!(resolve_seccomp_policy_env().is_none(), "unset env → None");

        unsafe {
            std::env::set_var(SANDBOX_SECCOMP_POLICY_ENV, "   ");
        }
        assert!(
            resolve_seccomp_policy_env().is_none(),
            "whitespace-only env → None so a stale `export VAR=' '` doesn't accidentally enable seccomp",
        );

        unsafe {
            std::env::set_var(SANDBOX_SECCOMP_POLICY_ENV, "/etc/libra/seccomp.bpf");
        }
        assert_eq!(
            resolve_seccomp_policy_env().as_deref(),
            Some(std::path::Path::new("/etc/libra/seccomp.bpf")),
        );
    }

    #[cfg_attr(target_os = "linux", serial)]
    #[test]
    fn seccomp_policy_path_falls_back_to_default_and_obeys_explicit_disable() {
        let temp = tempfile::tempdir().expect("tempdir for default seccomp path test");
        let _home = ScopedEnvVar::set("HOME", temp.path());
        let policy_path = temp.path().join(".libra").join("seccomp.bpf");
        let _ = std::fs::create_dir_all(policy_path.parent().expect("policy parent dir exists"));
        std::fs::write(&policy_path, b"placeholder bpf bytes").expect("write placeholder bpf");

        let prior = std::env::var_os(SANDBOX_SECCOMP_POLICY_ENV);
        unsafe {
            std::env::remove_var(SANDBOX_SECCOMP_POLICY_ENV);
        }
        struct EnvRestore {
            key: &'static str,
            value: Option<std::ffi::OsString>,
        }
        impl Drop for EnvRestore {
            fn drop(&mut self) {
                unsafe {
                    if let Some(val) = &self.value {
                        std::env::set_var(self.key, val);
                    } else {
                        std::env::remove_var(self.key);
                    }
                }
            }
        }
        let _restore = EnvRestore {
            key: SANDBOX_SECCOMP_POLICY_ENV,
            value: prior,
        };
        assert_eq!(
            resolve_seccomp_policy_path().as_deref(),
            Some(policy_path.as_path()),
            "default policy path should be used when env var is unset",
        );

        let _policy_explicit = ScopedEnvVar::set(
            SANDBOX_SECCOMP_POLICY_ENV,
            policy_path.to_string_lossy().to_string(),
        );
        assert_eq!(
            resolve_seccomp_policy_path().as_deref(),
            Some(policy_path.as_path()),
            "explicit env path should be preferred",
        );

        let _policy_disabled = ScopedEnvVar::set(SANDBOX_SECCOMP_POLICY_ENV, " ");
        assert!(
            resolve_seccomp_policy_path().is_none(),
            "whitespace env value should disable seccomp even when default path exists",
        );
    }

    #[test]
    fn sandbox_runtime_config_overrides_enforcement_env_resolution() {
        let config = SandboxRuntimeConfig {
            enforcement: SandboxEnforcement::Required,
            ..SandboxRuntimeConfig::default()
        };

        let enforcement = resolve_sandbox_enforcement(Some(&config))
            .expect("runtime config enforcement should be accepted");

        assert_eq!(enforcement, SandboxEnforcement::Required);
    }

    #[test]
    fn sandbox_config_absent_yields_empty_deny_read_paths() {
        let temp = tempfile::tempdir().expect("tempdir for sandbox config");

        let paths =
            resolve_deny_read_paths(temp.path(), None).expect("missing config should be accepted");

        assert!(paths.is_empty());
    }

    #[test]
    fn sandbox_config_deny_read_paths_resolve_relative_and_absolute_entries() {
        let temp = tempfile::tempdir().expect("tempdir for sandbox config");
        let libra_dir = temp.path().join(crate::utils::util::ROOT_DIR);
        std::fs::create_dir_all(&libra_dir).expect("create .libra dir");
        std::fs::write(
            libra_dir.join(SANDBOX_CONFIG_FILE),
            r#"deny_read = ["relative/secrets", "/var/secret-token"]"#,
        )
        .expect("write sandbox config");

        let paths = resolve_deny_read_paths(temp.path(), None).expect("config should parse");

        assert_eq!(
            paths,
            vec![
                temp.path().join("relative/secrets"),
                PathBuf::from("/var/secret-token"),
            ]
        );
    }

    #[test]
    fn sandbox_runtime_config_appends_deny_read_paths() {
        let temp = tempfile::tempdir().expect("tempdir for sandbox config");
        let config = SandboxRuntimeConfig {
            deny_read_paths: vec![PathBuf::from("/runtime/secret")],
            ..SandboxRuntimeConfig::default()
        };

        let paths = resolve_deny_read_paths(temp.path(), Some(&config))
            .expect("runtime config paths should resolve");

        assert_eq!(paths, vec![PathBuf::from("/runtime/secret")]);
    }

    #[test]
    fn sandbox_config_parse_errors_are_user_facing() {
        let temp = tempfile::tempdir().expect("tempdir for sandbox config");
        let libra_dir = temp.path().join(crate::utils::util::ROOT_DIR);
        std::fs::create_dir_all(&libra_dir).expect("create .libra dir");
        std::fs::write(libra_dir.join(SANDBOX_CONFIG_FILE), "deny_read = [")
            .expect("write invalid sandbox config");

        let error = resolve_deny_read_paths(temp.path(), None)
            .expect_err("invalid TOML should be reported");

        assert!(
            error.contains("failed to parse sandbox config"),
            "unexpected error: {error}"
        );
    }

    fn parse_network_access(toml: &str) -> Result<Option<NetworkAccess>, String> {
        toml::from_str::<SandboxConfigFile>(toml)
            .map_err(|error| error.to_string())?
            .network_access()
    }

    /// `[sandbox.network]` is optional. A config with only the legacy
    /// top-level `deny_read` (or an empty file) yields `None`, leaving
    /// the policy-derived network access untouched.
    #[test]
    fn config_network_access_absent_yields_none() {
        assert_eq!(parse_network_access("").expect("empty parses"), None);
        assert_eq!(
            parse_network_access(r#"deny_read = ["/secret"]"#).expect("deny_read-only parses"),
            None,
            "deny_read at root must not be confused for a network section",
        );
    }

    /// A misspelled table or field name under `[sandbox]` must fail
    /// loudly rather than be silently ignored (which would leave the
    /// intended restriction unapplied). `deny_unknown_fields` on the
    /// new config structs guarantees this; an unknown `mode` value is
    /// already rejected by the enum.
    #[test]
    fn config_network_access_rejects_unknown_keys_and_modes() {
        // Typo'd subtable name: `networks` instead of `network`.
        assert!(
            parse_network_access("[sandbox.networks]\nmode = \"denied\"").is_err(),
            "a typo'd [sandbox.networks] table must not be silently ignored",
        );
        // Typo'd field inside the network table.
        assert!(
            parse_network_access("[sandbox.network]\nmodee = \"denied\"").is_err(),
            "a typo'd field under [sandbox.network] must be rejected",
        );
        // Unknown mode value.
        assert!(
            parse_network_access("[sandbox.network]\nmode = \"denyed\"").is_err(),
            "an unknown mode value must be rejected",
        );
    }

    /// The three documented `mode` values map onto the three-state
    /// `NetworkAccess` (sandbox.md §7.3). `deny_read` at the root and
    /// `[sandbox.network]` coexist in one file without collision.
    #[test]
    fn config_network_access_parses_each_mode() {
        assert_eq!(
            parse_network_access("[sandbox.network]\nmode = \"denied\"").expect("denied parses"),
            Some(NetworkAccess::Denied),
        );
        assert_eq!(
            parse_network_access("[sandbox.network]\nmode = \"full\"").expect("full parses"),
            Some(NetworkAccess::Full),
        );

        let allowlist = parse_network_access(
            "deny_read = [\"/secret\"]\n\
             [sandbox.network]\n\
             mode = \"allowlist\"\n\
             [[sandbox.network.services]]\n\
             host = \"registry.npmjs.org\"\n\
             ports = [443]\n",
        )
        .expect("allowlist parses");
        assert_eq!(
            allowlist,
            Some(NetworkAccess::Allowlist {
                services: vec![NetworkService {
                    host: "registry.npmjs.org".to_string(),
                    ports: vec![443],
                    protocol: None,
                }],
            }),
        );
    }

    /// Allowlist services are validated per sandbox.md §7.3: a bare
    /// `host = "*"` and an entry that omits `ports` for a
    /// high-sensitivity service are rejected with an actionable,
    /// file-attributed error.
    #[test]
    fn config_network_access_rejects_invalid_allowlist_services() {
        let wildcard = parse_network_access(
            "[sandbox.network]\n\
             mode = \"allowlist\"\n\
             [[sandbox.network.services]]\n\
             host = \"*\"\n\
             ports = [443]\n",
        )
        .expect_err("bare wildcard host must be rejected");
        assert!(
            wildcard.contains(".libra/sandbox.toml") && wildcard.contains("bare wildcard"),
            "expected file-attributed wildcard error, got: {wildcard}",
        );

        let empty_ports = parse_network_access(
            "[sandbox.network]\n\
             mode = \"allowlist\"\n\
             [[sandbox.network.services]]\n\
             host = \"bastion.example.com\"\n",
        )
        .expect_err("empty ports on a high-sensitivity entry must be rejected");
        assert!(
            empty_ports.contains("high-sensitivity"),
            "expected high-sensitivity-port error, got: {empty_ports}",
        );
    }

    /// End-to-end of the tightening wire: a `mode = "denied"` config
    /// locks down a `WorkspaceWrite { Full }` policy, while a config
    /// allowlist narrows a `Full` policy to exactly the listed
    /// services. The config can never widen the policy.
    #[test]
    fn config_network_access_tightens_policy_via_with_network_restriction() {
        let full_policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            network_access: NetworkAccess::Full,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        };

        let denied = parse_network_access("[sandbox.network]\nmode = \"denied\"")
            .expect("denied parses")
            .expect("present");
        match full_policy.with_network_restriction(&denied) {
            SandboxPolicy::WorkspaceWrite { network_access, .. } => {
                assert_eq!(network_access, NetworkAccess::Denied);
            }
            other => panic!("expected WorkspaceWrite, got {other:?}"),
        }

        // A config that opens an allowlist cannot loosen a Denied policy.
        let denied_policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            network_access: NetworkAccess::Denied,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        };
        let allowlist = parse_network_access(
            "[sandbox.network]\n\
             mode = \"allowlist\"\n\
             [[sandbox.network.services]]\n\
             host = \"registry.npmjs.org\"\n\
             ports = [443]\n",
        )
        .expect("allowlist parses")
        .expect("present");
        match denied_policy.with_network_restriction(&allowlist) {
            SandboxPolicy::WorkspaceWrite { network_access, .. } => {
                assert_eq!(
                    network_access,
                    NetworkAccess::Denied,
                    "config allowlist must not open a Denied policy without approval",
                );
            }
            other => panic!("expected WorkspaceWrite, got {other:?}"),
        }
    }

    #[test]
    fn on_request_requires_approval_in_workspace_write() {
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            network_access: NetworkAccess::Denied,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        };
        let requirement = shell_exec_approval_requirement(
            AskForApproval::OnRequest,
            Some(&policy),
            "python script.py",
            SandboxPermissions::RequireEscalated,
            None,
        );
        assert!(matches!(
            requirement,
            ExecApprovalRequirement::NeedsApproval { .. }
        ));
    }

    #[test]
    fn on_request_skips_approval_for_sandboxed_commands() {
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            network_access: NetworkAccess::Denied,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        };
        let requirement = shell_exec_approval_requirement(
            AskForApproval::OnRequest,
            Some(&policy),
            "python script.py",
            SandboxPermissions::UseDefault,
            None,
        );
        assert!(matches!(
            requirement,
            ExecApprovalRequirement::Skip {
                bypass_sandbox: false
            }
        ));
    }

    #[test]
    fn on_request_skips_approval_in_danger_full_access() {
        let requirement = shell_exec_approval_requirement(
            AskForApproval::OnRequest,
            Some(&SandboxPolicy::DangerFullAccess),
            "python script.py",
            SandboxPermissions::RequireEscalated,
            None,
        );
        assert!(matches!(
            requirement,
            ExecApprovalRequirement::Skip {
                bypass_sandbox: false
            }
        ));
    }

    #[test]
    fn unless_trusted_allows_known_safe_commands() {
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            network_access: NetworkAccess::Denied,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        };
        let requirement = shell_exec_approval_requirement(
            AskForApproval::UnlessTrusted,
            Some(&policy),
            "ls -la",
            SandboxPermissions::UseDefault,
            None,
        );
        assert!(matches!(
            requirement,
            ExecApprovalRequirement::Skip {
                bypass_sandbox: false
            }
        ));
    }

    #[test]
    fn never_forbids_dangerous_commands() {
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            network_access: NetworkAccess::Denied,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        };
        let requirement = shell_exec_approval_requirement(
            AskForApproval::Never,
            Some(&policy),
            "git reset --hard",
            SandboxPermissions::UseDefault,
            None,
        );
        assert!(matches!(
            requirement,
            ExecApprovalRequirement::Forbidden { .. }
        ));
    }

    #[test]
    fn sandbox_denied_keywords_trigger_detection() {
        let output = SandboxExecOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "operation not permitted".to_string(),
            timed_out: false,
        };
        assert!(is_likely_sandbox_denied(&output));
    }

    #[test]
    fn default_timeout_allows_typical_build_commands() {
        assert_eq!(DEFAULT_TIMEOUT_MS, 60_000);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_tmpdir_is_private_0700_and_cleanup_removes_it() {
        use std::os::unix::fs::PermissionsExt;

        let tmpdir = create_command_tmpdir().expect("private command tmpdir should be created");
        let metadata = std::fs::metadata(&tmpdir).expect("tmpdir metadata should be readable");

        assert_eq!(metadata.permissions().mode() & 0o777, COMMAND_TMPDIR_MODE);

        cleanup_command_tmpdir(&tmpdir, None).await;
        assert!(!tmpdir.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn protected_mountpoint_cleanup_removes_only_prepared_empty_directories() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("create protected mountpoint fixture");
        let git = temp.path().join(".git");
        let codex = temp.path().join(".codex");
        let mut prepared = PreparedProtectedMountpoints::prepare(&[git.clone(), codex.clone()])
            .expect("prepare absent protected mountpoints");

        assert_eq!(
            fs::metadata(&git)
                .expect("read prepared mountpoint metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        prepared
            .cleanup()
            .expect("remove unchanged empty mountpoints");
        assert!(!git.exists());
        assert!(!codex.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn protected_mountpoint_cleanup_refuses_replaced_paths() {
        let temp = tempfile::tempdir().expect("create protected mountpoint fixture");
        let git = temp.path().join(".git");
        let mut prepared = PreparedProtectedMountpoints::prepare(std::slice::from_ref(&git))
            .expect("prepare absent protected mountpoint");
        fs::remove_dir(&git).expect("remove prepared mountpoint");
        fs::write(&git, b"replacement").expect("replace mountpoint with a file");

        let error = prepared
            .cleanup()
            .expect_err("cleanup must not remove a replaced path");
        assert!(error.contains("changed identity"), "{error}");
        assert_eq!(
            fs::read(&git).expect("replacement must remain"),
            b"replacement"
        );
    }

    /// Pin the structured `TmpdirCleanupFailed` Evidence event the
    /// sandbox emits when `remove_dir_all` fails. We force a
    /// failure by pointing the cleanup at a path that exists as a
    /// regular file (not a directory) — `remove_dir_all` returns
    /// `NotADirectory`/`Other` and the sink should capture exactly
    /// one event with the file path. See
    /// `docs/development/commands/sandbox.md:142`.
    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_command_tmpdir_records_evidence_on_failure() {
        use std::sync::Arc;

        use crate::internal::ai::sandbox::evidence::{
            InMemorySandboxEvidenceSink, SandboxEvidenceEvent,
        };

        let temp = tempfile::tempdir().expect("tempdir for cleanup-evidence test");
        let file_path = temp.path().join("not-a-directory.txt");
        std::fs::write(&file_path, b"file, not a dir").expect("write tmpdir-target file");

        let sink = Arc::new(InMemorySandboxEvidenceSink::new());
        cleanup_command_tmpdir(&file_path, Some(sink.as_ref())).await;

        let events = sink.events();
        assert_eq!(events.len(), 1);
        match &events[0] {
            SandboxEvidenceEvent::TmpdirCleanupFailed { path, error } => {
                assert_eq!(path, &file_path);
                assert!(
                    !error.is_empty(),
                    "tmpdir cleanup failure must include a non-empty error string"
                );
            }
            other => panic!("expected TmpdirCleanupFailed, got {other:?}"),
        }
    }

    /// Pin that a successful cleanup emits **no** Evidence event.
    /// The sink contract is "structured surface for failures"; a
    /// happy-path cleanup must remain silent on the structured
    /// channel so consumers can treat any event as actionable.
    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_command_tmpdir_records_no_evidence_on_success() {
        use std::sync::Arc;

        use crate::internal::ai::sandbox::evidence::InMemorySandboxEvidenceSink;

        let tmpdir = create_command_tmpdir().expect("private command tmpdir should be created");
        let sink = Arc::new(InMemorySandboxEvidenceSink::new());

        cleanup_command_tmpdir(&tmpdir, Some(sink.as_ref())).await;

        assert!(!tmpdir.exists());
        assert!(
            sink.events().is_empty(),
            "successful cleanup must not emit an Evidence event"
        );
    }

    /// Pin the structured `EnforcementFailed` Evidence event the
    /// sandbox emits when `SandboxEnforcement::Required` cannot
    /// produce an effective backend (Linux without
    /// `LIBRA_LINUX_SANDBOX_EXE`). See
    /// `docs/development/commands/sandbox.md:143`, L162, L373. Linux-only
    /// because the EnforcementFailed branch fires on the missing-
    /// helper path; on macOS the seatbelt helper is always
    /// available so the same inputs select `MacosSeatbelt` instead.
    #[cfg(target_os = "linux")]
    #[test]
    #[serial(sandbox_env)]
    fn build_command_from_spec_records_evidence_on_enforcement_failed() {
        use std::sync::Arc;

        use crate::internal::ai::sandbox::{
            evidence::{InMemorySandboxEvidenceSink, SandboxEvidenceEvent},
            policy::SandboxPolicy,
        };

        // Clear any ambient Linux sandbox helper and bwrap fallback
        // so the missing-helper branch fires deterministically.
        let _helper_guard = EnvVarGuard::unset("LIBRA_LINUX_SANDBOX_EXE");
        let _bwrap_guard = EnvVarGuard::set("LIBRA_BWRAP_BINARY", "/tmp/libra-never-exists");

        let sink = Arc::new(InMemorySandboxEvidenceSink::new());
        let sandbox_runtime = SandboxRuntimeConfig {
            enforcement: SandboxEnforcement::Required,
            evidence_sink: Some(sink.clone()),
            ..SandboxRuntimeConfig::default()
        };
        let sandbox_context = ToolSandboxContext {
            policy: SandboxPolicy::WorkspaceWrite {
                writable_roots: Vec::new(),
                network_access: NetworkAccess::Denied,
                exclude_tmpdir_env_var: false,
                exclude_slash_tmp: false,
            },
            permissions: SandboxPermissions::default(),
        };
        let spec = CommandSpec {
            program: "/bin/true".to_string(),
            args: Vec::new(),
            cwd: std::env::temp_dir(),
            env: std::collections::HashMap::new(),
            clear_env: false,
            stdin: None,
            timeout_ms: None,
            sandbox_permissions: SandboxPermissions::default(),
            justification: None,
        };

        let result = build_command_from_spec(
            spec,
            Some(&sandbox_context),
            Some(&sandbox_runtime),
            None,
            None,
        );

        assert!(
            result.is_err(),
            "Required enforcement without a helper must abort build"
        );

        let events = sink.events();
        assert_eq!(events.len(), 1, "exactly one Evidence event per rejection");
        match &events[0] {
            SandboxEvidenceEvent::EnforcementFailed { reason } => {
                assert!(
                    reason.contains("enforcement is required"),
                    "reason must echo the doc's enforcement phrase: {reason}"
                );
            }
            other => panic!("expected EnforcementFailed, got {other:?}"),
        }
    }

    /// Pin the structured `WritableRootRejected` Evidence event
    /// the sandbox emits when a configured writable_root matches a
    /// dangerous mount pattern (e.g. `/var/run/docker.sock`). See
    /// `docs/development/commands/sandbox.md:143`. The sandbox's
    /// `validate_writable_roots_with_cwd` returns
    /// `SandboxPolicyError::DangerousWritableRoot`, which
    /// `build_command_from_spec` propagates as a `String` error
    /// after emitting the Evidence row.
    #[test]
    fn build_command_from_spec_records_evidence_on_dangerous_writable_root() {
        use std::sync::Arc;

        use crate::internal::ai::sandbox::{
            evidence::{InMemorySandboxEvidenceSink, SandboxEvidenceEvent},
            policy::SandboxPolicy,
        };

        let sink = Arc::new(InMemorySandboxEvidenceSink::new());
        let sandbox_runtime = SandboxRuntimeConfig {
            evidence_sink: Some(sink.clone()),
            ..SandboxRuntimeConfig::default()
        };
        let dangerous_root = PathBuf::from("/var/run/docker.sock");
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![dangerous_root.clone()],
            network_access: NetworkAccess::Denied,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        };
        let sandbox_context = ToolSandboxContext {
            policy,
            permissions: SandboxPermissions::default(),
        };
        let spec = CommandSpec {
            program: "/bin/true".to_string(),
            args: Vec::new(),
            cwd: PathBuf::from("/tmp/workspace"),
            env: std::collections::HashMap::new(),
            clear_env: false,
            stdin: None,
            timeout_ms: None,
            sandbox_permissions: SandboxPermissions::default(),
            justification: None,
        };

        let result = build_command_from_spec(
            spec,
            Some(&sandbox_context),
            Some(&sandbox_runtime),
            None,
            None,
        );

        // The dangerous writable root must reject the spec.
        assert!(result.is_err(), "dangerous writable root must abort build");

        // AND the sink must have captured the structured event.
        let events = sink.events();
        assert_eq!(events.len(), 1, "exactly one Evidence event per rejection");
        match &events[0] {
            SandboxEvidenceEvent::WritableRootRejected { root, reason } => {
                // The recorded root is canonicalised before validation
                // (see `push_root_unique` in policy.rs), so on hosts
                // where `/var/run` is a symlink to `/run` it surfaces as
                // `/run/docker.sock` rather than the requested
                // `/var/run/docker.sock`. Assert the stable, symlink-
                // independent property: the rejected root is the Docker
                // socket by file name.
                assert_eq!(
                    root.file_name(),
                    dangerous_root.file_name(),
                    "rejected root should be the docker socket (canonicalised path may differ by host symlink layout): {root:?}"
                );
                assert!(
                    !reason.is_empty(),
                    "rejection must include a non-empty reason"
                );
            }
            other => panic!("expected WritableRootRejected, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_command_spec_injects_private_tmp_and_cleans_it() {
        let temp = tempfile::tempdir().expect("tempdir for command tmp env test");
        let caller_tmp = temp.path().join("caller-tmp");
        std::fs::create_dir(&caller_tmp).expect("caller tmpdir should be created");
        let mut spec = CommandSpec {
            program: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "printf '%s\\n' \"$TMPDIR\"; touch \"$TMPDIR/probe\"; test \"$TMPDIR\" = \"$TEMP\"; test \"$TMPDIR\" = \"$TMP\"".to_string(),
            ],
            cwd: temp.path().to_path_buf(),
            env: std::collections::HashMap::from([(
                "TMPDIR".to_string(),
                caller_tmp.to_string_lossy().into_owned(),
            )]),
            clear_env: false,
            stdin: None,
            timeout_ms: Some(5_000),
            sandbox_permissions: SandboxPermissions::UseDefault,
            justification: None,
        };
        spec.env.insert(
            "TEMP".to_string(),
            caller_tmp.to_string_lossy().into_owned(),
        );
        spec.env
            .insert("TMP".to_string(), caller_tmp.to_string_lossy().into_owned());

        let output = run_command_spec(spec, 16 * 1024, None, None, None, None)
            .await
            .expect("command should run with private tmp env");

        assert_eq!(output.exit_code, 0, "stderr: {}", output.stderr);
        let command_tmpdir = PathBuf::from(output.stdout.trim());
        assert_ne!(command_tmpdir, caller_tmp);
        assert!(
            command_tmpdir
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(COMMAND_TMPDIR_PREFIX))
        );
        assert!(!command_tmpdir.exists());
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn repair_missing_process_cwd_restores_deleted_process_cwd() {
        struct RestoreCwd(Option<PathBuf>);

        impl Drop for RestoreCwd {
            fn drop(&mut self) {
                if let Some(path) = self.0.take() {
                    let _ = std::env::set_current_dir(path);
                }
            }
        }

        let _cwd_lock = crate::utils::test::cwd_lock_guard();
        let original =
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
        let _restore = RestoreCwd(Some(original));
        let deleted_cwd = tempfile::tempdir().expect("deleted cwd tempdir");
        let fallback_cwd = tempfile::tempdir().expect("fallback cwd tempdir");
        std::env::set_current_dir(deleted_cwd.path()).expect("enter deleted cwd candidate");
        drop(deleted_cwd);
        assert!(std::env::current_dir().is_err());

        repair_missing_process_cwd(fallback_cwd.path()).expect("cwd repair should succeed");

        let expected = fallback_cwd
            .path()
            .canonicalize()
            .unwrap_or_else(|_| fallback_cwd.path().to_path_buf());
        let actual = std::env::current_dir().expect("current dir should be restored");
        assert_eq!(actual, expected);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_command_spec_injects_allowlist_proxy_env_for_allowlist_policy() {
        let spec = CommandSpec {
            program: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "printf '%s\n%s\n%s\n' \"$HTTPS_PROXY\" \"$NO_PROXY\" \"$LIBRA_SANDBOX_ALLOWLIST_PROXY\"".to_string(),
            ],
            cwd: std::env::temp_dir(),
            env: std::collections::HashMap::new(),
            clear_env: false,
            stdin: None,
            timeout_ms: Some(5_000),
            sandbox_permissions: SandboxPermissions::UseDefault,
            justification: None,
        };
        let sandbox = ToolSandboxContext {
            policy: SandboxPolicy::ExternalSandbox {
                network_access: NetworkAccess::Allowlist {
                    services: vec![NetworkService {
                        host: "registry.npmjs.org".to_string(),
                        ports: vec![443],
                        protocol: Some(NetworkProtocol::Tcp),
                    }],
                },
            },
            permissions: SandboxPermissions::UseDefault,
        };
        let sandbox_runtime = SandboxRuntimeConfig {
            enforcement: SandboxEnforcement::Required,
            ..SandboxRuntimeConfig::default()
        };

        let output = run_command_spec(
            spec,
            16 * 1024,
            Some(sandbox),
            Some(&sandbox_runtime),
            None,
            None,
        )
        .await
        .expect("allowlist command should receive proxy env");

        assert_eq!(output.exit_code, 0, "stderr: {}", output.stderr);
        let lines = output.stdout.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3, "stdout: {:?}", output.stdout);
        assert!(lines[0].starts_with("http://127.0.0.1:"), "{lines:?}");
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], lines[0]);
    }

    #[test]
    fn approval_context_reports_workspace_write_details() {
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![PathBuf::from("src")],
            network_access: NetworkAccess::Denied,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        };

        let (sandbox_label, network_access, writable_roots) = approval_request_context(
            Some(&policy),
            Path::new("/tmp/workspace"),
            SandboxPermissions::UseDefault,
            false,
            None,
        );

        assert_eq!(sandbox_label, "workspace-write");
        assert_eq!(network_access, NetworkAccess::Denied);
        assert_eq!(writable_roots, vec![PathBuf::from("/tmp/workspace/src")]);
    }

    #[test]
    fn approval_context_marks_retry_as_outside_sandbox() {
        let (sandbox_label, network_access, writable_roots) = approval_request_context(
            Some(&SandboxPolicy::ReadOnly),
            Path::new("/tmp/workspace"),
            SandboxPermissions::UseDefault,
            true,
            None,
        );

        assert_eq!(sandbox_label, "outside sandbox");
        assert_eq!(network_access, NetworkAccess::Full);
        assert!(writable_roots.is_empty());
    }

    #[test]
    fn requested_network_access_upgrade_detects_config_widening() {
        let temp = tempfile::tempdir().expect("tempdir for network config");
        let libra_dir = temp.path().join(crate::utils::util::ROOT_DIR);
        std::fs::create_dir_all(&libra_dir).expect("create .libra dir");
        std::fs::write(
            libra_dir.join(SANDBOX_CONFIG_FILE),
            "[sandbox.network]\n\
             mode = \"allowlist\"\n\
             [[sandbox.network.services]]\n\
             host = \"registry.npmjs.org\"\n\
             ports = [443]\n",
        )
        .expect("write sandbox config");
        let policy = SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![temp.path().to_path_buf()],
            network_access: NetworkAccess::Denied,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        };

        let upgrade = requested_network_access_upgrade(
            Some(&policy),
            SandboxPermissions::UseDefault,
            temp.path(),
            false,
        )
        .expect("config should parse");

        assert_eq!(
            upgrade,
            Some(NetworkAccess::Allowlist {
                services: vec![NetworkService {
                    host: "registry.npmjs.org".to_string(),
                    ports: vec![443],
                    protocol: None,
                }],
            })
        );
    }

    #[test]
    fn requested_network_access_upgrade_detects_config_full_widening() {
        let temp = tempfile::tempdir().expect("tempdir for full network config");
        let libra_dir = temp.path().join(crate::utils::util::ROOT_DIR);
        std::fs::create_dir_all(&libra_dir).expect("create .libra dir");
        std::fs::write(
            libra_dir.join(SANDBOX_CONFIG_FILE),
            "[sandbox.network]\nmode = \"full\"\n",
        )
        .expect("write sandbox config");
        let policy = SandboxPolicy::ExternalSandbox {
            network_access: NetworkAccess::Denied,
        };

        let upgrade = requested_network_access_upgrade(
            Some(&policy),
            SandboxPermissions::UseDefault,
            temp.path(),
            false,
        )
        .expect("config should parse");

        assert_eq!(upgrade, Some(NetworkAccess::Full));
    }

    #[test]
    fn requested_network_access_upgrade_skips_non_network_bearing_policy() {
        let temp = tempfile::tempdir().expect("tempdir for read-only network config");
        let libra_dir = temp.path().join(crate::utils::util::ROOT_DIR);
        std::fs::create_dir_all(&libra_dir).expect("create .libra dir");
        std::fs::write(
            libra_dir.join(SANDBOX_CONFIG_FILE),
            "[sandbox.network]\nmode = \"full\"\n",
        )
        .expect("write sandbox config");

        let upgrade = requested_network_access_upgrade(
            Some(&SandboxPolicy::ReadOnly),
            SandboxPermissions::UseDefault,
            temp.path(),
            false,
        )
        .expect("config should parse");

        assert_eq!(upgrade, None);
    }

    #[tokio::test]
    async fn network_access_upgrade_uses_uncached_approval_request() {
        let temp = tempfile::tempdir().expect("tempdir for network approval test");
        let libra_dir = temp.path().join(crate::utils::util::ROOT_DIR);
        std::fs::create_dir_all(&libra_dir).expect("create .libra dir");
        std::fs::write(
            libra_dir.join(SANDBOX_CONFIG_FILE),
            "[sandbox.network]\n\
             mode = \"allowlist\"\n\
             [[sandbox.network.services]]\n\
             host = \"registry.npmjs.org\"\n\
             ports = [443]\n",
        )
        .expect("write sandbox config");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = ToolApprovalContext {
            policy: AskForApproval::OnRequest,
            request_tx: tx,
            store: Arc::new(tokio::sync::Mutex::new(ApprovalStore::default())),
            scope_key_prefix: None,
            approval_ttl: DEFAULT_APPROVAL_TTL,
            cache_policy: ApprovalCachePolicy::default(),
        };
        let cwd = temp.path().to_path_buf();
        let run = tokio::spawn({
            let cwd = cwd.clone();
            async move {
                run_shell_command_with_approval(ShellCommandRequest {
                    call_id: "call-network-upgrade".to_string(),
                    command: "true".to_string(),
                    cwd: cwd.clone(),
                    timeout_ms: Some(5_000),
                    max_output_bytes: 16 * 1024,
                    sandbox: Some(ToolSandboxContext {
                        policy: SandboxPolicy::WorkspaceWrite {
                            writable_roots: vec![cwd],
                            network_access: NetworkAccess::Denied,
                            exclude_tmpdir_env_var: false,
                            exclude_slash_tmp: false,
                        },
                        permissions: SandboxPermissions::UseDefault,
                    }),
                    sandbox_runtime: None,
                    evidence_sink: None,
                    approval: Some(ctx),
                    justification: None,
                    safety_decision: None,
                })
                .await
            }
        });

        let request = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("network upgrade approval should be requested")
            .expect("approval channel should stay open");
        assert_eq!(request.command, "true");
        assert_eq!(request.sandbox_label, "workspace-write");
        assert_eq!(
            request.cache_disabled_reason.as_deref(),
            Some("network access escalation approvals are not cached")
        );
        assert_eq!(
            request.network_access,
            NetworkAccess::Allowlist {
                services: vec![NetworkService {
                    host: "registry.npmjs.org".to_string(),
                    ports: vec![443],
                    protocol: None,
                }],
            }
        );
        assert!(
            request
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("requested network access escalation")
        );
        request
            .response_tx
            .send(ReviewDecision::Denied)
            .expect("approval receiver should be active");

        let error = run
            .await
            .expect("network upgrade run task should not panic")
            .expect_err("denying network upgrade should stop execution");
        assert_eq!(error, "rejected by user");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn approved_network_access_upgrade_reaches_command_environment() {
        let temp = tempfile::tempdir().expect("tempdir for approved network test");
        let libra_dir = temp.path().join(crate::utils::util::ROOT_DIR);
        std::fs::create_dir_all(&libra_dir).expect("create .libra dir");
        std::fs::write(
            libra_dir.join(SANDBOX_CONFIG_FILE),
            "[sandbox.network]\n\
             mode = \"allowlist\"\n\
             [[sandbox.network.services]]\n\
             host = \"registry.npmjs.org\"\n\
             ports = [443]\n",
        )
        .expect("write sandbox config");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = ToolApprovalContext {
            policy: AskForApproval::OnRequest,
            request_tx: tx,
            store: Arc::new(tokio::sync::Mutex::new(ApprovalStore::default())),
            scope_key_prefix: None,
            approval_ttl: DEFAULT_APPROVAL_TTL,
            cache_policy: ApprovalCachePolicy::default(),
        };
        let cwd = temp.path().to_path_buf();
        let run = tokio::spawn({
            let cwd = cwd.clone();
            async move {
                run_shell_command_with_approval(ShellCommandRequest {
                    call_id: "call-approved-network-upgrade".to_string(),
                    command: "printf '%s\n%s\n%s\n' \"$HTTPS_PROXY\" \"$NO_PROXY\" \"$LIBRA_SANDBOX_ALLOWLIST_PROXY\""
                        .to_string(),
                    cwd: cwd.clone(),
                    timeout_ms: Some(5_000),
                    max_output_bytes: 16 * 1024,
                    sandbox: Some(ToolSandboxContext {
                        policy: SandboxPolicy::ExternalSandbox {
                            network_access: NetworkAccess::Denied,
                        },
                        permissions: SandboxPermissions::UseDefault,
                    }),
                    sandbox_runtime: Some(SandboxRuntimeConfig {
                        enforcement: SandboxEnforcement::Required,
                        ..SandboxRuntimeConfig::default()
                    }),
                    evidence_sink: None,
                    approval: Some(ctx),
                    justification: None,
                    safety_decision: None,
                })
                .await
            }
        });

        let request = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("network upgrade approval should be requested")
            .expect("approval channel should stay open");
        assert_eq!(
            request.command,
            "printf '%s\n%s\n%s\n' \"$HTTPS_PROXY\" \"$NO_PROXY\" \"$LIBRA_SANDBOX_ALLOWLIST_PROXY\""
        );
        assert_eq!(request.sandbox_label, "external-sandbox");
        assert_eq!(
            request.network_access,
            NetworkAccess::Allowlist {
                services: vec![NetworkService {
                    host: "registry.npmjs.org".to_string(),
                    ports: vec![443],
                    protocol: None,
                }],
            }
        );
        request
            .response_tx
            .send(ReviewDecision::Approved)
            .expect("approval receiver should be active");

        let output = run
            .await
            .expect("approved network upgrade task should not panic")
            .expect("approved network upgrade should run command");
        assert_eq!(output.exit_code, 0, "stderr: {}", output.stderr);
        let lines = output.stdout.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3, "stdout: {:?}", output.stdout);
        assert!(lines[0].starts_with("http://127.0.0.1:"), "{lines:?}");
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], lines[0]);
        assert!(matches!(
            rx.try_recv(),
            Err(TryRecvError::Empty | TryRecvError::Disconnected)
        ));
    }

    #[tokio::test]
    async fn safety_deny_does_not_request_network_upgrade_approval() {
        let temp = tempfile::tempdir().expect("tempdir for denied network approval test");
        let libra_dir = temp.path().join(crate::utils::util::ROOT_DIR);
        std::fs::create_dir_all(&libra_dir).expect("create .libra dir");
        std::fs::write(
            libra_dir.join(SANDBOX_CONFIG_FILE),
            "[sandbox.network]\nmode = \"full\"\n",
        )
        .expect("write sandbox config");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = ToolApprovalContext {
            policy: AskForApproval::OnRequest,
            request_tx: tx,
            store: Arc::new(tokio::sync::Mutex::new(ApprovalStore::default())),
            scope_key_prefix: None,
            approval_ttl: DEFAULT_APPROVAL_TTL,
            cache_policy: ApprovalCachePolicy::default(),
        };

        let error = run_shell_command_with_approval(ShellCommandRequest {
            call_id: "call-network-upgrade-denied".to_string(),
            command: "echo should-not-run".to_string(),
            cwd: temp.path().to_path_buf(),
            timeout_ms: Some(5_000),
            max_output_bytes: 16 * 1024,
            sandbox: Some(ToolSandboxContext {
                policy: SandboxPolicy::WorkspaceWrite {
                    writable_roots: vec![temp.path().to_path_buf()],
                    network_access: NetworkAccess::Denied,
                    exclude_tmpdir_env_var: false,
                    exclude_slash_tmp: false,
                },
                permissions: SandboxPermissions::UseDefault,
            }),
            sandbox_runtime: None,
            evidence_sink: None,
            approval: Some(ctx),
            justification: None,
            safety_decision: Some(SafetyDecision::deny(
                "test.deny",
                "policy denial remains authoritative",
                super::super::runtime::hardening::BlastRadius::Workspace,
            )),
        })
        .await
        .expect_err("safety deny should stop before network upgrade approval");

        assert!(error.contains("policy denial remains authoritative"));
        assert!(matches!(
            rx.try_recv(),
            Err(TryRecvError::Empty | TryRecvError::Disconnected)
        ));
    }

    #[tokio::test]
    async fn cached_approval_skips_prompt_when_all_keys_are_preapproved() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let store = Arc::new(tokio::sync::Mutex::new(ApprovalStore::default()));
        {
            let mut guard = store.lock().await;
            guard.put("k1".to_string(), ReviewDecision::ApprovedForSession);
            guard.put("k2".to_string(), ReviewDecision::ApprovedForSession);
        }
        let ctx = ToolApprovalContext {
            policy: AskForApproval::OnRequest,
            request_tx: tx,
            store: Arc::clone(&store),
            scope_key_prefix: None,
            approval_ttl: DEFAULT_APPROVAL_TTL,
            cache_policy: ApprovalCachePolicy::default(),
        };
        let keys = vec!["k1".to_string(), "k2".to_string()];

        let decision =
            request_cached_approval_with_keys(&ctx, &keys, |response_tx| ExecApprovalRequest {
                call_id: "call-1".to_string(),
                command: "echo hi".to_string(),
                cwd: PathBuf::from("/tmp"),
                reason: None,
                is_retry: false,
                sandbox_label: "workspace-write".to_string(),
                network_access: NetworkAccess::Denied,
                writable_roots: vec![PathBuf::from("/tmp")],
                cache_disabled_reason: None,
                response_tx,
            })
            .await;

        assert_eq!(decision, ReviewDecision::ApprovedForSession);
        assert!(matches!(
            rx.try_recv(),
            Err(TryRecvError::Empty | TryRecvError::Disconnected)
        ));
    }

    #[tokio::test]
    async fn approved_for_session_decision_is_cached_for_each_key() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let store = Arc::new(tokio::sync::Mutex::new(ApprovalStore::default()));
        let ctx = ToolApprovalContext {
            policy: AskForApproval::OnRequest,
            request_tx: tx,
            store: Arc::clone(&store),
            scope_key_prefix: None,
            approval_ttl: DEFAULT_APPROVAL_TTL,
            cache_policy: ApprovalCachePolicy::default(),
        };
        let keys = vec!["a".to_string(), "b".to_string()];

        let responder = tokio::spawn(async move {
            let request = rx.recv().await.expect("approval request expected");
            let _ = request.response_tx.send(ReviewDecision::ApprovedForSession);
        });

        let decision =
            request_cached_approval_with_keys(&ctx, &keys, |response_tx| ExecApprovalRequest {
                call_id: "call-2".to_string(),
                command: "apply_patch".to_string(),
                cwd: PathBuf::from("/tmp"),
                reason: Some("test".to_string()),
                is_retry: false,
                sandbox_label: "workspace-write".to_string(),
                network_access: NetworkAccess::Denied,
                writable_roots: vec![PathBuf::from("/tmp")],
                cache_disabled_reason: None,
                response_tx,
            })
            .await;

        responder.await.expect("responder task failed");
        assert_eq!(decision, ReviewDecision::ApprovedForSession);
        let guard = store.lock().await;
        assert_eq!(guard.get("a"), Some(ReviewDecision::ApprovedForSession));
        assert_eq!(guard.get("b"), Some(ReviewDecision::ApprovedForSession));
    }

    #[tokio::test]
    async fn approved_for_all_commands_decision_skips_later_prompts() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let store = Arc::new(tokio::sync::Mutex::new(ApprovalStore::default()));
        let ctx = ToolApprovalContext {
            policy: AskForApproval::OnRequest,
            request_tx: tx,
            store: Arc::clone(&store),
            scope_key_prefix: None,
            approval_ttl: DEFAULT_APPROVAL_TTL,
            cache_policy: ApprovalCachePolicy::default(),
        };

        let responder = tokio::spawn(async move {
            let request = rx.recv().await.expect("approval request expected");
            let _ = request
                .response_tx
                .send(ReviewDecision::ApprovedForAllCommands);
            assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
            rx
        });

        let first_keys = vec!["first".to_string()];
        let first_decision = request_cached_approval_with_keys(&ctx, &first_keys, |response_tx| {
            ExecApprovalRequest {
                call_id: "call-allow-all".to_string(),
                command: "cargo test".to_string(),
                cwd: PathBuf::from("/tmp"),
                reason: None,
                is_retry: false,
                sandbox_label: "workspace-write".to_string(),
                network_access: NetworkAccess::Denied,
                writable_roots: vec![PathBuf::from("/tmp")],
                cache_disabled_reason: None,
                response_tx,
            }
        })
        .await;

        assert_eq!(first_decision, ReviewDecision::ApprovedForAllCommands);
        assert!(store.lock().await.allow_all_commands());
        let mut rx = responder.await.expect("responder task failed");

        let second_keys = vec!["different-command".to_string()];
        let second_decision =
            request_cached_approval_with_keys(&ctx, &second_keys, |response_tx| {
                ExecApprovalRequest {
                    call_id: "call-skipped".to_string(),
                    command: "git status".to_string(),
                    cwd: PathBuf::from("/tmp/other"),
                    reason: None,
                    is_retry: false,
                    sandbox_label: "workspace-write".to_string(),
                    network_access: NetworkAccess::Denied,
                    writable_roots: vec![PathBuf::from("/tmp/other")],
                    cache_disabled_reason: None,
                    response_tx,
                }
            })
            .await;

        assert_eq!(second_decision, ReviewDecision::ApprovedForAllCommands);
        assert!(matches!(
            rx.try_recv(),
            Err(TryRecvError::Empty | TryRecvError::Disconnected)
        ));
    }

    #[tokio::test]
    async fn uncached_exec_approval_ignores_allow_all_command_cache() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let store = Arc::new(tokio::sync::Mutex::new(ApprovalStore::default()));
        store.lock().await.approve_all_commands();
        let ctx = ToolApprovalContext {
            policy: AskForApproval::OnRequest,
            request_tx: tx,
            store,
            scope_key_prefix: None,
            approval_ttl: DEFAULT_APPROVAL_TTL,
            cache_policy: ApprovalCachePolicy::default(),
        };

        let responder = tokio::spawn(async move {
            let request = rx.recv().await.expect("uncached approval request expected");
            assert_eq!(
                request.cache_disabled_reason.as_deref(),
                Some("sandbox fallback approvals are not cached")
            );
            let _ = request.response_tx.send(ReviewDecision::Denied);
        });

        let decision = request_uncached_exec_approval(
            &ctx,
            ExecApprovalPrompt {
                call_id: "call-uncached",
                command: "touch generated.txt",
                cwd: Path::new("/tmp/workspace"),
                reason: Some("sandbox fallback requires confirmation".to_string()),
                sandbox_policy: None,
                sandbox_permissions: SandboxPermissions::RequireEscalated,
                is_retry: true,
                requested_network_access: None,
            },
            Some("sandbox fallback approvals are not cached".to_string()),
        )
        .await;

        responder.await.expect("responder task failed");
        assert_eq!(decision, ReviewDecision::Denied);
    }

    #[tokio::test]
    async fn allow_all_policy_runs_dangerous_shell_without_prompt() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let store = Arc::new(tokio::sync::Mutex::new(ApprovalStore::default()));
        store.lock().await.approve_all_commands();
        let ctx = ToolApprovalContext {
            policy: AskForApproval::Never,
            request_tx: tx,
            store,
            scope_key_prefix: None,
            approval_ttl: DEFAULT_APPROVAL_TTL,
            cache_policy: ApprovalCachePolicy::default(),
        };
        let temp = tempfile::tempdir().expect("tempdir for allow-all shell test");

        let output = run_shell_command_with_approval(ShellCommandRequest {
            call_id: "call-allow-all-shell".to_string(),
            command: "rm -f libra-approval-test-nonexistent-file.txt".to_string(),
            cwd: temp.path().to_path_buf(),
            timeout_ms: Some(5_000),
            max_output_bytes: 16 * 1024,
            sandbox: None,
            sandbox_runtime: None,
            evidence_sink: None,
            approval: Some(ctx),
            justification: None,
            safety_decision: None,
        })
        .await
        .expect("allow-all approval policy should run without prompting");

        assert_eq!(output.exit_code, 0);
        assert!(matches!(
            rx.try_recv(),
            Err(TryRecvError::Empty | TryRecvError::Disconnected)
        ));
    }

    #[tokio::test]
    async fn allow_all_policy_does_not_override_shell_safety_deny() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let store = Arc::new(tokio::sync::Mutex::new(ApprovalStore::default()));
        store.lock().await.approve_all_commands();
        let ctx = ToolApprovalContext {
            policy: AskForApproval::Never,
            request_tx: tx,
            store,
            scope_key_prefix: None,
            approval_ttl: DEFAULT_APPROVAL_TTL,
            cache_policy: ApprovalCachePolicy::default(),
        };
        let temp = tempfile::tempdir().expect("tempdir for allow-all shell deny test");

        let error = run_shell_command_with_approval(ShellCommandRequest {
            call_id: "call-allow-all-deny".to_string(),
            command: "echo should-not-run".to_string(),
            cwd: temp.path().to_path_buf(),
            timeout_ms: Some(5_000),
            max_output_bytes: 16 * 1024,
            sandbox: None,
            sandbox_runtime: None,
            evidence_sink: None,
            approval: Some(ctx),
            justification: None,
            safety_decision: Some(SafetyDecision::deny(
                "test.deny",
                "policy denial remains authoritative",
                super::super::runtime::hardening::BlastRadius::Workspace,
            )),
        })
        .await
        .expect_err("safety deny should override allow-all approval cache");

        assert!(error.contains("policy denial remains authoritative"));
        assert!(matches!(
            rx.try_recv(),
            Err(TryRecvError::Empty | TryRecvError::Disconnected)
        ));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[serial(sandbox_env)]
    async fn prefer_strict_missing_linux_helper_requires_fallback_approval() {
        let _env_guard = EnvVarGuard::unset("LIBRA_LINUX_SANDBOX_EXE");
        let _bwrap_guard = EnvVarGuard::set("LIBRA_BWRAP_BINARY", "/tmp/libra-never-exists");
        let temp = tempfile::tempdir().expect("tempdir for prefer-strict fallback test");
        let marker = temp.path().join("should-not-run");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = ToolApprovalContext {
            policy: AskForApproval::OnRequest,
            request_tx: tx,
            store: Arc::new(tokio::sync::Mutex::new(ApprovalStore::default())),
            scope_key_prefix: None,
            approval_ttl: DEFAULT_APPROVAL_TTL,
            cache_policy: ApprovalCachePolicy::default(),
        };

        let run = tokio::spawn({
            let cwd = temp.path().to_path_buf();
            async move {
                run_shell_command_with_approval(ShellCommandRequest {
                    call_id: "call-prefer-strict-fallback".to_string(),
                    command: "touch should-not-run".to_string(),
                    cwd: cwd.clone(),
                    timeout_ms: Some(5_000),
                    max_output_bytes: 16 * 1024,
                    sandbox: Some(ToolSandboxContext {
                        policy: SandboxPolicy::WorkspaceWrite {
                            writable_roots: vec![cwd],
                            network_access: NetworkAccess::Denied,
                            exclude_tmpdir_env_var: false,
                            exclude_slash_tmp: false,
                        },
                        permissions: SandboxPermissions::UseDefault,
                    }),
                    sandbox_runtime: Some(SandboxRuntimeConfig {
                        enforcement: SandboxEnforcement::PreferStrict,
                        ..SandboxRuntimeConfig::default()
                    }),
                    evidence_sink: None,
                    approval: Some(ctx),
                    justification: None,
                    safety_decision: None,
                })
                .await
            }
        });

        let request = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("prefer-strict fallback approval should be requested")
            .expect("approval channel should stay open");
        assert_eq!(request.command, "touch should-not-run");
        assert_eq!(request.sandbox_label, "outside sandbox");
        assert!(request.is_retry);
        assert!(
            request
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("Linux sandbox helper is not configured"),
            "unexpected reason: {:?}",
            request.reason
        );
        assert_eq!(
            request.cache_disabled_reason.as_deref(),
            Some("sandbox fallback approvals are not cached")
        );
        request
            .response_tx
            .send(ReviewDecision::Denied)
            .expect("approval receiver should be active");

        let error = run
            .await
            .expect("prefer-strict run task should not panic")
            .expect_err("denying fallback approval should stop execution");
        assert_eq!(error, "rejected by user");
        assert!(
            !marker.exists(),
            "command must not run after fallback approval is denied"
        );
    }

    #[tokio::test]
    async fn directory_ttl_approval_reuses_for_same_command_family_in_cwd() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let store = Arc::new(tokio::sync::Mutex::new(ApprovalStore::default()));
        let ctx = ToolApprovalContext {
            policy: AskForApproval::OnRequest,
            request_tx: tx,
            store: Arc::clone(&store),
            scope_key_prefix: None,
            approval_ttl: DEFAULT_APPROVAL_TTL,
            cache_policy: ApprovalCachePolicy::default(),
        };
        let cwd = Path::new("/tmp/workspace");
        let first_keys =
            ApprovalCacheKeys::shell("touch generated-a.txt", cwd, SandboxPermissions::UseDefault);

        let responder = tokio::spawn(async move {
            let request = rx.recv().await.expect("approval request expected");
            let _ = request
                .response_tx
                .send(ReviewDecision::ApprovedForDirectoryTtl);
            rx
        });

        let first_decision = request_cached_approval_with_cache_keys(
            &ctx,
            first_keys,
            None,
            |response_tx, cache_disabled_reason| {
                test_exec_request(
                    "touch generated-a.txt",
                    cwd,
                    response_tx,
                    cache_disabled_reason,
                )
            },
        )
        .await;
        assert_eq!(first_decision, ReviewDecision::ApprovedForDirectoryTtl);

        let mut rx = responder.await.expect("responder task failed");
        let second_keys =
            ApprovalCacheKeys::shell("touch generated-b.txt", cwd, SandboxPermissions::UseDefault);
        let second_decision = request_cached_approval_with_cache_keys(
            &ctx,
            second_keys,
            None,
            |response_tx, cache_disabled_reason| {
                test_exec_request(
                    "touch generated-b.txt",
                    cwd,
                    response_tx,
                    cache_disabled_reason,
                )
            },
        )
        .await;

        assert_eq!(second_decision, ReviewDecision::ApprovedForTtl);
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[tokio::test]
    async fn protected_branch_policy_disables_approval_cache_reuse() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let store = Arc::new(tokio::sync::Mutex::new(ApprovalStore::default()));
        let ctx = ToolApprovalContext {
            policy: AskForApproval::OnRequest,
            request_tx: tx,
            store: Arc::clone(&store),
            scope_key_prefix: None,
            approval_ttl: DEFAULT_APPROVAL_TTL,
            cache_policy: ApprovalCachePolicy {
                protected_branches: vec!["main".to_string()],
                allowed_network_domains: Vec::new(),
                no_cache_unknown_network: false,
                approved_ruleset: None,
            },
        };

        let responder = tokio::spawn(async move {
            let first = rx.recv().await.expect("first approval request expected");
            assert!(
                first
                    .cache_disabled_reason
                    .as_deref()
                    .unwrap_or_default()
                    .contains("protected branch `main`")
            );
            let _ = first.response_tx.send(ReviewDecision::ApprovedForTtl);

            let second = rx.recv().await.expect("second approval request expected");
            assert!(second.cache_disabled_reason.is_some());
            let _ = second.response_tx.send(ReviewDecision::Denied);
        });

        let first = request_exec_approval(
            &ctx,
            ExecApprovalPrompt {
                call_id: "call-main-1",
                command: "libra switch main",
                cwd: Path::new("/tmp/workspace"),
                reason: None,
                sandbox_policy: None,
                sandbox_permissions: SandboxPermissions::UseDefault,
                is_retry: false,
                requested_network_access: None,
            },
        )
        .await;
        assert_eq!(first, ReviewDecision::Approved);

        let second = request_exec_approval(
            &ctx,
            ExecApprovalPrompt {
                call_id: "call-main-2",
                command: "libra switch main",
                cwd: Path::new("/tmp/workspace"),
                reason: None,
                sandbox_policy: None,
                sandbox_permissions: SandboxPermissions::UseDefault,
                is_retry: false,
                requested_network_access: None,
            },
        )
        .await;
        assert_eq!(second, ReviewDecision::Denied);

        responder.await.expect("responder task failed");
        assert!(store.lock().await.active_memos_at(Utc::now()).is_empty());
    }

    #[test]
    fn approval_cache_policy_flags_non_allowlisted_network_domains() {
        let policy = ApprovalCachePolicy {
            protected_branches: Vec::new(),
            allowed_network_domains: vec!["github.com".to_string()],
            no_cache_unknown_network: true,
            approved_ruleset: None,
        };

        assert!(
            policy
                .disabled_reason_for_command("curl https://api.github.com/repos")
                .is_none()
        );
        assert!(
            policy
                .disabled_reason_for_command("curl example.com/path")
                .unwrap()
                .contains("example.com")
        );
    }

    #[tokio::test]
    async fn scoped_approval_does_not_inherit_interactive_session_cache() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let store = Arc::new(tokio::sync::Mutex::new(ApprovalStore::default()));
        {
            let mut guard = store.lock().await;
            guard.put(
                "shell:/tmp/workspace".to_string(),
                ReviewDecision::ApprovedForSession,
            );
            guard.approve_all_commands();
        }
        let automation_ctx = ToolApprovalContext {
            policy: AskForApproval::OnRequest,
            request_tx: tx,
            store: Arc::clone(&store),
            scope_key_prefix: Some("automation:thread-1".to_string()),
            approval_ttl: DEFAULT_APPROVAL_TTL,
            cache_policy: ApprovalCachePolicy::default(),
        };
        let keys = vec!["shell:/tmp/workspace".to_string()];

        let responder = tokio::spawn(async move {
            let request = rx
                .recv()
                .await
                .expect("automation approval request expected");
            let _ = request.response_tx.send(ReviewDecision::Denied);
        });
        let decision = request_cached_approval_with_keys(&automation_ctx, &keys, |response_tx| {
            ExecApprovalRequest {
                call_id: "call-automation".to_string(),
                command: "cargo test".to_string(),
                cwd: PathBuf::from("/tmp/workspace"),
                reason: None,
                is_retry: false,
                sandbox_label: "workspace-write".to_string(),
                network_access: NetworkAccess::Denied,
                writable_roots: vec![PathBuf::from("/tmp/workspace")],
                cache_disabled_reason: None,
                response_tx,
            }
        })
        .await;

        responder.await.expect("responder task failed");
        assert_eq!(decision, ReviewDecision::Denied);
        assert!(store.lock().await.allow_all_commands());
        assert!(
            !store
                .lock()
                .await
                .allow_all_commands_for_scope("automation:thread-1")
        );
    }

    fn test_exec_request(
        command: &str,
        cwd: &Path,
        response_tx: tokio::sync::oneshot::Sender<ReviewDecision>,
        cache_disabled_reason: Option<String>,
    ) -> ExecApprovalRequest {
        ExecApprovalRequest {
            call_id: "call-test".to_string(),
            command: command.to_string(),
            cwd: cwd.to_path_buf(),
            reason: None,
            is_retry: false,
            sandbox_label: "workspace-write".to_string(),
            network_access: NetworkAccess::Denied,
            writable_roots: vec![cwd.to_path_buf()],
            cache_disabled_reason,
            response_tx,
        }
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
            // SAFETY: this test helper is only used from tests serialized by
            // `#[serial(sandbox_env)]`, so no sibling test mutates this env var
            // concurrently.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }

        fn unset(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: this test helper is only used from tests serialized by
            // `#[serial(sandbox_env)]`, so no sibling test mutates this env var
            // concurrently.
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, previous }
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: the paired test is serialized by `#[serial(sandbox_env)]`,
            // so restoring the process env cannot race with another mutation.
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
